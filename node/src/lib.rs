//! `lyra-node`: lend a machine to lyra. It connects *out* to `lyra serve`
//! (nothing listens here) and runs the Operator's system tools on this
//! machine — shell, files, system info — with this machine's own rules:
//! everything is checked here again (`lyra_system::System::check`), forbidden
//! things never run, and changes run only when the user approved them.
//!
//! It also pairs without a keyboard-and-screen (the user approves the request
//! from a phone or terminal), updates itself from the server, and removes
//! itself when asked. The pairing and connection helpers are shared with
//! `lyra connect`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// This program's path, as it was at startup: once an update replaces the
/// file, the kernel reports the running one as "… (deleted)".
fn exe() -> PathBuf {
    static EXE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    EXE.get_or_init(|| {
        let p = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("lyra-node"));
        let s = p.display().to_string();
        PathBuf::from(s.strip_suffix(" (deleted)").unwrap_or(&s))
    })
    .clone()
}

/// `~/.config/lyra/node.toml`.
#[derive(Serialize, Deserialize)]
pub struct NodeConfig {
    /// lyra's address (`https://lyra.example.com`).
    pub url: String,
    pub token: String,
    /// What lyra calls this machine.
    pub name: String,
    /// This machine's rules: what may run without asking, what's off limits.
    #[serde(default)]
    pub system: lyra_system::Settings,
}

// ---- small helpers (shared with `lyra connect`)

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// `~` and `~/…` → the home directory.
pub fn expand(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some("") => home(),
        Some(rest) if rest.starts_with('/') => home().join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    }
}

pub fn config_dir() -> Option<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) if !d.is_empty() => Some(PathBuf::from(d).join("lyra")),
        _ => Some(PathBuf::from(std::env::var_os("HOME")?).join(".config/lyra")),
    }
}

pub fn config_path() -> PathBuf {
    config_dir().unwrap_or_else(|| PathBuf::from("/etc/lyra")).join("node.toml")
}

/// Write a file only its owner can read.
pub fn write_private(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path).map_err(|e| e.to_string())?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

/// rustls needs one crypto provider picked for the process.
pub fn tls_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn http() -> Result<reqwest::blocking::Client, String> {
    tls_provider();
    reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build().map_err(|e| e.to_string())
}

/// `ws(s)://host/<path>?token=…` from lyra's https address.
pub fn socket_url(base: &str, path: &str, token: &str) -> String {
    let base = base.trim_end_matches('/');
    let ws = base
        .strip_prefix("https://")
        .map(|h| format!("wss://{h}"))
        .or_else(|| base.strip_prefix("http://").map(|h| format!("ws://{h}")))
        .unwrap_or_else(|| base.to_string());
    format!("{ws}/{path}?token={token}")
}

fn is_root() -> bool {
    std::fs::read_to_string("/proc/self/status").is_ok_and(|s| s.lines().any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(1) == Some("0")))
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname").map(|h| h.trim().to_string()).unwrap_or_default()
}

fn os_name() -> String {
    std::fs::read_to_string("/etc/os-release")
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string()))
        .unwrap_or_default()
}

/// The sha256 of this program's file (how lyra tells builds apart), for the
/// standalone `lyra-node` (the full lyra can't update itself, and is big).
pub fn own_hash() -> String {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| if standalone() { std::fs::read(exe()).ok().map(|b| hex(&Sha256::digest(&b))).unwrap_or_default() } else { String::new() })
        .clone()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether this program can replace itself (the standalone `lyra-node`, not
/// `lyra node` inside the full lyra).
fn standalone() -> bool {
    exe().file_name().is_some_and(|n| n.to_string_lossy().starts_with("lyra-node"))
}

// ---- pairing

/// Pair with a code from `lyra pair` on the server. Returns the token.
pub fn pair(url: &str, code: &str, name: &str, kind: &str) -> Result<String, String> {
    let url = url.trim_end_matches('/');
    let resp = http()?
        .post(format!("{url}/api/pair"))
        .json(&json!({ "code": code, "name": name, "kind": kind }))
        .send()
        .map_err(|e| format!("can't reach {url}: {e}"))?;
    let ok = resp.status().is_success();
    let body: Value = resp.json().unwrap_or(json!({}));
    if !ok {
        return Err(body["error"].as_str().unwrap_or("pairing failed").to_string());
    }
    body["token"].as_str().map(str::to_string).ok_or("no token in the answer".into())
}

/// Pair without a code (a headless machine): ask, show a short code, and wait
/// while the user approves the request in lyra. Returns the token.
pub fn request_pairing(url: &str, name: &str, kind: &str) -> Result<String, String> {
    let url = url.trim_end_matches('/');
    let client = http()?;
    let resp = client
        .post(format!("{url}/api/pair/request"))
        .json(&json!({ "name": name, "kind": kind, "hostname": hostname(), "os": os_name() }))
        .send()
        .map_err(|e| format!("can't reach {url}: {e}"))?;
    let ok = resp.status().is_success();
    let body: Value = resp.json().unwrap_or(json!({}));
    if !ok {
        return Err(body["error"].as_str().unwrap_or("the pairing request failed").to_string());
    }
    let id = body["id"].as_str().unwrap_or("").to_string();
    println!(
        "Asked {url} to pair \"{name}\". Approve it in lyra — the web app, a phone notification, or\n\
         `/devices approve {}` in a lyra terminal. Code: {}\n(waiting up to {} minutes…)",
        body["code"].as_str().unwrap_or(""),
        body["code"].as_str().unwrap_or(""),
        body["expires_in"].as_u64().unwrap_or(600) / 60
    );
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let v: Value = match client.get(format!("{url}/api/pair/request/{id}")).send().and_then(|r| r.json()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match v["state"].as_str().unwrap_or("") {
            "approved" => return v["token"].as_str().map(str::to_string).ok_or("approved, but no token came back".into()),
            "denied" => return Err("the pairing request was denied".into()),
            "expired" | "unknown" => return Err("the pairing request expired: run it again".into()),
            _ => {}
        }
    }
}

// ---- requests from lyra

/// Answer a check or call from lyra, deciding everything here.
pub fn handle(system: &lyra_system::System, machine: &str, request: &Value) -> Result<Value, String> {
    let tool = request["tool"].as_str().unwrap_or("");
    let mut args = request["args"].clone();
    if let Some(map) = args.as_object_mut() {
        map.remove("machine");
    }
    let check = system.check(tool, &args);
    let what = |args: &Value| {
        let (what, detail) = system.describe(tool, args);
        let what = if what.contains("this machine") { what.replace("this machine", machine) } else { format!("{what} on {machine}") };
        (what, detail)
    };
    match request["type"].as_str().unwrap_or("") {
        "check" => Ok(match check {
            lyra_system::Check::Auto => json!({ "check": "auto" }),
            lyra_system::Check::Ask { why, dangerous } => {
                let (what, detail) = what(&args);
                json!({ "check": "ask", "what": what, "detail": detail, "why": why, "dangerous": dangerous })
            }
            lyra_system::Check::Forbidden(why) => json!({ "check": "forbidden", "why": why }),
        }),
        "call" => match check {
            lyra_system::Check::Forbidden(why) => Err(format!("refused on {machine}: {why}")),
            lyra_system::Check::Ask { why, .. } if request["approved"] != true => Err(format!("needs the user's approval ({why})")),
            _ => system.call(tool, &args),
        },
        other => Err(format!("unknown request {other:?}")),
    }
}

/// Fetch the server's current `lyra-node`, check it, and put it in place of
/// this program. Returns what happened; the caller restarts into it.
fn update(config: &NodeConfig) -> Result<Value, String> {
    if !standalone() {
        return Err("this node runs inside the full `lyra` program; update that with `cargo install` (or reinstall the node with install.sh)".into());
    }
    let base = config.url.trim_end_matches('/');
    let client = http()?;
    let expected = client
        .get(format!("{base}/download/lyra-node.sha256"))
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(|e| format!("can't get the new version's checksum: {e}"))?;
    let expected = expected.split_whitespace().next().unwrap_or("").to_string();
    let current = own_hash();
    if expected == current {
        return Ok(json!({ "updated": false, "why": "already the server's version" }));
    }
    let bytes = client
        .get(format!("{base}/download/lyra-node"))
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .map_err(|e| format!("download failed: {e}"))?;
    let got = hex(&Sha256::digest(&bytes));
    if got != expected {
        return Err(format!("the download doesn't match its checksum ({got} ≠ {expected}); nothing changed"));
    }
    let exe = exe();
    let tmp = exe.with_extension("new");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("can't write {}: {e}", tmp.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &exe).map_err(|e| format!("can't replace {}: {e}", exe.display()))?;
    Ok(json!({ "updated": true, "from": &current[..12.min(current.len())], "to": &expected[..12.min(expected.len())] }))
}

/// Restart as the (new) program in place, same pid, same arguments.
fn restart_in_place() -> ! {
    use std::os::unix::process::CommandExt;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let err = std::process::Command::new(exe()).args(args).exec();
    eprintln!("lyra-node: couldn't restart after updating: {err}");
    std::process::exit(1);
}

/// The service unit's path for this user (root: a system service).
fn unit_path() -> PathBuf {
    if is_root() { PathBuf::from("/etc/systemd/system/lyra-node.service") } else { home().join(".config/systemd/user/lyra-node.service") }
}

fn systemctl(args: &[&str]) -> bool {
    let mut cmd = std::process::Command::new("systemctl");
    if !is_root() {
        cmd.arg("--user");
    }
    cmd.args(args).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Remove this node from the machine: its service, its settings and (the
/// standalone build) its program. Returns what was removed.
fn uninstall() -> Vec<String> {
    let mut removed = Vec::new();
    let unit = unit_path();
    if unit.exists() {
        systemctl(&["disable", "lyra-node"]);
        if std::fs::remove_file(&unit).is_ok() {
            removed.push(unit.display().to_string());
        }
        systemctl(&["daemon-reload"]);
    }
    let config = config_path();
    if std::fs::remove_file(&config).is_ok() {
        removed.push(config.display().to_string());
    }
    let program = exe();
    if standalone() && std::fs::remove_file(&program).is_ok() {
        removed.push(program.display().to_string());
    }
    removed
}

// ---- the connection

fn hello() -> Value {
    json!({
        "type": "hello",
        "hostname": hostname(),
        "os": os_name(),
        "user": std::env::var("USER").unwrap_or_else(|_| if is_root() { "root".into() } else { String::new() }),
        "version": VERSION,
        "build": own_hash(),
        "self_update": standalone(),
    })
}

/// What a session ended with.
enum End {
    Lost(String),
    /// Updated: restart into the new program.
    Restart,
    /// Removed from this machine: stop.
    Removed,
}

async fn session(config: &NodeConfig, system: std::sync::Arc<lyra_system::System>) -> End {
    let url = socket_url(&config.url, "node", &config.token);
    let ws = match tokio_tungstenite::connect_async(url.as_str()).await {
        Ok((ws, _)) => ws,
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status().as_u16() == 401 => {
            return End::Lost("lyra doesn't know this machine any more: pair it again (lyra-node pair …)".into());
        }
        Err(e) => return End::Lost(e.to_string()),
    };
    let (mut sink, mut stream) = ws.split();
    if let Err(e) = sink.send(Message::Text(hello().to_string().into())).await {
        return End::Lost(e.to_string());
    }
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<(String, Option<bool>)>();
    let mut ping = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if let Err(e) = sink.send(Message::Text(json!({ "type": "ping" }).to_string().into())).await {
                    return End::Lost(e.to_string());
                }
            }
            out = out_rx.recv() => {
                if let Some((text, then)) = out {
                    let sent = sink.send(Message::Text(text.into())).await;
                    let _ = sink.flush().await;
                    match then {
                        Some(true) => return End::Restart,
                        Some(false) => return End::Removed,
                        None if sent.is_err() => return End::Lost("connection lost".into()),
                        None => {}
                    }
                }
            }
            incoming = stream.next() => {
                let msg = match incoming {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => return End::Lost(e.to_string()),
                    None => return End::Lost("lyra closed the connection".into()),
                };
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                let reply = |id: &Value, result: Result<Value, String>| match result {
                    Ok(value) => json!({ "type": "result", "id": id, "ok": true, "value": value }).to_string(),
                    Err(e) => json!({ "type": "result", "id": id, "ok": false, "error": e }).to_string(),
                };
                match v["type"].as_str().unwrap_or("") {
                    "welcome" => println!("connected to {} as {}", config.url, config.name),
                    "check" | "call" => {
                        let (system, machine, out) = (system.clone(), config.name.clone(), out_tx.clone());
                        // Calls can take a while; keep the connection (and pings) going.
                        tokio::task::spawn_blocking(move || {
                            let result = handle(&system, &machine, &v);
                            if v["type"] == "call" {
                                let tool = v["tool"].as_str().unwrap_or("");
                                let summary = v["args"]["command"].as_str().or(v["args"]["path"].as_str()).or(v["args"]["url"].as_str()).unwrap_or("");
                                println!("{tool} {summary} → {}", if result.is_ok() { "done" } else { "refused/failed" });
                            }
                            let _ = out.send((reply(&v["id"], result), None));
                        });
                    }
                    "update" => {
                        let out = out_tx.clone();
                        let (url, token, name) = (config.url.clone(), config.token.clone(), config.name.clone());
                        tokio::task::spawn_blocking(move || {
                            let config = NodeConfig { url, token, name, system: Default::default() };
                            let result = update(&config);
                            let restart = result.as_ref().is_ok_and(|v| v["updated"] == true);
                            println!("update: {}", match &result { Ok(v) => v.to_string(), Err(e) => e.clone() });
                            let _ = out.send((reply(&v["id"], result), restart.then_some(true)));
                        });
                    }
                    "uninstall" => {
                        let removed = uninstall();
                        println!("removed from this machine: {}", removed.join(", "));
                        let _ = out_tx.send((reply(&v["id"], Ok(json!({ "removed": removed }))), Some(false)));
                    }
                    _ => {}
                }
            }
        }
    }
}

/// `lyra-node` (no arguments): stay connected, reconnecting with a growing pause.
fn run() {
    let path = config_path();
    let config: NodeConfig = match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| toml::from_str(&t).map_err(|e| e.to_string())) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lyra-node: {} ({e}). Pair first: lyra-node pair <https://lyra…> [code] [--name NAME]", path.display());
            std::process::exit(1);
        }
    };
    tls_provider();
    let system = std::sync::Arc::new(lyra_system::System::new(config.system.clone(), expand));
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let mut pause = 1;
    loop {
        let started = std::time::Instant::now();
        match rt.block_on(session(&config, system.clone())) {
            End::Restart => {
                println!("updated; restarting");
                restart_in_place();
            }
            End::Removed => {
                // Last: stop the service (if any) so it doesn't come back.
                systemctl(&["stop", "--no-block", "lyra-node"]);
                std::process::exit(0);
            }
            End::Lost(e) => {
                eprintln!("lyra-node: {e}");
                if e.contains("pair it again") {
                    std::process::exit(1);
                }
            }
        }
        if started.elapsed() > Duration::from_secs(60) {
            pause = 1;
        }
        std::thread::sleep(Duration::from_secs(pause));
        pause = (pause * 2).min(60);
    }
}

/// Install (and with `enable`, start) the service that keeps the node running.
fn service(enable: bool) -> Result<String, String> {
    let binary = exe().display().to_string();
    let root = is_root();
    let args = if standalone() { String::new() } else { " node".into() };
    let unit = format!(
        "[Unit]\nDescription=lyra node (lets lyra work on this machine)\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\n{}ExecStart={binary}{args}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy={}\n",
        if root { "User=root\nEnvironment=HOME=/root\n" } else { "" },
        if root { "multi-user.target" } else { "default.target" }
    );
    let path = unit_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, unit).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    let scope = if root { "" } else { "--user " };
    if enable {
        systemctl(&["daemon-reload"]);
        if !systemctl(&["enable", "--now", "lyra-node"]) {
            return Err(format!("wrote {} but couldn't start it: systemctl {scope}enable --now lyra-node", path.display()));
        }
        let linger = if root { String::new() } else { "\nTo keep it running while you're logged out: loginctl enable-linger $USER".into() };
        return Ok(format!("lyra-node is running as a service ({}){linger}\nlogs: journalctl {scope}-u lyra-node -f", path.display()));
    }
    Ok(format!(
        "wrote {}\n  systemctl {scope}daemon-reload\n  systemctl {scope}enable --now lyra-node\nlogs: journalctl {scope}-u lyra-node -f",
        path.display()
    ))
}

pub const USAGE: &str = "\
lyra-node — let lyra (lyra serve) work on this machine

  lyra-node pair <url> [code] [--name NAME]   pair; without a code, approve the request in lyra
  lyra-node                                   connect and serve lyra's requests (Ctrl-C stops)
  lyra-node service [--enable]                install the service that keeps it running
  lyra-node version

Rules for this machine are in ~/.config/lyra/node.toml ([system]: allow_commands,
write_roots, deny_paths, timeouts). Reading runs at once; changes need your approval
in lyra; forbidden things never run. lyra can update or remove this node remotely.";

pub fn main(args: &[String]) {
    // Remember where this program is before anything can replace it.
    exe();
    match args.first().map(String::as_str) {
        Some("pair") => {
            let Some(url) = args.get(1) else {
                eprintln!("usage: lyra-node pair <https://lyra…> [code] [--name NAME]");
                std::process::exit(2);
            };
            let flag = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
            let name = flag("--name").unwrap_or_else(|| {
                let h = hostname();
                if h.is_empty() { "machine".into() } else { h.split('.').next().unwrap_or("machine").to_string() }
            });
            let code = args.get(2).filter(|c| !c.starts_with("--")).cloned();
            let token = match code {
                Some(code) => pair(url, &code, &name, "node"),
                None => request_pairing(url, &name, "node"),
            };
            match token {
                Ok(token) => {
                    let path = config_path();
                    let config = NodeConfig { url: url.trim_end_matches('/').to_string(), token, name: name.clone(), system: lyra_system::Settings::default() };
                    match toml::to_string_pretty(&config).map_err(|e| e.to_string()).and_then(|t| write_private(&path, &t)) {
                        Ok(()) => println!("paired as {name}; settings in {}\nnext: lyra-node service --enable (or just: lyra-node)", path.display()),
                        Err(e) => {
                            eprintln!("lyra-node: couldn't save {}: {e}", path.display());
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("lyra-node: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("service") => match service(args.iter().any(|a| a == "--enable" || a == "--now")) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("lyra-node: {e}");
                std::process::exit(1);
            }
        },
        Some("version" | "--version") => println!("lyra-node {VERSION} ({})", &own_hash()[..12.min(own_hash().len())]),
        Some("-h" | "--help" | "help") => println!("{USAGE}"),
        None => run(),
        Some(other) => {
            eprintln!("lyra-node: unknown {other:?}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system() -> lyra_system::System {
        lyra_system::System::new(lyra_system::Settings::default(), expand)
    }

    #[test]
    fn this_machine_decides_what_runs() {
        let s = system();
        let req = |kind: &str, command: &str, approved: bool| {
            json!({ "type": kind, "tool": "shell_run", "args": { "command": command, "cwd": "/tmp", "machine": "desktop" }, "approved": approved })
        };
        assert_eq!(handle(&s, "desktop", &req("check", "echo hi", false)).unwrap(), json!({ "check": "auto" }));
        let ask = handle(&s, "desktop", &req("check", "touch /tmp/lyra-node-x", false)).unwrap();
        assert_eq!(ask["check"], "ask");
        assert_eq!(ask["what"], "run a command on desktop", "the machine is named in the approval");
        assert_eq!(handle(&s, "desktop", &req("check", "rm -rf ~", false)).unwrap()["check"], "forbidden");

        assert_eq!(handle(&s, "desktop", &req("call", "echo hi", false)).unwrap()["stdout"], "hi");
        assert!(handle(&s, "desktop", &req("call", "touch /tmp/lyra-node-x", false)).unwrap_err().contains("approval"), "a change needs approval");
        assert!(handle(&s, "desktop", &req("call", "rm -rf ~", true)).unwrap_err().contains("refused on desktop"), "approval can't unlock forbidden");
        let file = json!({ "type": "check", "tool": "file_read", "args": { "path": "~/.ssh/id_ed25519" } });
        assert_eq!(handle(&s, "desktop", &file).unwrap()["check"], "forbidden", "keys stay off limits");
        assert_eq!(socket_url("https://lyra.example.com/", "node", "t"), "wss://lyra.example.com/node?token=t");
        assert_eq!(socket_url("http://127.0.0.1:8484", "ws", "t"), "ws://127.0.0.1:8484/ws?token=t");
        assert_eq!(expand("~"), home());
        assert_eq!(expand("~/x"), home().join("x"));
    }

    #[test]
    fn hello_says_who_and_which_build() {
        let h = hello();
        assert_eq!(h["type"], "hello");
        assert_eq!(h["version"], VERSION);
        // A test binary isn't the standalone lyra-node: no build to compare.
        assert_eq!(h["self_update"], false);
        assert_eq!(h["build"], "");
    }
}
