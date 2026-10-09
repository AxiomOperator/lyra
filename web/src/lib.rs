//! Lyra on the web (`lyra serve`): a small HTTP + WebSocket server for the
//! PWA, device pairing and Web Push. It knows nothing about chatting: the
//! app sends it JSON to broadcast ([`Hub::publish`]) and gets the devices'
//! input back as [`Inbound`] messages. Plain HTTP on a private address; TLS
//! comes from the reverse proxy in front (Zoraxy, Caddy, nginx).

pub mod devices;
pub mod oidc;
pub mod users;
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
pub use users::{Role, Status, User, Users, Who};
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
    /// `[web.entra]`: signing in with Microsoft.
    pub entra: oidc::Entra,
}

impl Default for Settings {
    fn default() -> Self {
        Self { listen: "127.0.0.1:8484".into(), public_url: String::new(), notify: true, node_binary: String::new(), entra: oidc::Entra::default() }
    }
}

/// What the devices ask of the app.
pub enum Inbound {
    /// A chat message or a /command, for the conversation the connection
    /// shows (`conn` can be moved to another one: `/new`, `/resume`).
    Send { text: String, device: String, who: Who, session: String, conn: u64, files: Vec<String> },
    /// Stop the reply being written in that conversation.
    Stop { device: String, who: Who, session: String },
    /// An answer to an approval (`y`, `n`, `a`).
    Approve { id: u64, answer: String, device: String, who: Who },
    /// A notification's button (`action`) about `reference` (a PMI task).
    Action { action: String, reference: String, device: String, who: Who },
    /// The whole current state of a conversation ("" = the device's usual
    /// one), for a device that just connected or switched.
    Snapshot { session: String, who: Who, reply: oneshot::Sender<Value> },
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
    /// Someone new signed in with Microsoft and waits for an admin.
    SignIn { name: String, email: String },
    /// Someone connected their Microsoft account to a service (their
    /// calendar): its refresh token, to keep with their secrets.
    Connected { user: String, service: String, token: String, scope: String },
    /// A device asks for a list (sessions, devices, activity…) for a page.
    Get { what: String, arg: Value, session: String, who: Who, reply: oneshot::Sender<Value> },
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
    /// Coding harnesses it has and their versions ({"claude": "2.1.291", …}).
    pub harnesses: Value,
}

type Pending = Arc<Mutex<HashMap<u64, std::sync::mpsc::Sender<Result<Value, String>>>>>;

struct MachineConn {
    info: MachineInfo,
    /// Progress for long calls (coding jobs), by request id.
    progress: Arc<Mutex<HashMap<u64, std::sync::mpsc::Sender<Value>>>>,
    /// This connection (a reconnect replaces it; only the newest may remove itself).
    conn: u64,
    to_node: tokio::sync::mpsc::UnboundedSender<String>,
    pending: Pending,
}

/// A folder a person's open page lends lyra (File System Access, in their
/// browser): only that person's requests ever reach it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, Deserialize)]
pub struct Folder {
    pub name: String,
    /// They allowed changes (readwrite), not just reading.
    #[serde(default)]
    pub writable: bool,
    /// The browser still has their OK (after a restart it may ask again).
    #[serde(default)]
    pub allowed: bool,
    /// They said lyra may change files here without asking each time.
    #[serde(default)]
    pub trusted: bool,
}

/// An open page and the folders it lends.
struct Browser {
    user: String,
    device: String,
    to_page: tokio::sync::mpsc::UnboundedSender<String>,
    folders: Vec<Folder>,
    pending: Pending,
}

/// Which of a person's open pages has `folder` (allowed ones first, then the
/// one seen on screen most recently). `pages`: (connection, user, folders,
/// last visible).
/// The page a folder request goes to: `user`'s own, with the folder allowed,
/// the one seen most recently. `trusted`: only a page where that folder is
/// trusted (a write going ahead without asking, I-11).
fn pick_page(pages: &[(u64, &str, &[Folder], Option<Instant>)], user: &str, folder: &str, trusted: bool) -> Result<u64, String> {
    let theirs: Vec<_> = pages.iter().filter(|p| p.1 == user && p.2.iter().any(|f| f.name.eq_ignore_ascii_case(folder) && (!trusted || f.trusted))).collect();
    if theirs.is_empty() {
        return Err(format!("no open lyra page has the folder {folder:?}: open lyra (Projects) on the PC that has it"));
    }
    let allowed = |p: &&&(u64, &str, &[Folder], Option<Instant>)| p.2.iter().any(|f| f.name.eq_ignore_ascii_case(folder) && f.allowed);
    theirs
        .iter()
        .filter(allowed)
        .max_by_key(|p| p.3)
        .map(|p| p.0)
        .ok_or_else(|| format!("lyra needs your OK for {folder:?} again: open Projects in lyra and tap Allow"))
}

/// Who a notification is for.
#[derive(Debug, Clone, PartialEq)]
pub enum To {
    Everyone,
    /// Admins' devices (the server's health, machines, pairing, sign-ins).
    Admins,
    /// One user's devices (their replies, approvals, reminders).
    User(String),
}

/// A notification for the devices (of `to`) with push turned on.
#[derive(Debug, Clone)]
pub struct Notification {
    pub title: String,
    pub body: String,
    /// Replaces an earlier notification with the same tag.
    pub tag: String,
    /// An approval it asks about (Android shows Allow / Deny buttons).
    pub approval: Option<u64>,
    /// Other buttons (id, label), sent back with `reference` to `/api/action`.
    pub actions: Vec<(String, String)>,
    pub reference: Option<String>,
    pub to: To,
    /// The app page a tap opens ("/?page=status"); the chat when none.
    pub url: Option<String>,
}

struct Shared {
    devices: Devices,
    users: Users,
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
    /// Connections: whose, and when each last said it was visible on screen.
    visible: Mutex<HashMap<u64, (String, Option<Instant>)>>,
    next_conn: AtomicU64,
    /// Pushes sent and failed since the last look (lyra's Status), and the last error.
    push_sent: AtomicU64,
    push_failed: AtomicU64,
    push_error: Mutex<String>,
    machines: Mutex<HashMap<String, MachineConn>>,
    /// Open pages lending folders, by connection.
    browsers: Mutex<HashMap<u64, Browser>>,
    next_call: AtomicU64,
    /// Devices connected now: connection → (device id, name).
    online: Mutex<HashMap<u64, (String, String)>>,
    requests: Mutex<Vec<PairRequest>>,
    node_binary: std::path::PathBuf,
    public_url: String,
    entra: oidc::Entra,
    /// Microsoft sign-ins on their way, by state.
    logins: Mutex<HashMap<String, oidc::Pending>>,
    /// Finished sign-ins waiting for their browser: code → (device token, the sign-in's state, when).
    handoffs: Mutex<HashMap<String, (String, String, Instant)>>,
    /// Wrong passwords by username: (how many, since when), to slow guessing.
    tries: Mutex<HashMap<String, (u32, Instant)>>,
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
        // The first user is the owner (an admin); devices from before users are theirs.
        let users = Users::open(dir);
        users.ensure_owner("Owner")?;
        devices.adopt(users::OWNER)?;
        let vapid = vapid(dir)?;
        let subject = if settings.public_url.starts_with("https://") { settings.public_url.trim_end_matches('/').to_string() } else { "mailto:lyra@example.com".into() };
        let (out, _) = broadcast::channel(4096);
        let shared = Arc::new(Shared {
            devices,
            users,
            backups: Mutex::new(None),
            vapid,
            subject,
            out,
            seq: AtomicU64::new(0),
            inbound,
            visible: Mutex::new(HashMap::new()),
            next_conn: AtomicU64::new(1),
            push_sent: AtomicU64::new(0),
            push_failed: AtomicU64::new(0),
            push_error: Mutex::new(String::new()),
            machines: Mutex::new(HashMap::new()),
            browsers: Mutex::new(HashMap::new()),
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
            entra: settings.entra.clone(),
            logins: Mutex::new(HashMap::new()),
            handoffs: Mutex::new(HashMap::new()),
            tries: Mutex::new(HashMap::new()),
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

    pub fn users(&self) -> &Users {
        &self.shared.users
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

    /// Approve or deny a pairing request (by its code or id). A device (a
    /// terminal) becomes `user`'s; a machine is nobody's.
    pub fn answer_pair(&self, key: &str, approve: bool, user: Option<&str>) -> Result<String, String> {
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
        // A terminal acts as someone: say whose, never by default.
        let owner = match (p.kind.as_str(), user) {
            ("node", _) => None,
            (_, Some(u)) => Some(self.shared.users.find(u).filter(|u| u.status == users::Status::Active).ok_or_else(|| format!("no active user {u:?}"))?.id),
            (_, None) => return Err(format!("{} is a terminal: say whose it is (/devices approve {} for <name or email>)", p.name, p.code)),
        };
        let (d, token) = self.shared.devices.add(&p.name, &p.kind, owner.as_deref())?;
        p.state = PairState::Approved(token);
        Ok(format!("paired {} ({}) as a {}", d.name, p.hostname, if d.kind == "node" { "machine" } else { "device" }))
    }

    /// sha256 of the `lyra-node` program this server hands out, if it has one.
    /// The address devices use (`[web] public_url`), "" when not set.
    pub fn public_url(&self) -> String {
        self.shared.public_url.clone()
    }

    /// Pushes (sent, failed) since the last call, and the last failure.
    pub fn take_push_counts(&self) -> (u64, u64, String) {
        let sent = self.shared.push_sent.swap(0, Ordering::Relaxed);
        let failed = self.shared.push_failed.swap(0, Ordering::Relaxed);
        let error = if failed > 0 { self.shared.push_error.lock().unwrap_or_else(|e| e.into_inner()).clone() } else { String::new() };
        (sent, failed, error)
    }

    /// Devices with notifications on.
    pub fn push_devices(&self) -> usize {
        self.shared.devices.list().iter().filter(|d| d.push.is_some()).count()
    }

    /// Where lyra's backups are, so a paired device can download the latest.
    pub fn set_backups(&self, dir: std::path::PathBuf) {
        *self.shared.backups.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir);
    }

    pub fn node_build(&self) -> Option<String> {
        node_build(&self.shared.node_binary)
    }

    /// The Windows lyra-node's build (`lyra-node.exe` next to the Linux one), read once.
    pub fn node_build_windows(&self) -> Option<String> {
        static BUILD: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        BUILD.get_or_init(|| node_build(&self.shared.node_binary.with_extension("exe"))).clone()
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

    /// The folders `user`'s open pages lend, with the device each is on.
    pub fn folders(&self, user: &str) -> Vec<(String, Folder)> {
        let mut out: Vec<(String, Folder)> = self.shared.browsers.lock().unwrap_or_else(|e| e.into_inner()).values().filter(|b| b.user == user).flat_map(|b| b.folders.iter().map(|f| (b.device.clone(), f.clone()))).collect();
        out.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        out.dedup_by(|a, b| a.1.name.eq_ignore_ascii_case(&b.1.name) && a.0 == b.0);
        out
    }

    /// Ask `user`'s open page with `folder` to do something in it, and wait
    /// (blocking, like `call_machine`). Never another person's page.
    pub fn call_folder(&self, user: &str, folder: &str, request: Value, timeout: std::time::Duration) -> Result<Value, String> {
        self.folder_call(user, folder, request, timeout, false)
    }

    /// The same, only to a page where that folder is trusted (a write that
    /// didn't ask): never a same-named folder on another PC that isn't.
    pub fn call_trusted_folder(&self, user: &str, folder: &str, request: Value, timeout: std::time::Duration) -> Result<Value, String> {
        self.folder_call(user, folder, request, timeout, true)
    }

    /// Whether the page a request for `folder` would go to trusts it: decided
    /// by the same pick as the request itself.
    pub fn folder_trusted(&self, user: &str, folder: &str) -> bool {
        let browsers = self.shared.browsers.lock().unwrap_or_else(|e| e.into_inner());
        let visible = self.shared.visible.lock().unwrap_or_else(|e| e.into_inner());
        let pages: Vec<(u64, &str, &[Folder], Option<Instant>)> = browsers.iter().map(|(c, b)| (*c, b.user.as_str(), b.folders.as_slice(), visible.get(c).and_then(|v| v.1))).collect();
        pick_page(&pages, user, folder, false).is_ok_and(|conn| browsers[&conn].folders.iter().any(|f| f.trusted && f.name.eq_ignore_ascii_case(folder)))
    }

    fn folder_call(&self, user: &str, folder: &str, mut request: Value, timeout: std::time::Duration, trusted: bool) -> Result<Value, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let id = self.shared.next_call.fetch_add(1, Ordering::SeqCst);
        let pending = {
            let browsers = self.shared.browsers.lock().unwrap_or_else(|e| e.into_inner());
            let visible = self.shared.visible.lock().unwrap_or_else(|e| e.into_inner());
            let pages: Vec<(u64, &str, &[Folder], Option<Instant>)> = browsers.iter().map(|(c, b)| (*c, b.user.as_str(), b.folders.as_slice(), visible.get(c).and_then(|v| v.1))).collect();
            let conn = pick_page(&pages, user, folder, trusted).map_err(|e| if trusted { format!("{folder:?} isn't trusted on the PC lyra would use: the change needs your yes") } else { e })?;
            let b = &browsers[&conn];
            b.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
            request["type"] = json!("fs");
            request["id"] = json!(id);
            request["folder"] = json!(folder);
            b.to_page.send(request.to_string()).map_err(|_| "that page just closed".to_string())?;
            b.pending.clone()
        };
        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => {
                pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                Err(format!("the page with {folder:?} didn't answer within {}s (is lyra still open there?)", timeout.as_secs()))
            }
        }
    }

    /// Like `call_machine`, for long jobs: `on_progress` gets the machine's
    /// progress events as they come; setting `cancel` asks it to stop.
    pub fn call_machine_streaming(
        &self,
        name: &str,
        mut request: Value,
        timeout: std::time::Duration,
        cancel: &std::sync::atomic::AtomicBool,
        on_progress: &dyn Fn(Value),
    ) -> Result<Value, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (ptx, prx) = std::sync::mpsc::channel();
        let id = self.shared.next_call.fetch_add(1, Ordering::SeqCst);
        let (pending, progress, to_node) = {
            let machines = self.shared.machines.lock().unwrap_or_else(|e| e.into_inner());
            let m = machines
                .values()
                .find(|m| m.info.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| format!("{name} isn't connected (is `lyra node` running on it?)"))?;
            m.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
            m.progress.lock().unwrap_or_else(|e| e.into_inner()).insert(id, ptx);
            request["id"] = json!(id);
            m.to_node.send(request.to_string()).map_err(|_| format!("{name} just disconnected"))?;
            (m.pending.clone(), m.progress.clone(), m.to_node.clone())
        };
        let started = std::time::Instant::now();
        let mut asked_stop = false;
        let result = loop {
            while let Ok(e) = prx.try_recv() {
                on_progress(e);
            }
            match rx.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(r) => break r,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break Err(format!("{name} disconnected")),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            if cancel.load(Ordering::SeqCst) && !asked_stop {
                asked_stop = true;
                let _ = to_node.send(json!({ "type": "code_stop", "job": id }).to_string());
            }
            if started.elapsed() > timeout {
                let _ = to_node.send(json!({ "type": "code_stop", "job": id }).to_string());
                pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                break Err(format!("{name} didn't finish within {} min", timeout.as_secs() / 60));
            }
        };
        while let Ok(e) = prx.try_recv() {
            on_progress(e);
        }
        progress.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        result
    }

    pub fn connections(&self) -> usize {
        self.shared.visible.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether someone is looking at lyra right now (a device has it open
    /// and on screen): then there's no need to notify.
    pub fn someone_watching(&self) -> bool {
        self.shared.visible.lock().unwrap_or_else(|e| e.into_inner()).values().any(|v| v.1.is_some_and(|t| t.elapsed().as_secs() < 90))
    }

    /// Whether this user has lyra open and on screen somewhere.
    pub fn watching(&self, user: &str) -> bool {
        self.shared.visible.lock().unwrap_or_else(|e| e.into_inner()).values().any(|v| v.0 == user && v.1.is_some_and(|t| t.elapsed().as_secs() < 90))
    }

    /// Push a notification to every device that allowed them (in the
    /// background). Subscriptions the push service says are gone are dropped.
    pub fn notify(&self, n: Notification) {
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let payload = json!({ "title": n.title, "body": n.body, "tag": n.tag, "approval": n.approval, "url": n.url,
                "actions": n.actions.iter().map(|(a, t)| json!({ "action": a, "title": t })).collect::<Vec<_>>(), "ref": n.reference }).to_string();
            for d in shared.devices.list() {
                let Some(sub) = &d.push else { continue };
                let mine = match &n.to {
                    To::Everyone => true,
                    To::Admins => shared.users.who(d.user.as_deref()).is_some_and(|w| w.admin),
                    To::User(u) => d.user.as_deref() == Some(u.as_str()) && shared.users.who(Some(u)).is_some(),
                };
                if !mine {
                    continue;
                }
                match push::send(&shared.vapid, &shared.subject, sub, payload.as_bytes()) {
                    push::Sent::Ok => {
                        shared.push_sent.fetch_add(1, Ordering::Relaxed);
                    }
                    push::Sent::Gone => {
                        let _ = shared.devices.set_push(&d.id, None);
                    }
                    push::Sent::Failed(e) => {
                        shared.push_failed.fetch_add(1, Ordering::Relaxed);
                        *shared.push_error.lock().unwrap_or_else(|e| e.into_inner()) = format!("{}: {e}", d.name);
                    }
                }
            }
        });
    }
}

fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/", get(|headers: HeaderMap| app_file("index.html", headers)))
        .route("/api/pair", post(pair))
        .route("/api/pair/request", post(pair_request))
        .route("/api/pair/request/{id}", get(pair_poll))
        .route("/download/lyra-node", get(download_node))
        .route("/download/lyra-node.sha256", get(download_node_sha))
        .route("/install.sh", get(install_script))
        .route("/download/lyra-node.exe", get(download_node_exe))
        .route("/download/lyra-node.exe.sha256", get(download_node_exe_sha))
        .route("/install.ps1", get(install_ps1))
        .route("/api/me", get(me))
        .route("/api/auth", get(auth_info))
        .route("/auth/login", get(auth_login))
        .route("/auth/callback", get(auth_callback))
        .route("/api/auth/redeem", post(auth_redeem))
        .route("/api/auth/password", post(password_sign_in))
        .route("/api/auth/password/change", post(password_change))
        .route("/api/connect/calendar", post(connect_calendar))
        .route("/api/vapid", get(vapid_key))
        .route("/api/push", post(set_push))
        .route("/api/test-push", post(test_push))
        .route("/api/approve", post(approve))
        .route("/api/action", post(action))
        .route("/api/files", post(upload).layer(axum::extract::DefaultBodyLimit::max(uploads::MAX_BYTES + 1024)))
        .route("/api/files/{id}", get(download))
        .route("/api/backups/latest", get(latest_backup))
        .route("/ws", get(ws))
        .route("/health", get(health))
        .route("/node", get(node_ws))
        .fallback(get(|uri: axum::http::Uri, headers: HeaderMap| app_file_owned(uri.path().trim_start_matches('/').to_string(), headers)))
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
async fn app_file(path: &'static str, headers: HeaderMap) -> Response {
    app_file_owned(path.to_string(), headers).await
}

/// Whether the browser takes gzip (every current one does).
fn takes_gzip(headers: &HeaderMap) -> bool {
    headers.get(header::ACCEPT_ENCODING).and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(',').any(|e| e.trim().split(';').next() == Some("gzip")))
}

/// A bundle file gzipped, made once and kept: the embedded app never changes
/// while lyra runs, and a phone's first load is a third of the bytes.
fn gzipped(path: &str, bytes: &'static [u8]) -> std::sync::Arc<Vec<u8>> {
    static CACHE: std::sync::Mutex<Option<HashMap<String, std::sync::Arc<Vec<u8>>>>> = std::sync::Mutex::new(None);
    let mut all = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    all.get_or_insert_with(HashMap::new)
        .entry(path.to_string())
        .or_insert_with(|| {
            use std::io::Write;
            let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
            let _ = z.write_all(bytes);
            std::sync::Arc::new(z.finish().unwrap_or_default())
        })
        .clone()
}

/// Worth compressing: text (scripts, styles, maps, manifests, SVG).
fn compressible(kind: &str) -> bool {
    kind.starts_with("text/") || kind.contains("javascript") || kind.contains("json") || kind == "image/svg+xml" || kind == "application/wasm"
}

async fn app_file_owned(path: String, headers: HeaderMap) -> Response {
    let path = if path.is_empty() { "index.html".to_string() } else { path };
    let Some(file) = APP.get_file(&path) else { return error(StatusCode::NOT_FOUND, "not found") };
    let kind = content_type(&path);
    if path == "index.html" || path == "sw.js" {
        let text = String::from_utf8_lossy(file.contents()).replace("__LYRA_VERSION__", app_version());
        return ([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, "no-cache")], text).into_response();
    }
    let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "max-age=86400" };
    if compressible(kind) && file.contents().len() > 1024 && takes_gzip(&headers) {
        let body = gzipped(&path, file.contents());
        return ([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, cache), (header::CONTENT_ENCODING, "gzip"), (header::VARY, "Accept-Encoding")], body.as_ref().clone()).into_response();
    }
    ([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, cache), (header::VARY, "Accept-Encoding")], file.contents()).into_response()
}

fn error(status: StatusCode, text: &str) -> Response {
    (status, Json(json!({ "error": text }))).into_response()
}

fn bearer(headers: &HeaderMap) -> String {
    headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("").to_string()
}

/// A phone or browser (not a machine's node token), and whose it is.
fn device(shared: &Shared, headers: &HeaderMap) -> Result<(Device, Who), Box<Response>> {
    let d = shared
        .devices
        .authenticate(&bearer(headers))
        .filter(|d| d.kind == "device")
        .ok_or_else(|| Box::new(error(StatusCode::UNAUTHORIZED, "not paired: pair this device again")))?;
    let who = shared.users.who(d.user.as_deref()).ok_or_else(|| Box::new(error(StatusCode::FORBIDDEN, "your lyra account isn't active: ask an admin")))?;
    Ok((d, who))
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
        Ok((d, who)) => {
            // Whether they sign in with a password, and must choose one now (a one-time one).
            let u = s.users.get(&who.user);
            let (password, must_change) = u.map_or((false, false), |u| (!u.username.is_empty(), u.must_change));
            Json(json!({ "id": d.id, "name": d.name, "push": d.push.is_some(), "user": who, "password": password, "must_change": must_change })).into_response()
        }
        Err(r) => *r,
    }
}

/// How this server lets people in: Microsoft sign-in when it's set up, a
/// username and password when anyone has one (or there's no Microsoft), and
/// a pairing code always.
async fn auth_info(State(s): State<Arc<Shared>>) -> Response {
    let passwords = !s.entra.ready() || s.users.list().iter().any(|u| !u.username.is_empty());
    Json(json!({ "entra": s.entra.ready(), "passwords": passwords })).into_response()
}

#[derive(Deserialize)]
struct PasswordBody {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    /// What this browser is called (its devices list shows it).
    #[serde(default)]
    device: String,
}

/// After this many wrong passwords for one username, it waits this long.
const MAX_TRIES: u32 = 5;
const LOCKED_FOR: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Sign in with a username and password (no Microsoft needed): a device token for this browser.
async fn password_sign_in(State(s): State<Arc<Shared>>, Json(b): Json<PasswordBody>) -> Response {
    // Every try takes a moment, right or wrong.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let key = b.username.trim().to_lowercase();
    {
        let mut tries = s.tries.lock().unwrap_or_else(|e| e.into_inner());
        tries.retain(|_, (_, since)| since.elapsed() < LOCKED_FOR);
        if tries.get(&key).is_some_and(|(n, _)| *n >= MAX_TRIES) {
            return error(StatusCode::TOO_MANY_REQUESTS, "too many wrong passwords: wait 15 minutes, or ask an admin to reset it");
        }
    }
    let (users, username, password) = (s.users.clone(), b.username.clone(), b.password.clone());
    // Argon2 takes a little CPU: off the async threads.
    let checked = tokio::task::spawn_blocking(move || users.sign_in(&username, &password)).await.unwrap_or_else(|e| Err(e.to_string()));
    let user = match checked {
        Ok(u) => {
            s.tries.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
            u
        }
        Err(e) => {
            if e.contains("don't match") {
                let mut tries = s.tries.lock().unwrap_or_else(|e| e.into_inner());
                let t = tries.entry(key).or_insert((0, Instant::now()));
                t.0 += 1;
            }
            return error(StatusCode::FORBIDDEN, &e);
        }
    };
    let device = if b.device.trim().is_empty() { "Browser" } else { b.device.trim() };
    match s.devices.add(&format!("{} · {device}", user.name), "device", Some(&user.id)) {
        Ok((d, token)) => {
            let _ = s.inbound.send(Inbound::DevicesChanged);
            Json(json!({ "token": token, "device": { "id": d.id, "name": d.name, "kind": d.kind }, "must_change": user.must_change })).into_response()
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

#[derive(Deserialize)]
struct ChangeBody {
    #[serde(default)]
    old: String,
    #[serde(default)]
    new: String,
}

/// Change one's own password (the old one first, unless it's a one-time one being replaced).
async fn password_change(State(s): State<Arc<Shared>>, headers: HeaderMap, Json(b): Json<ChangeBody>) -> Response {
    let (_, who) = match device(&s, &headers) {
        Ok(x) => x,
        Err(r) => return *r,
    };
    let Some(user) = s.users.get(&who.user) else { return error(StatusCode::NOT_FOUND, "no such user") };
    let need_old = !user.must_change;
    if need_old && b.old.is_empty() {
        return error(StatusCode::BAD_REQUEST, "type your current password first");
    }
    let users = s.users.clone();
    let id = who.user.clone();
    let done = tokio::task::spawn_blocking(move || users.change_password(&id, need_old.then_some(b.old.as_str()), &b.new)).await.unwrap_or_else(|e| Err(e.to_string()));
    match done {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, &e),
    }
}

#[derive(Deserialize)]
struct LoginQuery {
    #[serde(default)]
    device: String,
}

fn redirect(to: &str) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, to.to_string()), (header::CACHE_CONTROL, "no-store".to_string())]).into_response()
}

/// Back to the app with a message for the sign-in screen.
fn signin_error(text: &str) -> Response {
    redirect(&format!("/#signin-error={}", oidc::encode(text)))
}

/// Off to Microsoft.
async fn auth_login(State(s): State<Arc<Shared>>, Query(q): Query<LoginQuery>) -> Response {
    if !s.entra.ready() || s.public_url.is_empty() {
        return signin_error("Microsoft sign-in isn't set up on this server ([web.entra] and public_url)");
    }
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let (state, verifier, nonce) = (devices::random(32, alphabet), devices::random(64, alphabet), devices::random(32, alphabet));
    let device: String = q.device.trim().chars().take(60).collect();
    {
        let mut logins = s.logins.lock().unwrap_or_else(|e| e.into_inner());
        logins.retain(|_, p| p.created.elapsed() < std::time::Duration::from_secs(600));
        if logins.len() > 200 {
            return signin_error("too many sign-ins at once: try again in a few minutes");
        }
        logins.insert(state.clone(), oidc::Pending { verifier: verifier.clone(), nonce: nonce.clone(), device: if device.is_empty() { "browser".into() } else { device }, created: Instant::now(), connect: None });
    }
    let callback = format!("{}/auth/callback", s.public_url);
    // The sign-in belongs to this browser: only it can finish it.
    let mut r = redirect(&oidc::authorize_url(&s.entra, &callback, &state, &nonce, &verifier));
    if let Ok(v) = HeaderValue::from_str(&format!("{LOGIN_COOKIE}={state}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=600")) {
        r.headers_mut().insert(header::SET_COOKIE, v);
    }
    r
}

const LOGIN_COOKIE: &str = "lyra_login";

/// A signed-in person connects their own Outlook calendar: where to send
/// their browser (the sign-in cookie comes with the answer).
async fn connect_calendar(State(s): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    let (_, who) = match device(&s, &headers) {
        Ok(x) => x,
        Err(r) => return *r,
    };
    if !s.entra.ready() || s.public_url.is_empty() {
        return error(StatusCode::BAD_REQUEST, "Microsoft sign-in isn't set up on this server");
    }
    // The Microsoft account connected must be their own (when lyra knows it).
    let oid = s.users.get(&who.user).map(|u| u.oid).unwrap_or_default();
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let (state, verifier, nonce) = (devices::random(32, alphabet), devices::random(64, alphabet), devices::random(32, alphabet));
    {
        let mut logins = s.logins.lock().unwrap_or_else(|e| e.into_inner());
        logins.retain(|_, p| p.created.elapsed() < std::time::Duration::from_secs(600));
        if logins.len() > 200 {
            return error(StatusCode::TOO_MANY_REQUESTS, "too many sign-ins at once");
        }
        logins.insert(state.clone(), oidc::Pending { verifier: verifier.clone(), nonce: nonce.clone(), device: String::new(), created: Instant::now(), connect: Some((who.user.clone(), oid)) });
    }
    let callback = format!("{}/auth/callback", s.public_url);
    let url = oidc::authorize_url_for(&s.entra, &callback, &state, &nonce, &verifier, s.entra.connect_scope());
    let mut r = Json(json!({ "url": url })).into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("{LOGIN_COOKIE}={state}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=600")) {
        r.headers_mut().insert(header::SET_COOKIE, v);
    }
    r
}

/// The browser's sign-in cookie (its state), if any.
fn login_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().strip_prefix(&format!("{LOGIN_COOKIE}=")).map(str::to_string))
        .find(|c| !c.is_empty())
}

#[derive(Deserialize)]
struct RedeemBody {
    code: String,
}

/// The browser that signed in collects its device token (once, soon, with
/// its own sign-in cookie): a link made from someone else's sign-in is useless.
async fn auth_redeem(State(s): State<Arc<Shared>>, headers: HeaderMap, Json(b): Json<RedeemBody>) -> Response {
    let cookie = login_cookie(&headers);
    let found = s.handoffs.lock().unwrap_or_else(|e| e.into_inner()).remove(b.code.trim());
    let clear = (header::SET_COOKIE, format!("{LOGIN_COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0"));
    match found {
        Some((token, state, at)) if at.elapsed() < std::time::Duration::from_secs(120) && cookie.as_deref() == Some(state.as_str()) => {
            ([clear], Json(json!({ "token": token }))).into_response()
        }
        _ => error(StatusCode::FORBIDDEN, "that sign-in didn't start in this browser (or it's too old): sign in again"),
    }
}

#[derive(Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    error_description: String,
}

/// Back from Microsoft: who it is, then a device token for this browser.
async fn auth_callback(State(s): State<Arc<Shared>>, Query(q): Query<CallbackQuery>, headers: HeaderMap) -> Response {
    // Only the browser that started this sign-in may finish it.
    if login_cookie(&headers).as_deref() != Some(q.state.as_str()) {
        return signin_error("that sign-in didn't start in this browser: sign in again");
    }
    let Some(pending) = s.logins.lock().unwrap_or_else(|e| e.into_inner()).remove(&q.state) else {
        return signin_error("that sign-in has expired: try again");
    };
    if pending.created.elapsed() > std::time::Duration::from_secs(600) {
        return signin_error("that sign-in has expired: try again");
    }
    if q.code.is_empty() {
        return signin_error(if q.error_description.is_empty() { "Microsoft didn't sign you in" } else { &q.error_description });
    }
    let callback = format!("{}/auth/callback", s.public_url);
    let scope = if pending.connect.is_some() { s.entra.connect_scope() } else { oidc::SIGN_IN };
    let form = oidc::token_form_for(&s.entra, &q.code, &callback, &pending.verifier, scope);
    let answer = async {
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
        let resp = client.post(oidc::token_url(&s.entra)).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded").body(form).send().await.map_err(|e| format!("Microsoft isn't answering: {e}"))?;
        let ok = resp.status().is_success();
        let body: Value = serde_json::from_str(&resp.text().await.map_err(|e| e.to_string())?).unwrap_or(json!({}));
        if !ok {
            return Err(body["error_description"].as_str().unwrap_or("Microsoft refused the sign-in").lines().next().unwrap_or("").to_string());
        }
        let id = body["id_token"].as_str().map(str::to_string).ok_or_else(|| "Microsoft sent no id_token".to_string())?;
        Ok((id, body["refresh_token"].as_str().unwrap_or("").to_string(), body["scope"].as_str().unwrap_or("").to_string()))
    }
    .await;
    let (id_token, refresh, granted) = match answer {
        Ok(t) => t,
        Err(e) => return signin_error(&e),
    };
    let person = match oidc::check(&s.entra, &id_token, &pending.nonce, chrono::Utc::now().timestamp()) {
        Ok(p) => p,
        Err(e) => return signin_error(&e),
    };
    // Connecting a calendar: theirs only, then back to the app.
    if let Some((user, oid)) = pending.connect {
        if !oid.is_empty() && oid != person.oid {
            return redirect(&format!("/?page=more#connect-error={}", oidc::encode("that's a different Microsoft account from the one you sign in to lyra with")));
        }
        if refresh.is_empty() {
            return redirect(&format!("/?page=more#connect-error={}", oidc::encode("Microsoft didn't allow lasting access (offline_access)")));
        }
        let _ = s.inbound.send(Inbound::Connected { user, service: "graph".into(), token: refresh, scope: granted });
        return redirect("/?page=more#connected=calendar");
    }
    // A guest from another organization never becomes the owner.
    let owner_email = if person.guest { "" } else { s.entra.owner_email.as_str() };
    let user = match s.users.signed_in(&person.oid, &person.tenant, &person.name, &person.email, owner_email) {
        Ok(u) => u,
        Err(e) => return signin_error(&e),
    };
    if user.status == users::Status::Disabled {
        return signin_error("your lyra account is turned off: ask an admin");
    }
    if user.status == users::Status::Pending {
        let _ = s.inbound.send(Inbound::SignIn { name: user.name.clone(), email: user.email.clone() });
    }
    match s.devices.add(&format!("{} · {}", user.name, pending.device), "device", Some(&user.id)) {
        // The browser collects the token with a one-time code (and its cookie).
        Ok((_, token)) => {
            let code = devices::random(32, b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789");
            let mut h = s.handoffs.lock().unwrap_or_else(|e| e.into_inner());
            h.retain(|_, v| v.2.elapsed() < std::time::Duration::from_secs(120));
            h.insert(code.clone(), (token, q.state.clone(), Instant::now()));
            redirect(&format!("/#signin-code={code}"))
        }
        Err(e) => signin_error(&e),
    }
}

async fn vapid_key(State(s): State<Arc<Shared>>) -> Response {
    Json(json!({ "key": s.vapid.public_base64() })).into_response()
}

async fn set_push(State(s): State<Arc<Shared>>, headers: HeaderMap, Json(b): Json<Value>) -> Response {
    let d = match device(&s, &headers) {
        Ok((d, _)) => d,
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
        Ok((d, _)) => d,
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
    let (d, who) = match device(&s, &headers) {
        Ok(x) => x,
        Err(r) => return *r,
    };
    let raw = headers.get("x-filename").and_then(|v| v.to_str().ok()).unwrap_or("file");
    let name = percent_decode(raw);
    let mime = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let saved = tokio::task::spawn_blocking(move || s.uploads.save(&name, &mime, &d.name, &who.user, &body)).await;
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

/// Its sender, an admin, or a machine (placing a file the user sent) fetches an upload.
async fn download(State(s): State<Arc<Shared>>, headers: HeaderMap, axum::extract::Path(id): axum::extract::Path<String>) -> Response {
    let Some(d) = s.devices.authenticate(&bearer(&headers)) else { return error(StatusCode::UNAUTHORIZED, "not paired") };
    let Some((up, path)) = s.uploads.get(&id) else { return error(StatusCode::NOT_FOUND, "no such file") };
    if d.kind != "node" {
        let who = s.users.who(d.user.as_deref());
        let theirs = who.as_ref().is_some_and(|w| w.admin || up.user.as_deref().unwrap_or(users::OWNER) == w.user);
        if !theirs {
            return error(StatusCode::NOT_FOUND, "no such file");
        }
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, up.mime.clone()),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", up.name.replace('"', ""))),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

/// The newest backup of lyra, for a paired phone or browser (not a machine)
/// to keep a copy off the server.
async fn latest_backup(State(s): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    match device(&s, &headers) {
        Err(r) => return *r,
        Ok((_, who)) if !who.admin => return error(StatusCode::FORBIDDEN, "backups are for admins"),
        Ok(_) => {}
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
    let (d, who) = match device(&s, &headers) {
        Ok(x) => x,
        Err(r) => return *r,
    };
    let _ = s.inbound.send(Inbound::Approve { id: b.id, answer: b.answer, device: d.name, who });
    Json(json!({ "ok": true })).into_response()
}

#[derive(Deserialize)]
struct ActionBody {
    action: String,
    #[serde(rename = "ref")]
    reference: String,
}

/// From a notification's other buttons (Done, Snooze).
async fn action(State(s): State<Arc<Shared>>, headers: HeaderMap, Json(b): Json<ActionBody>) -> Response {
    let (d, who) = match device(&s, &headers) {
        Ok(x) => x,
        Err(r) => return *r,
    };
    let _ = s.inbound.send(Inbound::Action { action: b.action, reference: b.reference, device: d.name, who });
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

/// A WebSocket's token: offered as a subprotocol (`lyra, <token>`) so it stays
/// out of URLs and proxy logs; the query string still works for old clients.
fn socket_token(headers: &HeaderMap, query: &str) -> String {
    let offered = headers.get(header::SEC_WEBSOCKET_PROTOCOL).and_then(|v| v.to_str().ok()).unwrap_or("");
    let mut parts = offered.split(',').map(str::trim);
    match (parts.next(), parts.next()) {
        (Some("lyra"), Some(t)) if !t.is_empty() => t.to_string(),
        _ => query.to_string(),
    }
}

async fn ws(State(s): State<Arc<Shared>>, Query(q): Query<WsQuery>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    let token = socket_token(&headers, &q.token);
    let Some(d) = s.devices.authenticate(&token).filter(|d| d.kind == "device") else { return error(StatusCode::UNAUTHORIZED, "not paired") };
    let Some(who) = s.users.who(d.user.as_deref()) else { return error(StatusCode::FORBIDDEN, "your lyra account isn't active: ask an admin") };
    let session = if q.session.is_empty() { d.last_session.clone().unwrap_or_default() } else { q.session.clone() };
    let mut r = upgrade.protocols(["lyra"]).on_upgrade(move |socket| connection(s, d, who, session, socket));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// A conversation's snapshot, with the device's details.
async fn snapshot(s: &Shared, d: &Device, who: &Who, session: &str) -> Option<Value> {
    let (tx, rx) = oneshot::channel();
    s.inbound.send(Inbound::Snapshot { session: session.to_string(), who: who.clone(), reply: tx }).ok()?;
    let mut snap = rx.await.ok()?;
    snap["device"] = json!({ "id": d.id, "name": d.name, "push": d.push.is_some() });
    snap["user"] = json!(who);
    Some(snap)
}

/// One device's live connection: a conversation's state, then its updates
/// (and everyone's); its messages go to that conversation.
async fn connection(s: Arc<Shared>, d: Device, mut who: Who, session: String, mut socket: WebSocket) {
    let conn = s.next_conn.fetch_add(1, Ordering::SeqCst);
    let (moved_tx, mut moved) = tokio::sync::mpsc::unbounded_channel::<String>();
    // lyra's requests for this page's folders.
    let (page_tx, mut to_page) = tokio::sync::mpsc::unbounded_channel::<String>();
    let page_pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    // Answers to the page's requests: each awaited in its own task, so a slow
    // one (search everything, Enhance, models) doesn't hold up live updates.
    let (answers_tx, mut answers) = tokio::sync::mpsc::unbounded_channel::<String>();
    s.visible.lock().unwrap_or_else(|e| e.into_inner()).insert(conn, (who.user.clone(), Some(Instant::now())));
    s.online.lock().unwrap_or_else(|e| e.into_inner()).insert(conn, (d.id.clone(), d.name.clone()));
    // Subscribe before asking for the snapshot, so nothing falls in between.
    let mut updates = s.out.subscribe();
    let Some(first) = snapshot(&s, &d, &who, &session).await else { return };
    // The app says which conversation "" turned out to be.
    let mut session = first["session_id"].as_str().unwrap_or(&session).to_string();
    s.conns.lock().unwrap_or_else(|e| e.into_inner()).insert(conn, ConnState { session: session.clone(), moved: moved_tx });
    let _ = s.devices.set_last_session(&d.id, &session);
    let _ = s.inbound.send(Inbound::DevicesChanged);
    let forget = |s: &Shared| {
        s.visible.lock().unwrap_or_else(|e| e.into_inner()).remove(&conn);
        s.online.lock().unwrap_or_else(|e| e.into_inner()).remove(&conn);
        s.conns.lock().unwrap_or_else(|e| e.into_inner()).remove(&conn);
        if let Some(b) = s.browsers.lock().unwrap_or_else(|e| e.into_inner()).remove(&conn) {
            fail_pending(&b.pending, "that lyra page closed");
        }
        let _ = s.inbound.send(Inbound::DevicesChanged);
    };
    if socket.send(Message::Text(first.to_string().into())).await.is_err() {
        forget(&s);
        return;
    }
    // Still paired, still let in, still the same role? Asked before every
    // message and every half minute: removing a device, turning someone off
    // or changing their role takes effect at once.
    let current = |s: &Shared| s.devices.get(&d.id).and_then(|dev| s.users.who(dev.user.as_deref()));
    let mut recheck = tokio::time::interval(std::time::Duration::from_secs(30));
    recheck.tick().await;
    loop {
        tokio::select! {
            _ = recheck.tick() => {
                match current(&s) {
                    Some(now) if now == who => {}
                    _ => {
                        let _ = socket.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
            Some(req) = to_page.recv() => {
                if socket.send(Message::Text(req.into())).await.is_err() {
                    break;
                }
            }
            Some(answer) = answers.recv() => {
                if socket.send(Message::Text(answer.into())).await.is_err() {
                    break;
                }
            }
            to = moved.recv() => {
                let Some(to) = to else { break };
                session = to;
                let _ = s.devices.set_last_session(&d.id, &session);
                let Some(snap) = snapshot(&s, &d, &who, &session).await else { break };
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
                match current(&s) {
                    Some(now) => who = now,
                    None => {
                        let _ = socket.send(Message::Close(None)).await;
                        break;
                    }
                }
                match v["type"].as_str().unwrap_or("") {
                    "send" => {
                        let text = v["text"].as_str().unwrap_or("").trim().to_string();
                        let files: Vec<String> = v["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str().map(str::to_string)).take(10).collect();
                        if !text.is_empty() || !files.is_empty() {
                            let _ = s.inbound.send(Inbound::Send { text, device: d.name.clone(), who: who.clone(), session: session.clone(), conn, files });
                        }
                    }
                    "stop" => {
                        let _ = s.inbound.send(Inbound::Stop { device: d.name.clone(), who: who.clone(), session: session.clone() });
                    }
                    "approve" => {
                        let _ = s.inbound.send(Inbound::Approve {
                            id: v["id"].as_u64().unwrap_or(0),
                            answer: v["answer"].as_str().unwrap_or("").to_string(),
                            device: d.name.clone(),
                            who: who.clone(),
                        });
                    }
                    "visible" => {
                        let at = (v["visible"] == true).then(Instant::now);
                        s.visible.lock().unwrap_or_else(|e| e.into_inner()).insert(conn, (who.user.clone(), at));
                    }
                    "ping" => {
                        let _ = socket.send(Message::Text(json!({ "type": "pong" }).to_string().into())).await;
                    }
                    // The folders this page lends lyra (on connect and on every change).
                    "folders" => {
                        // Only folders said to be the signed-in person's: a page (or an
                        // older app) lending someone else's, or nobody's, lends none.
                        let theirs = v["user"].as_str() == Some(who.user.as_str());
                        let folders: Vec<Folder> = if theirs { serde_json::from_value(v["folders"].clone()).unwrap_or_default() } else { Vec::new() };
                        let folders: Vec<Folder> = folders.into_iter().filter(|f| !f.name.trim().is_empty()).take(50).collect();
                        let mut browsers = s.browsers.lock().unwrap_or_else(|e| e.into_inner());
                        // Always the person signed in now (re-checked above).
                        let b = browsers.entry(conn).or_insert_with(|| Browser { user: who.user.clone(), device: d.name.clone(), to_page: page_tx.clone(), folders: vec![], pending: page_pending.clone() });
                        b.user = who.user.clone();
                        b.folders = folders;
                    }
                    "fs_result" => {
                        let id = v["id"].as_u64().unwrap_or(0);
                        if let Some(tx) = page_pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
                            let result = match v["error"].as_str() {
                                Some(e) => Err(e.to_string()),
                                None => Ok(v["result"].clone()),
                            };
                            let _ = tx.send(result);
                        }
                    }
                    "pair_answer" => {
                        let hub = Hub { shared: s.clone(), address: "0.0.0.0:0".parse().expect("address") };
                        let key = v["id"].as_str().or(v["code"].as_str()).unwrap_or("").to_string();
                        let text = if !who.admin {
                            "only an admin can let a machine or terminal in".to_string()
                        } else {
                            let user = v["user"].as_str().filter(|u| !u.trim().is_empty());
                            match hub.answer_pair(&key, v["approve"] == true, user) {
                                Ok(t) => t,
                                Err(e) => e,
                            }
                        };
                        let _ = s.inbound.send(Inbound::Note(format!("{} (from {})", text, d.name)));
                    }
                    "get" => {
                        let what = v["what"].as_str().unwrap_or("").to_string();
                        let (tx, rx) = oneshot::channel();
                        let ask = Inbound::Get { what: what.clone(), arg: v["arg"].clone(), session: session.clone(), who: who.clone(), reply: tx };
                        if s.inbound.send(ask).is_ok() {
                            let (arg, id, answers) = (v["arg"].clone(), v["id"].clone(), answers_tx.clone());
                            tokio::spawn(async move {
                                if let Ok(data) = rx.await {
                                    let _ = answers.send(json!({ "type": "data", "what": what, "arg": arg, "id": id, "data": data }).to_string());
                                }
                            });
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

async fn node_ws(State(s): State<Arc<Shared>>, Query(q): Query<WsQuery>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    let Some(d) = s.devices.authenticate(&socket_token(&headers, &q.token)).filter(|d| d.kind == "node") else { return error(StatusCode::UNAUTHORIZED, "not a paired machine") };
    upgrade.protocols(["lyra"]).on_upgrade(move |socket| node_connection(s, d, socket))
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
        harnesses: hello["harnesses"].clone(),
    };
    let conn = s.next_conn.fetch_add(1, Ordering::SeqCst);
    let (to_node, mut outgoing) = tokio::sync::mpsc::unbounded_channel::<String>();
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let progress: Arc<Mutex<HashMap<u64, std::sync::mpsc::Sender<Value>>>> = Default::default();
    let replaced = s.machines.lock().unwrap_or_else(|e| e.into_inner()).insert(d.name.to_lowercase(), MachineConn { info, conn, to_node, pending: pending.clone(), progress: progress.clone() });
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
                    "progress" => {
                        if let Some(tx) = progress.lock().unwrap_or_else(|e| e.into_inner()).get(&v["id"].as_u64().unwrap_or(0)) {
                            let _ = tx.send(v["event"].clone());
                        }
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

async fn download_node_exe(State(s): State<Arc<Shared>>) -> Response {
    let path = s.node_binary.with_extension("exe");
    match tokio::fs::read(&path).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/octet-stream"), (header::CONTENT_DISPOSITION, "attachment; filename=\"lyra-node.exe\"")], bytes).into_response(),
        Err(_) => error(StatusCode::NOT_FOUND, &format!("this server has no Windows lyra-node to hand out ({})", path.display())),
    }
}

async fn download_node_exe_sha(State(s): State<Arc<Shared>>) -> Response {
    let path = s.node_binary.with_extension("exe");
    match tokio::task::spawn_blocking(move || node_build(&path)).await.ok().flatten() {
        Some(sum) => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], format!("{sum}  lyra-node.exe\n")).into_response(),
        None => error(StatusCode::NOT_FOUND, "this server has no Windows lyra-node to hand out"),
    }
}

const INSTALL_PS1: &str = include_str!("../assets/install.ps1");

/// `irm https://lyra…/install.ps1 | iex` (as administrator) on Windows.
async fn install_ps1(State(s): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    let url = if s.public_url.is_empty() {
        let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost");
        let proto = headers.get("x-forwarded-proto").and_then(|h| h.to_str().ok()).unwrap_or("http");
        format!("{proto}://{host}")
    } else {
        s.public_url.clone()
    };
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], INSTALL_PS1.replace("__LYRA_URL__", &url)).into_response()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_files_go_out_gzipped_when_the_browser_takes_it() {
        let mut h = HeaderMap::new();
        assert!(!takes_gzip(&h));
        h.insert(header::ACCEPT_ENCODING, "br;q=1.0, gzip;q=0.8, deflate".parse().unwrap());
        assert!(takes_gzip(&h));
        h.insert(header::ACCEPT_ENCODING, "gzipx".parse().unwrap());
        assert!(!takes_gzip(&h));
        assert!(compressible("text/javascript; charset=utf-8") && compressible("text/css") && !compressible("image/png"));
        let text: &'static [u8] = Box::leak("let a = 1;\n".repeat(2000).into_bytes().into_boxed_slice());
        let z = gzipped("test/a.js", text);
        assert!(z.len() < text.len() / 10, "compressed");
        let mut back = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(z.as_slice()), &mut back).unwrap();
        assert_eq!(back, text);
        assert!(std::sync::Arc::ptr_eq(&z, &gzipped("test/a.js", text)), "made once");
    }

    #[test]
    fn folder_requests_only_reach_the_persons_own_pages() {
        let f = |name: &str, allowed: bool| Folder { name: name.into(), writable: true, allowed, trusted: false };
        let (dana, admin) = ([f("Firewall", true)], [f("Firewall", true), f("Budget", true)]);
        let earlier = Instant::now();
        let later = earlier + std::time::Duration::from_secs(5);
        let stale = [f("Firewall", false)];
        let pages = [(1, "dana", &dana[..], Some(earlier)), (2, "owner", &admin[..], Some(later)), (3, "dana", &stale[..], Some(later))];
        assert_eq!(pick_page(&pages, "dana", "firewall", false), Ok(1), "Dana's allowed page, not the owner's newer one");
        assert_eq!(pick_page(&pages, "owner", "Firewall", false), Ok(2));
        assert!(pick_page(&pages, "dana", "Budget", false).unwrap_err().contains("no open lyra page"), "never someone else's folder");
        assert!(pick_page(&pages[2..], "dana", "Firewall", false).unwrap_err().contains("needs your OK"));
        // Trusted on one PC, not on the newer one: a write that didn't ask goes only to the trusting PC (I-11).
        let trusting = [Folder { trusted: true, ..f("Firewall", true) }];
        let other = [f("Firewall", true)];
        let two = [(1, "dana", &trusting[..], Some(earlier)), (2, "dana", &other[..], Some(later))];
        assert_eq!(pick_page(&two, "dana", "Firewall", false), Ok(2), "the newest page answers requests");
        assert_eq!(pick_page(&two, "dana", "Firewall", true), Ok(1), "an unasked write: only where it's trusted");
        assert!(pick_page(&two[1..], "dana", "Firewall", true).is_err(), "nowhere trusted: it has to ask");
    }
}
