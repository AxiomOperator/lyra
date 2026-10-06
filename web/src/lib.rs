//! Lyra on the web (`lyra serve`): a small HTTP + WebSocket server for the
//! PWA, device pairing and Web Push. It knows nothing about chatting: the
//! app sends it JSON to broadcast ([`Hub::publish`]) and gets the devices'
//! input back as [`Inbound`] messages. Plain HTTP on a private address; TLS
//! comes from the reverse proxy in front (Zoraxy, Caddy, nginx).

pub mod devices;
pub mod uploads;
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
    /// The `lyra-node` program handed out at /download/lyra-node (default:
    /// `lyra-node` next to the running lyra).
    pub node_binary: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self { listen: "127.0.0.1:8484".into(), public_url: String::new(), notify: true, node_binary: String::new() }
    }
}

/// What the devices ask of the app.
pub enum Inbound {
    /// A chat message or a /command, for the conversation the connection
    /// shows (`conn` can be moved to another one: `/new`, `/resume`).
    Send { text: String, device: String, session: String, conn: u64, files: Vec<String> },
    /// Stop the reply being written in that conversation.
    Stop { device: String, session: String },
    /// An answer to an approval (`y`, `n`, `a`).
    Approve { id: u64, answer: String, device: String },
    /// The whole current state of a conversation ("" = the device's usual
    /// one), for a device that just connected or switched.
    Snapshot { session: String, reply: oneshot::Sender<Value> },
    /// Is the app's loop alive? (`/health`)
    Health(oneshot::Sender<Value>),
    /// A machine (`lyra node`) connected or went away.
    MachinesChanged,
    /// A machine sent its health (disks, memory, load, failed units, updates).
    MachineHealth { name: String, health: Value },
    /// A phone, browser or terminal connected or went away.
    DevicesChanged,
    /// A machine without a screen asks to pair; a paired device approves it.
    PairRequested(PairRequest),
    /// Something to say in the conversation (who approved a pairing, …).
    Note(String),
    /// A device asks for a list (sessions, devices, activity…) for a page.
    Get { what: String, arg: Value, session: String, reply: oneshot::Sender<Value> },
}

/// A headless machine asking to pair (`lyra-node pair <url>` without a code).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PairRequest {
    pub id: String,
    /// Shown on the machine too, so the right request gets approved.
    pub code: String,
    pub name: String,
    pub kind: String,
    pub hostname: String,
    pub os: String,
    pub created: chrono::DateTime<chrono::Utc>,
    #[serde(skip)]
    state: PairState,
}

#[derive(Debug, Clone, Default)]
enum PairState {
    #[default]
    Waiting,
    Approved(String),
    Denied,
}

/// How long a pairing request waits for an answer.
const PAIR_REQUEST_MINUTES: i64 = 10;

/// A machine lending lyra its tools (`lyra node`), as it introduced itself.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MachineInfo {
    /// What lyra calls it (the name it paired with).
    pub name: String,
    pub hostname: String,
    pub os: String,
    pub user: String,
    pub since: chrono::DateTime<chrono::Utc>,
    pub version: String,
    /// sha256 of its program (compared with the server's to offer updates).
    pub build: String,
    /// It can replace itself (the standalone `lyra-node`).
    pub self_update: bool,
    /// Its latest health report (`lyra_node::health::report`).
    pub health: Option<Value>,
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
    /// Where lyra's own backups are (`[backup] dir`), for downloading the latest.
    backups: Mutex<Option<std::path::PathBuf>>,
    uploads: uploads::Uploads,
    vapid: Vapid,
    subject: String,
    /// Updates: the conversation they belong to (none: everyone's), the JSON.
    out: broadcast::Sender<Arc<(Option<String>, String)>>,
    /// Device connections: which conversation each shows, and how to move it.
    conns: Mutex<HashMap<u64, ConnState>>,
    seq: AtomicU64,
    inbound: std::sync::mpsc::Sender<Inbound>,
    /// Connections and when each last said it was visible on screen.
    visible: Mutex<HashMap<u64, Option<Instant>>>,
    next_conn: AtomicU64,
    machines: Mutex<HashMap<String, MachineConn>>,
    next_call: AtomicU64,
    /// Devices connected now: connection → (device id, name).
    online: Mutex<HashMap<u64, (String, String)>>,
    requests: Mutex<Vec<PairRequest>>,
    node_binary: std::path::PathBuf,
    public_url: String,
}

/// The running server, as the app sees it.
#[derive(Clone)]
pub struct Hub {
    shared: Arc<Shared>,
    pub address: SocketAddr,
}

/// The web app, built by Vite from `web/ui` (`npm run build`), inside lyra.
static APP: include_dir::Dir<'static> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/ui/dist");

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
            backups: Mutex::new(None),
            vapid,
            subject,
            out,
            seq: AtomicU64::new(0),
            inbound,
            visible: Mutex::new(HashMap::new()),
            next_conn: AtomicU64::new(1),
            machines: Mutex::new(HashMap::new()),
            next_call: AtomicU64::new(1),
            online: Mutex::new(HashMap::new()),
            conns: Mutex::new(HashMap::new()),
            uploads: uploads::Uploads::new(&dir.parent().unwrap_or(dir).join("uploads")),
            requests: Mutex::new(Vec::new()),
            node_binary: if settings.node_binary.trim().is_empty() {
                std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("lyra-node"))).unwrap_or_default()
            } else {
                std::path::PathBuf::from(settings.node_binary.trim())
            },
            public_url: settings.public_url.trim_end_matches('/').to_string(),
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

    /// Send an update to the devices showing `session` (all of them: None).
    pub fn publish(&self, session: Option<&str>, mut msg: Value) {
        let seq = self.shared.seq.fetch_add(1, Ordering::SeqCst) + 1;
        msg["seq"] = json!(seq);
        if let Some(s) = session {
            msg["session"] = json!(s);
        }
        let _ = self.shared.out.send(Arc::new((session.map(str::to_string), msg.to_string())));
    }

    /// Move a connection to another conversation (it gets a fresh snapshot).
    pub fn attach(&self, conn: u64, session: &str) {
        if let Some(c) = self.shared.conns.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&conn) {
            c.session = session.to_string();
            let _ = c.moved.send(session.to_string());
        }
    }

    /// Conversations some device is showing now.
    pub fn attached_sessions(&self) -> Vec<String> {
        let mut all: Vec<String> = self.shared.conns.lock().unwrap_or_else(|e| e.into_inner()).values().map(|c| c.session.clone()).collect();
        all.sort();
        all.dedup();
        all
    }

    /// Devices (phones, browsers, terminals) connected now: (id, name), once each.
    pub fn online_devices(&self) -> Vec<(String, String)> {
        let mut all: Vec<(String, String)> = self.shared.online.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        all.sort();
        all.dedup();
        all
    }

    /// A file a device sent, and where it is.
    pub fn upload(&self, id: &str) -> Option<(uploads::Upload, std::path::PathBuf)> {
        self.shared.uploads.get(id)
    }

    /// Headless machines waiting for approval.
    pub fn pair_requests(&self) -> Vec<PairRequest> {
        let mut r = self.shared.requests.lock().unwrap_or_else(|e| e.into_inner());
        r.retain(|p| chrono::Utc::now() - p.created < chrono::Duration::minutes(PAIR_REQUEST_MINUTES));
        r.iter().filter(|p| matches!(p.state, PairState::Waiting)).cloned().collect()
    }

    /// Approve or deny a pairing request (by its code or id).
    pub fn answer_pair(&self, key: &str, approve: bool) -> Result<String, String> {
        let key = key.trim().to_uppercase().replace('-', "");
        let mut r = self.shared.requests.lock().unwrap_or_else(|e| e.into_inner());
        let p = r
            .iter_mut()
            .find(|p| matches!(p.state, PairState::Waiting) && (p.code == key || p.id.to_uppercase() == key))
            .ok_or_else(|| format!("no pairing request {key:?} is waiting"))?;
        if !approve {
            p.state = PairState::Denied;
            return Ok(format!("denied {} ({})", p.name, p.hostname));
        }
        let (d, token) = self.shared.devices.add(&p.name, &p.kind)?;
        p.state = PairState::Approved(token);
        Ok(format!("paired {} ({}) as a {}", d.name, p.hostname, if d.kind == "node" { "machine" } else { "device" }))
    }

    /// sha256 of the `lyra-node` program this server hands out, if it has one.
    /// Where lyra's backups are, so a paired device can download the latest.
    pub fn set_backups(&self, dir: std::path::PathBuf) {
        *self.shared.backups.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir);
    }

    pub fn node_build(&self) -> Option<String> {
        node_build(&self.shared.node_binary)
    }

    /// The web app's version: changes whenever its files do.
    pub fn app_version(&self) -> &'static str {
        app_version()
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
    Router::new()
        .route("/", get(|| app_file("index.html")))
        .route("/api/pair", post(pair))
        .route("/api/pair/request", post(pair_request))
        .route("/api/pair/request/{id}", get(pair_poll))
        .route("/download/lyra-node", get(download_node))
        .route("/download/lyra-node.sha256", get(download_node_sha))
        .route("/install.sh", get(install_script))
        .route("/api/me", get(me))
        .route("/api/vapid", get(vapid_key))
        .route("/api/push", post(set_push))
        .route("/api/test-push", post(test_push))
        .route("/api/approve", post(approve))
        .route("/api/files", post(upload).layer(axum::extract::DefaultBodyLimit::max(uploads::MAX_BYTES + 1024)))
        .route("/api/files/{id}", get(download))
        .route("/api/backups/latest", get(latest_backup))
        .route("/ws", get(ws))
        .route("/health", get(health))
        .route("/node", get(node_ws))
        .fallback(get(|uri: axum::http::Uri| app_file_owned(uri.path().trim_start_matches('/').to_string())))
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

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// A file of the web app. The page and the service worker carry the app's
/// version; the bundle's files have content hashes, so they cache for good.
async fn app_file(path: &'static str) -> Response {
    app_file_owned(path.to_string()).await
}

async fn app_file_owned(path: String) -> Response {
    let path = if path.is_empty() { "index.html".to_string() } else { path };
    let Some(file) = APP.get_file(&path) else { return error(StatusCode::NOT_FOUND, "not found") };
    let kind = content_type(&path);
    if path == "index.html" || path == "sw.js" {
        let text = String::from_utf8_lossy(file.contents()).replace("__LYRA_VERSION__", app_version());
        return ([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, "no-cache")], text).into_response();
    }
    let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "max-age=86400" };
    ([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, cache)], file.contents()).into_response()
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

/// A file from a device: the body is the file, `X-Filename` its name.
async fn upload(State(s): State<Arc<Shared>>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let d = match device(&s, &headers) {
        Ok(d) => d,
        Err(r) => return *r,
    };
    let raw = headers.get("x-filename").and_then(|v| v.to_str().ok()).unwrap_or("file");
    let name = percent_decode(raw);
    let mime = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let saved = tokio::task::spawn_blocking(move || s.uploads.save(&name, &mime, &d.name, &body)).await;
    match saved {
        Ok(Ok(up)) => Json(json!({ "id": up.id, "name": up.name, "mime": up.mime, "size": up.size })).into_response(),
        Ok(Err(e)) => error(StatusCode::PAYLOAD_TOO_LARGE, &e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

/// `%20`-style escapes (file names travel in a header).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A device or a machine (placing a file the user sent) fetches an upload.
async fn download(State(s): State<Arc<Shared>>, headers: HeaderMap, axum::extract::Path(id): axum::extract::Path<String>) -> Response {
    if s.devices.authenticate(&bearer(&headers)).is_none() {
        return error(StatusCode::UNAUTHORIZED, "not paired");
    }
    let Some((up, path)) = s.uploads.get(&id) else { return error(StatusCode::NOT_FOUND, "no such file") };
    match tokio::fs::read(&path).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, up.mime.clone())], bytes).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

/// The newest backup of lyra, for a paired phone or browser (not a machine)
/// to keep a copy off the server.
async fn latest_backup(State(s): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if let Err(r) = device(&s, &headers) {
        return *r;
    }
    let Some(dir) = s.backups.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        return error(StatusCode::NOT_FOUND, "backups aren't set up");
    };
    // `lyra-<YYYYmmdd-HHMMSS>.tar.gz`: the newest sorts last.
    let newest = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("lyra-") && n.ends_with(".tar.gz"))
        .max();
    let Some(name) = newest else { return error(StatusCode::NOT_FOUND, "no backup yet") };
    match tokio::fs::read(dir.join(&name)).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/gzip".to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
                (header::CACHE_CONTROL, "no-store".to_string()),
            ],
            bytes,
        )
            .into_response(),
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
    /// The conversation to show (empty: the one the device had open last).
    #[serde(default)]
    session: String,
}

struct ConnState {
    session: String,
    /// Tells the connection's task it was moved.
    moved: tokio::sync::mpsc::UnboundedSender<String>,
}

async fn ws(State(s): State<Arc<Shared>>, Query(q): Query<WsQuery>, upgrade: WebSocketUpgrade) -> Response {
    let Some(d) = s.devices.authenticate(&q.token).filter(|d| d.kind == "device") else { return error(StatusCode::UNAUTHORIZED, "not paired") };
    let session = if q.session.is_empty() { d.last_session.clone().unwrap_or_default() } else { q.session.clone() };
    let mut r = upgrade.on_upgrade(move |socket| connection(s, d, session, socket));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// A conversation's snapshot, with the device's details.
async fn snapshot(s: &Shared, d: &Device, session: &str) -> Option<Value> {
    let (tx, rx) = oneshot::channel();
    s.inbound.send(Inbound::Snapshot { session: session.to_string(), reply: tx }).ok()?;
    let mut snap = rx.await.ok()?;
    snap["device"] = json!({ "id": d.id, "name": d.name, "push": d.push.is_some() });
    Some(snap)
}

/// One device's live connection: a conversation's state, then its updates
/// (and everyone's); its messages go to that conversation.
async fn connection(s: Arc<Shared>, d: Device, session: String, mut socket: WebSocket) {
    let conn = s.next_conn.fetch_add(1, Ordering::SeqCst);
    let (moved_tx, mut moved) = tokio::sync::mpsc::unbounded_channel::<String>();
    s.visible.lock().unwrap_or_else(|e| e.into_inner()).insert(conn, Some(Instant::now()));
    s.online.lock().unwrap_or_else(|e| e.into_inner()).insert(conn, (d.id.clone(), d.name.clone()));
    // Subscribe before asking for the snapshot, so nothing falls in between.
    let mut updates = s.out.subscribe();
    let Some(first) = snapshot(&s, &d, &session).await else { return };
    // The app says which conversation "" turned out to be.
    let mut session = first["session_id"].as_str().unwrap_or(&session).to_string();
    s.conns.lock().unwrap_or_else(|e| e.into_inner()).insert(conn, ConnState { session: session.clone(), moved: moved_tx });
    let _ = s.devices.set_last_session(&d.id, &session);
    let _ = s.inbound.send(Inbound::DevicesChanged);
    let forget = |s: &Shared| {
        s.visible.lock().unwrap_or_else(|e| e.into_inner()).remove(&conn);
        s.online.lock().unwrap_or_else(|e| e.into_inner()).remove(&conn);
        s.conns.lock().unwrap_or_else(|e| e.into_inner()).remove(&conn);
        let _ = s.inbound.send(Inbound::DevicesChanged);
    };
    if socket.send(Message::Text(first.to_string().into())).await.is_err() {
        forget(&s);
        return;
    }
    loop {
        tokio::select! {
            to = moved.recv() => {
                let Some(to) = to else { break };
                session = to;
                let _ = s.devices.set_last_session(&d.id, &session);
                let Some(snap) = snapshot(&s, &d, &session).await else { break };
                if socket.send(Message::Text(snap.to_string().into())).await.is_err() {
                    break;
                }
            }
            update = updates.recv() => match update {
                Ok(item) => {
                    // Another conversation's update: not for this device.
                    if item.0.as_ref().is_some_and(|x| *x != session) {
                        continue;
                    }
                    if socket.send(Message::Text(item.1.as_str().into())).await.is_err() {
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
                        let files: Vec<String> = v["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str().map(str::to_string)).take(10).collect();
                        if !text.is_empty() || !files.is_empty() {
                            let _ = s.inbound.send(Inbound::Send { text, device: d.name.clone(), session: session.clone(), conn, files });
                        }
                    }
                    "stop" => {
                        let _ = s.inbound.send(Inbound::Stop { device: d.name.clone(), session: session.clone() });
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
                    "pair_answer" => {
                        let hub = Hub { shared: s.clone(), address: "0.0.0.0:0".parse().expect("address") };
                        let key = v["id"].as_str().or(v["code"].as_str()).unwrap_or("").to_string();
                        let text = match hub.answer_pair(&key, v["approve"] == true) {
                            Ok(t) => t,
                            Err(e) => e,
                        };
                        let _ = s.inbound.send(Inbound::Note(format!("{} (from {})", text, d.name)));
                    }
                    "get" => {
                        let what = v["what"].as_str().unwrap_or("").to_string();
                        let (tx, rx) = oneshot::channel();
                        let ask = Inbound::Get { what: what.clone(), arg: v["arg"].clone(), session: session.clone(), reply: tx };
                        if s.inbound.send(ask).is_ok()
                            && let Ok(data) = rx.await
                        {
                            let msg = json!({ "type": "data", "what": what, "arg": v["arg"], "id": v["id"], "data": data });
                            if socket.send(Message::Text(msg.to_string().into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    forget(&s);
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
    let info = MachineInfo {
        name: d.name.clone(),
        hostname: field("hostname"),
        os: field("os"),
        user: field("user"),
        since: chrono::Utc::now(),
        version: field("version"),
        build: hello["build"].as_str().unwrap_or("").chars().take(64).collect(),
        self_update: hello["self_update"] == true,
        health: None,
    };
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
                    "health" if v["health"].is_object() => {
                        if let Some(m) = s.machines.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&d.name.to_lowercase()).filter(|m| m.conn == conn) {
                            m.info.health = Some(v["health"].clone());
                        }
                        let _ = s.inbound.send(Inbound::MachineHealth { name: d.name.clone(), health: v["health"].clone() });
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

// ---- headless pairing: the machine asks, a paired device approves

#[derive(Deserialize)]
struct PairRequestBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    os: String,
}

/// Waiting requests at once (anyone who can reach lyra can ask; only a
/// paired device can approve, and it sees the code the machine shows).
const MAX_PAIR_REQUESTS: usize = 5;

async fn pair_request(State(s): State<Arc<Shared>>, Json(b): Json<PairRequestBody>) -> Response {
    let kind = if b.kind.is_empty() { "node".to_string() } else { b.kind.clone() };
    if !matches!(kind.as_str(), "node" | "device") {
        return error(StatusCode::BAD_REQUEST, "kind is node or device");
    }
    let clip = |t: &str| t.trim().chars().take(60).collect::<String>();
    let name = clip(&b.name);
    if name.is_empty() {
        return error(StatusCode::BAD_REQUEST, "a name is needed (--name)");
    }
    let request = {
        let mut r = s.requests.lock().unwrap_or_else(|e| e.into_inner());
        r.retain(|p| chrono::Utc::now() - p.created < chrono::Duration::minutes(PAIR_REQUEST_MINUTES));
        if r.iter().filter(|p| matches!(p.state, PairState::Waiting)).count() >= MAX_PAIR_REQUESTS {
            return error(StatusCode::TOO_MANY_REQUESTS, "too many pairing requests are waiting; approve or deny them first");
        }
        let p = PairRequest {
            id: devices::random(16, b"abcdefghijkmnpqrstuvwxyz23456789"),
            code: devices::random(4, b"ABCDEFGHJKMNPQRSTUVWXYZ23456789"),
            name,
            kind,
            hostname: clip(&b.hostname),
            os: clip(&b.os),
            created: chrono::Utc::now(),
            state: PairState::Waiting,
        };
        r.push(p.clone());
        p
    };
    let _ = s.inbound.send(Inbound::PairRequested(request.clone()));
    Json(json!({ "id": request.id, "code": request.code, "expires_in": PAIR_REQUEST_MINUTES * 60 })).into_response()
}

/// The waiting machine polls this; the token is handed over once.
async fn pair_poll(State(s): State<Arc<Shared>>, axum::extract::Path(id): axum::extract::Path<String>) -> Response {
    let mut r = s.requests.lock().unwrap_or_else(|e| e.into_inner());
    let Some(i) = r.iter().position(|p| p.id == id) else { return Json(json!({ "state": "unknown" })).into_response() };
    if chrono::Utc::now() - r[i].created >= chrono::Duration::minutes(PAIR_REQUEST_MINUTES) {
        r.remove(i);
        return Json(json!({ "state": "expired" })).into_response();
    }
    match r[i].state.clone() {
        PairState::Waiting => Json(json!({ "state": "waiting" })).into_response(),
        PairState::Denied => {
            r.remove(i);
            Json(json!({ "state": "denied" })).into_response()
        }
        PairState::Approved(token) => {
            r.remove(i);
            Json(json!({ "state": "approved", "token": token })).into_response()
        }
    }
}

// ---- handing out lyra-node

fn node_build(path: &Path) -> Option<String> {
    use sha2::Digest;
    let bytes = std::fs::read(path).ok()?;
    Some(sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect())
}

async fn download_node(State(s): State<Arc<Shared>>) -> Response {
    match tokio::fs::read(&s.node_binary).await {
        Ok(bytes) => (
            [(header::CONTENT_TYPE, "application/octet-stream"), (header::CONTENT_DISPOSITION, "attachment; filename=\"lyra-node\"")],
            bytes,
        )
            .into_response(),
        Err(_) => error(StatusCode::NOT_FOUND, &format!("this server has no lyra-node to hand out ({})", s.node_binary.display())),
    }
}

async fn download_node_sha(State(s): State<Arc<Shared>>) -> Response {
    let path = s.node_binary.clone();
    match tokio::task::spawn_blocking(move || node_build(&path)).await.ok().flatten() {
        Some(sum) => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], format!("{sum}  lyra-node\n")).into_response(),
        None => error(StatusCode::NOT_FOUND, "this server has no lyra-node to hand out"),
    }
}

const INSTALL_SH: &str = include_str!("../assets/install.sh");

/// `curl -fsSL https://lyra…/install.sh | sh -s -- --name web1`
async fn install_script(State(s): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    let url = if s.public_url.is_empty() {
        let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost");
        let proto = headers.get("x-forwarded-proto").and_then(|h| h.to_str().ok()).unwrap_or("http");
        format!("{proto}://{host}")
    } else {
        s.public_url.clone()
    };
    ([(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")], INSTALL_SH.replace("__LYRA_URL__", &url)).into_response()
}

// ---- the web app's version

fn app_version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| {
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        let mut files: Vec<&include_dir::File> = Vec::new();
        fn walk<'a>(dir: &'a include_dir::Dir<'a>, out: &mut Vec<&'a include_dir::File<'a>>) {
            out.extend(dir.files());
            for d in dir.dirs() {
                walk(d, out);
            }
        }
        walk(&APP, &mut files);
        files.sort_by_key(|f| f.path());
        for f in files {
            h.update(f.path().to_string_lossy().as_bytes());
            h.update(f.contents());
        }
        h.finalize().iter().take(6).map(|b| format!("{b:02x}")).collect()
    })
}
