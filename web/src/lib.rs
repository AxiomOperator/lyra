//! Lyra on the web (`lyra serve`): a small HTTP + WebSocket server for the
//! PWA, device pairing and Web Push. It knows nothing about chatting: the
//! app sends it JSON to broadcast ([`Hub::publish`]) and gets the devices'
//! input back as [`Inbound`] messages. Plain HTTP on a private address; TLS
//! comes from the reverse proxy in front (Zoraxy, Caddy, nginx).

pub mod devices;
pub mod push;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};

pub use devices::{Device, Devices};
pub use push::{Subscription, Vapid};

/// `[web]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Where `lyra serve` listens (plain HTTP; put a TLS proxy in front).
    pub listen: String,
    /// The HTTPS address you open on the phone (`https://lyra.example.com`).
    pub public_url: String,
    /// Push notifications when no device has lyra open.
    pub notify: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { listen: "127.0.0.1:8484".into(), public_url: String::new(), notify: true }
    }
}

/// What the devices ask of the app.
pub enum Inbound {
    /// A chat message or a /command.
    Send { text: String, device: String },
    /// An answer to an approval (`y`, `n`, `a`).
    Approve { id: u64, answer: String, device: String },
    /// The whole current state, for a device that just connected.
    Snapshot(oneshot::Sender<Value>),
    /// Is the app's loop alive? (`/health`)
    Health(oneshot::Sender<Value>),
    /// A machine (`lyra node`) connected or went away.
    MachinesChanged,
}

/// A machine lending lyra its tools (`lyra node`), as it introduced itself.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MachineInfo {
    /// What lyra calls it (the name it paired with).
    pub name: String,
    pub hostname: String,
    pub os: String,
    pub user: String,
    pub since: chrono::DateTime<chrono::Utc>,
}

type Pending = Arc<Mutex<HashMap<u64, std::sync::mpsc::Sender<Result<Value, String>>>>>;

struct MachineConn {
    info: MachineInfo,
    /// This connection (a reconnect replaces it; only the newest may remove itself).
    conn: u64,
    to_node: tokio::sync::mpsc::UnboundedSender<String>,
    pending: Pending,
}

/// A notification for every device with push turned on.
#[derive(Debug, Clone)]
pub struct Notification {
    pub title: String,
    pub body: String,
    /// Replaces an earlier notification with the same tag.
    pub tag: String,
    /// An approval it asks about (Android shows Allow / Deny buttons).
    pub approval: Option<u64>,
}

struct Shared {
    devices: Devices,
    vapid: Vapid,
    subject: String,
    out: broadcast::Sender<Arc<String>>,
    seq: AtomicU64,
    inbound: std::sync::mpsc::Sender<Inbound>,
    /// Connections and when each last said it was visible on screen.
    visible: Mutex<HashMap<u64, Option<Instant>>>,
    next_conn: AtomicU64,
    machines: Mutex<HashMap<String, MachineConn>>,
    next_call: AtomicU64,
}

/// The running server, as the app sees it.
#[derive(Clone)]
pub struct Hub {
    shared: Arc<Shared>,
    pub address: SocketAddr,
}

const INDEX: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const SW_JS: &str = include_str!("../assets/sw.js");
const STYLE: &str = include_str!("../assets/style.css");
const MANIFEST: &str = include_str!("../assets/manifest.webmanifest");
const ICON_192: &[u8] = include_bytes!("../assets/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../assets/icon-512.png");
const ICON_180: &[u8] = include_bytes!("../assets/apple-touch-icon.png");

/// The VAPID key, made on first use and kept in `dir/vapid.key` (0600).
fn vapid(dir: &Path) -> Result<Vapid, String> {
    let path = dir.join("vapid.key");
    if let Ok(text) = std::fs::read_to_string(&path) {
        return Vapid::from_base64(&text);
    }
    let v = Vapid::generate();
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).map_err(|e| e.to_string())?;
    f.write_all(v.private_base64().as_bytes()).map_err(|e| e.to_string())?;
    Ok(v)
}

impl Hub {
    /// Start serving on the given runtime. `dir` holds devices and keys
    /// (`~/.lyra/web`).
    pub fn start(rt: &tokio::runtime::Handle, settings: &Settings, dir: &Path, inbound: std::sync::mpsc::Sender<Inbound>) -> Result<Hub, String> {
        let devices = Devices::open(dir)?;
        let vapid = vapid(dir)?;
        let subject = if settings.public_url.starts_with("https://") { settings.public_url.trim_end_matches('/').to_string() } else { "mailto:lyra@example.com".into() };
        let (out, _) = broadcast::channel(4096);
        let shared = Arc::new(Shared {
            devices,
            vapid,
            subject,
            out,
            seq: AtomicU64::new(0),
            inbound,
            visible: Mutex::new(HashMap::new()),
            next_conn: AtomicU64::new(1),
            machines: Mutex::new(HashMap::new()),
            next_call: AtomicU64::new(1),
        });
        let addr: SocketAddr = settings.listen.parse().map_err(|e| format!("[web] listen {:?}: {e}", settings.listen))?;
        let listener = rt.block_on(tokio::net::TcpListener::bind(addr)).map_err(|e| format!("can't listen on {addr}: {e}"))?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        let app = router(shared.clone());
        rt.spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Hub { shared, address })
    }

    pub fn devices(&self) -> &Devices {
        &self.shared.devices
    }

    /// The sequence number of the last published message (snapshots carry it).
    pub fn seq(&self) -> u64 {
        self.shared.seq.load(Ordering::SeqCst)
    }

    /// Send a message to every connected device.
    pub fn publish(&self, mut msg: Value) {
        let seq = self.shared.seq.fetch_add(1, Ordering::SeqCst) + 1;
        msg["seq"] = json!(seq);
        let _ = self.shared.out.send(Arc::new(msg.to_string()));
    }

    /// The machines connected right now (`lyra node`), by name.
    pub fn machines(&self) -> Vec<MachineInfo> {
        let mut all: Vec<MachineInfo> = self.shared.machines.lock().unwrap_or_else(|e| e.into_inner()).values().map(|m| m.info.clone()).collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        all
    }

    /// Ask a machine to do something and wait for its answer (blocking: call
    /// from a worker thread, never from the async runtime).
    pub fn call_machine(&self, name: &str, mut request: Value, timeout: std::time::Duration) -> Result<Value, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let id = self.shared.next_call.fetch_add(1, Ordering::SeqCst);
        let pending = {
            let machines = self.shared.machines.lock().unwrap_or_else(|e| e.into_inner());
            let m = machines
                .values()
                .find(|m| m.info.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| format!("{name} isn't connected (is `lyra node` running on it?)"))?;
            m.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
            request["id"] = json!(id);
            m.to_node.send(request.to_string()).map_err(|_| format!("{name} just disconnected"))?;
            m.pending.clone()
        };
        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => {
                pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                Err(format!("{name} didn't answer within {}s", timeout.as_secs()))
            }
        }
    }

    pub fn connections(&self) -> usize {
        self.shared.visible.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether someone is looking at lyra right now (a device has it open
    /// and on screen): then there's no need to notify.
    pub fn someone_watching(&self) -> bool {
        self.shared.visible.lock().unwrap_or_else(|e| e.into_inner()).values().any(|v| v.is_some_and(|t| t.elapsed().as_secs() < 90))
    }

    /// Push a notification to every device that allowed them (in the
    /// background). Subscriptions the push service says are gone are dropped.
    pub fn notify(&self, n: Notification) {
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let payload = json!({ "title": n.title, "body": n.body, "tag": n.tag, "approval": n.approval }).to_string();
            for d in shared.devices.list() {
                let Some(sub) = &d.push else { continue };
                if push::send(&shared.vapid, &shared.subject, sub, payload.as_bytes()) == push::Sent::Gone {
                    let _ = shared.devices.set_push(&d.id, None);
                }
            }
        });
    }
}

fn router(shared: Arc<Shared>) -> Router {
    let file = |body: &'static str, kind: &'static str| {
        move || async move { ([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, "no-cache")], body) }
    };
    let image = |body: &'static [u8]| move || async move { ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "max-age=86400")], body) };
    Router::new()
        .route("/", get(file(INDEX, "text/html; charset=utf-8")))
        .route("/app.js", get(file(APP_JS, "text/javascript; charset=utf-8")))
        .route("/style.css", get(file(STYLE, "text/css; charset=utf-8")))
        .route("/manifest.webmanifest", get(file(MANIFEST, "application/manifest+json")))
        .route("/sw.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], SW_JS) }))
        .route("/icon-192.png", get(image(ICON_192)))
        .route("/icon-512.png", get(image(ICON_512)))
        .route("/apple-touch-icon.png", get(image(ICON_180)))
        .route("/api/pair", post(pair))
        .route("/api/me", get(me))
        .route("/api/vapid", get(vapid_key))
        .route("/api/push", post(set_push))
        .route("/api/test-push", post(test_push))
        .route("/api/approve", post(approve))
        .route("/ws", get(ws))
        .route("/health", get(health))
        .route("/node", get(node_ws))
        .with_state(shared)
}

/// For uptime monitors: 200 when the server and lyra's main loop both
/// answer, 503 when the loop is stuck or gone. Says nothing private.
async fn health(State(s): State<Arc<Shared>>) -> Response {
    let (tx, rx) = oneshot::channel();
    let alive = s.inbound.send(Inbound::Health(tx)).is_ok();
    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(mut v)) if alive => {
            v["ok"] = json!(true);
            v["connections"] = json!(s.visible.lock().unwrap_or_else(|e| e.into_inner()).len());
            (StatusCode::OK, [(header::CACHE_CONTROL, "no-store")], Json(v)).into_response()
        }
        _ => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "ok": false, "error": "lyra's main loop isn't answering" }))).into_response(),
    }
}

fn error(status: StatusCode, text: &str) -> Response {
    (status, Json(json!({ "error": text }))).into_response()
}

fn bearer(headers: &HeaderMap) -> String {
    headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("").to_string()
}

/// A phone or browser (not a machine's node token).
fn device(shared: &Shared, headers: &HeaderMap) -> Result<Device, Box<Response>> {
    shared
        .devices
        .authenticate(&bearer(headers))
        .filter(|d| d.kind == "device")
        .ok_or_else(|| Box::new(error(StatusCode::UNAUTHORIZED, "not paired: pair this device again")))
}

#[derive(Deserialize)]
struct PairBody {
    code: String,
    #[serde(default)]
    name: String,
    /// `device` (default) or `node`.
    #[serde(default)]
    kind: String,
}

async fn pair(State(s): State<Arc<Shared>>, Json(b): Json<PairBody>) -> Response {
    // Slow guessing down a little more than the attempt limit does.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let kind = if b.kind.is_empty() { "device" } else { b.kind.as_str() };
    match s.devices.pair(&b.code, &b.name, kind) {
        Ok((d, token)) => Json(json!({ "token": token, "device": { "id": d.id, "name": d.name, "kind": d.kind } })).into_response(),
        Err(e) => error(StatusCode::FORBIDDEN, &e),
    }
}

async fn me(State(s): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    match device(&s, &headers) {
        Ok(d) => Json(json!({ "id": d.id, "name": d.name, "push": d.push.is_some() })).into_response(),
        Err(r) => *r,
    }
}

async fn vapid_key(State(s): State<Arc<Shared>>) -> Response {
    Json(json!({ "key": s.vapid.public_base64() })).into_response()
}

async fn set_push(State(s): State<Arc<Shared>>, headers: HeaderMap, Json(b): Json<Value>) -> Response {
    let d = match device(&s, &headers) {
        Ok(d) => d,
        Err(r) => return *r,
    };
    let sub: Option<Subscription> = match b.get("subscription") {
        None | Some(Value::Null) => None,
        Some(v) => match serde_json::from_value(v.clone()) {
            Ok(sub) => Some(sub),
            Err(e) => return error(StatusCode::BAD_REQUEST, &format!("bad subscription: {e}")),
        },
    };
    match s.devices.set_push(&d.id, sub) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

async fn test_push(State(s): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    let d = match device(&s, &headers) {
        Ok(d) => d,
        Err(r) => return *r,
    };
    let Some(sub) = d.push.clone() else { return error(StatusCode::BAD_REQUEST, "notifications aren't on for this device") };
    let payload = json!({ "title": "lyra", "body": format!("Notifications work on {}.", d.name), "tag": "test" }).to_string();
    let sent = tokio::task::spawn_blocking(move || push::send(&s.vapid, &s.subject, &sub, payload.as_bytes())).await;
    match sent {
        Ok(push::Sent::Ok) => Json(json!({ "ok": true })).into_response(),
        Ok(push::Sent::Gone) => error(StatusCode::GONE, "the push service says this subscription is gone; turn notifications off and on"),
        Ok(push::Sent::Failed(e)) => error(StatusCode::BAD_GATEWAY, &e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(Deserialize)]
struct ApproveBody {
    id: u64,
    answer: String,
}

/// From a notification's Allow / Deny button (the service worker).
async fn approve(State(s): State<Arc<Shared>>, headers: HeaderMap, Json(b): Json<ApproveBody>) -> Response {
    let d = match device(&s, &headers) {
        Ok(d) => d,
        Err(r) => return *r,
    };
    let _ = s.inbound.send(Inbound::Approve { id: b.id, answer: b.answer, device: d.name });
    Json(json!({ "ok": true })).into_response()
}

#[derive(Deserialize)]
struct WsQuery {
    #[serde(default)]
    token: String,
}

async fn ws(State(s): State<Arc<Shared>>, Query(q): Query<WsQuery>, upgrade: WebSocketUpgrade) -> Response {
    let Some(d) = s.devices.authenticate(&q.token).filter(|d| d.kind == "device") else { return error(StatusCode::UNAUTHORIZED, "not paired") };
    let mut r = upgrade.on_upgrade(move |socket| connection(s, d, socket));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// One device's live connection: the current state, then every update; its
/// messages go to the app.
async fn connection(s: Arc<Shared>, d: Device, mut socket: WebSocket) {
    let conn = s.next_conn.fetch_add(1, Ordering::SeqCst);
    s.visible.lock().unwrap_or_else(|e| e.into_inner()).insert(conn, Some(Instant::now()));
    // Subscribe before asking for the snapshot, so nothing falls in between.
    let mut updates = s.out.subscribe();
    let (tx, rx) = oneshot::channel();
    if s.inbound.send(Inbound::Snapshot(tx)).is_err() {
        return;
    }
    let Ok(snapshot) = rx.await else { return };
    let mut snapshot = snapshot;
    snapshot["device"] = json!({ "id": d.id, "name": d.name, "push": d.push.is_some() });
    if socket.send(Message::Text(snapshot.to_string().into())).await.is_err() {
        s.visible.lock().unwrap_or_else(|e| e.into_inner()).remove(&conn);
        return;
    }
    loop {
        tokio::select! {
            update = updates.recv() => match update {
                Ok(text) => {
                    if socket.send(Message::Text(text.as_str().into())).await.is_err() {
                        break;
                    }
                }
                // Too far behind: start over from a fresh snapshot.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = socket.send(Message::Text(json!({ "type": "resync" }).to_string().into())).await;
                    break;
                }
                Err(_) => break,
            },
            incoming = socket.recv() => {
                let Some(Ok(msg)) = incoming else { break };
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                match v["type"].as_str().unwrap_or("") {
                    "send" => {
                        let text = v["text"].as_str().unwrap_or("").trim().to_string();
                        if !text.is_empty() {
                            let _ = s.inbound.send(Inbound::Send { text, device: d.name.clone() });
                        }
                    }
                    "approve" => {
                        let _ = s.inbound.send(Inbound::Approve {
                            id: v["id"].as_u64().unwrap_or(0),
                            answer: v["answer"].as_str().unwrap_or("").to_string(),
                            device: d.name.clone(),
                        });
                    }
                    "visible" => {
                        let at = (v["visible"] == true).then(Instant::now);
                        s.visible.lock().unwrap_or_else(|e| e.into_inner()).insert(conn, at);
                    }
                    "ping" => {
                        let _ = socket.send(Message::Text(json!({ "type": "pong" }).to_string().into())).await;
                    }
                    _ => {}
                }
            }
        }
    }
    s.visible.lock().unwrap_or_else(|e| e.into_inner()).remove(&conn);
}

// ---- machines (`lyra node`): a machine connects out and runs lyra's
// system tools there. Calls go out as `{id, type: check|call, …}`; answers
// come back as `{type: result, id, ok, value | error}`.

async fn node_ws(State(s): State<Arc<Shared>>, Query(q): Query<WsQuery>, upgrade: WebSocketUpgrade) -> Response {
    let Some(d) = s.devices.authenticate(&q.token).filter(|d| d.kind == "node") else { return error(StatusCode::UNAUTHORIZED, "not a paired machine") };
    upgrade.on_upgrade(move |socket| node_connection(s, d, socket))
}

/// No message (a ping at least) for this long: the machine is gone.
const NODE_SILENCE: std::time::Duration = std::time::Duration::from_secs(90);

async fn node_connection(s: Arc<Shared>, d: Device, mut socket: WebSocket) {
    // The node introduces itself first.
    let hello = match tokio::time::timeout(std::time::Duration::from_secs(15), socket.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<Value>(t.as_str()).unwrap_or(json!({})),
        _ => return,
    };
    if hello["type"] != "hello" {
        return;
    }
    let field = |k: &str| hello[k].as_str().unwrap_or("").chars().take(80).collect::<String>();
    let info = MachineInfo { name: d.name.clone(), hostname: field("hostname"), os: field("os"), user: field("user"), since: chrono::Utc::now() };
    let conn = s.next_conn.fetch_add(1, Ordering::SeqCst);
    let (to_node, mut outgoing) = tokio::sync::mpsc::unbounded_channel::<String>();
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let replaced = s.machines.lock().unwrap_or_else(|e| e.into_inner()).insert(d.name.to_lowercase(), MachineConn { info, conn, to_node, pending: pending.clone() });
    if let Some(old) = replaced {
        fail_pending(&old.pending, &format!("{} reconnected", d.name));
    }
    let _ = s.inbound.send(Inbound::MachinesChanged);
    let _ = socket.send(Message::Text(json!({ "type": "welcome", "machine": d.name }).to_string().into())).await;
    loop {
        tokio::select! {
            out = outgoing.recv() => match out {
                Some(text) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            incoming = tokio::time::timeout(NODE_SILENCE, socket.recv()) => {
                let Ok(Some(Ok(msg))) = incoming else { break };
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                match v["type"].as_str().unwrap_or("") {
                    "result" => {
                        let id = v["id"].as_u64().unwrap_or(0);
                        if let Some(tx) = pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
                            let result = if v["ok"] == true { Ok(v["value"].clone()) } else { Err(v["error"].as_str().unwrap_or("failed").to_string()) };
                            let _ = tx.send(result);
                        }
                    }
                    "ping" => {
                        let _ = socket.send(Message::Text(json!({ "type": "pong" }).to_string().into())).await;
                    }
                    _ => {}
                }
            }
        }
    }
    let mut machines = s.machines.lock().unwrap_or_else(|e| e.into_inner());
    if machines.get(&d.name.to_lowercase()).is_some_and(|m| m.conn == conn) {
        machines.remove(&d.name.to_lowercase());
        drop(machines);
        let _ = s.inbound.send(Inbound::MachinesChanged);
    }
    fail_pending(&pending, &format!("{} disconnected", d.name));
}

fn fail_pending(pending: &Pending, why: &str) {
    for (_, tx) in pending.lock().unwrap_or_else(|e| e.into_inner()).drain() {
        let _ = tx.send(Err(why.to_string()));
    }
}
