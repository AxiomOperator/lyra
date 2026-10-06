//! `lyra node`: lend this machine to lyra. It connects *out* to `lyra serve`
//! (nothing listens here) and runs the Operator's system tools on this
//! machine — shell, files, system info — with this machine's own rules:
//! everything is checked here again (`lyra_system::System::check`), forbidden
//! things never run, and changes run only when the user approved them.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

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

pub fn config_dir() -> Option<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) if !d.is_empty() => Some(PathBuf::from(d).join("lyra")),
        _ => Some(PathBuf::from(std::env::var_os("HOME")?).join(".config/lyra")),
    }
}

/// Write a config file only its owner can read.
pub fn write_private(path: &std::path::Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path).map_err(|e| e.to_string())?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

/// Pair with `lyra serve` (a code from `lyra pair` there). Returns the token.
pub fn pair(url: &str, code: &str, name: &str, kind: &str) -> Result<String, String> {
    let url = url.trim_end_matches('/');
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
    let resp = client
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

/// `ws(s)://host/<path>?token=…` from lyra's https address.
pub fn socket_url(base: &str, path: &str, token: &str) -> String {
    let base = base.trim_end_matches('/');
    let ws = base.strip_prefix("https://").map(|h| format!("wss://{h}")).or_else(|| base.strip_prefix("http://").map(|h| format!("ws://{h}"))).unwrap_or_else(|| base.to_string());
    format!("{ws}/{path}?token={token}")
}

/// rustls needs one crypto provider picked for the process.
pub fn tls_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Answer one request from lyra, deciding everything here.
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

fn hello() -> Value {
    let os = std::fs::read_to_string("/etc/os-release")
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string()))
        .unwrap_or_default();
    let hostname = std::fs::read_to_string("/etc/hostname").map(|h| h.trim().to_string()).unwrap_or_default();
    json!({ "type": "hello", "hostname": hostname, "os": os, "user": std::env::var("USER").unwrap_or_default() })
}

/// One connection, until it drops.
async fn session(config: &NodeConfig, system: std::sync::Arc<lyra_system::System>) -> Result<(), String> {
    let url = socket_url(&config.url, "node", &config.token);
    let (ws, _) = tokio_tungstenite::connect_async(url.as_str()).await.map_err(|e| match e {
        tokio_tungstenite::tungstenite::Error::Http(r) if r.status().as_u16() == 401 => {
            "lyra doesn't know this machine any more: pair it again (lyra node pair …)".to_string()
        }
        e => e.to_string(),
    })?;
    let (mut sink, mut stream) = ws.split();
    sink.send(Message::Text(hello().to_string().into())).await.map_err(|e| e.to_string())?;
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut ping = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = ping.tick() => {
                sink.send(Message::Text(json!({ "type": "ping" }).to_string().into())).await.map_err(|e| e.to_string())?;
            }
            out = out_rx.recv() => {
                if let Some(text) = out {
                    sink.send(Message::Text(text.into())).await.map_err(|e| e.to_string())?;
                }
            }
            incoming = stream.next() => {
                let msg = match incoming {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => return Err(e.to_string()),
                    None => return Err("lyra closed the connection".into()),
                };
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else { continue };
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
                            let reply = match result {
                                Ok(value) => json!({ "type": "result", "id": v["id"], "ok": true, "value": value }),
                                Err(e) => json!({ "type": "result", "id": v["id"], "ok": false, "error": e }),
                            };
                            let _ = out.send(reply.to_string());
                        });
                    }
                    _ => {}
                }
            }
        }
    }
}

/// `lyra node`: stay connected, reconnecting with a growing pause.
fn run() {
    let path = config_dir().map(|d| d.join("node.toml")).expect("a home directory");
    let config: NodeConfig = match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| toml::from_str(&t).map_err(|e| e.to_string())) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lyra node: {} ({e}). Pair first: lyra node pair <https://lyra…> <code> [--name desktop]", path.display());
            std::process::exit(1);
        }
    };
    tls_provider();
    let system = std::sync::Arc::new(lyra_system::System::new(config.system.clone(), crate::config::expand_path));
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let mut pause = 1;
    loop {
        let started = std::time::Instant::now();
        match rt.block_on(session(&config, system.clone())) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("lyra node: {e}");
                if e.contains("pair it again") {
                    std::process::exit(1);
                }
            }
        }
        // A connection that lasted resets the pause.
        if started.elapsed() > Duration::from_secs(60) {
            pause = 1;
        }
        std::thread::sleep(Duration::from_secs(pause));
        pause = (pause * 2).min(60);
    }
}

pub const USAGE: &str = "\
lyra node — let lyra (lyra serve) work on this machine

  lyra node pair <url> <code> [--name desktop]   pair with a code from `lyra pair` on the server
  lyra node                                      connect and serve lyra's requests (Ctrl-C stops)
  lyra node service                              install a systemd user service that keeps it running

Rules for this machine are in ~/.config/lyra/node.toml ([system]: allow_commands,
write_roots, deny_paths, timeouts). Reading runs at once; changes need your approval
in lyra; forbidden things never run.";

pub fn main(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("pair") => {
            let (Some(url), Some(code)) = (args.get(1), args.get(2)) else {
                eprintln!("usage: lyra node pair <https://lyra…> <code> [--name desktop]");
                std::process::exit(2);
            };
            let name = args.iter().position(|a| a == "--name").and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| "desktop".into());
            tls_provider();
            match pair(url, code, &name, "node") {
                Ok(token) => {
                    let path = config_dir().map(|d| d.join("node.toml")).expect("a home directory");
                    let config = NodeConfig { url: url.trim_end_matches('/').to_string(), token, name: name.clone(), system: lyra_system::Settings::default() };
                    match toml::to_string_pretty(&config).map_err(|e| e.to_string()).and_then(|t| write_private(&path, &t)) {
                        Ok(()) => println!("paired as {name}; settings in {}\nnow: lyra node service (or just: lyra node)", path.display()),
                        Err(e) => {
                            eprintln!("lyra node: couldn't save {}: {e}", path.display());
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("lyra node: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("service") => {
            let binary = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "lyra".into());
            let Some(dir) = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/systemd/user")) else {
                eprintln!("lyra node: no home directory");
                std::process::exit(1);
            };
            let unit = format!(
                "[Unit]\nDescription=lyra node (lets lyra work on this machine)\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nExecStart={binary} node\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n"
            );
            let path = dir.join("lyra-node.service");
            if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, unit)) {
                eprintln!("lyra node: couldn't write {}: {e}", path.display());
                std::process::exit(1);
            }
            println!("wrote {}\n  systemctl --user daemon-reload\n  systemctl --user enable --now lyra-node\n  loginctl enable-linger $USER    # keep it running when you're logged out\nlogs: journalctl --user -u lyra-node -f", path.display());
        }
        Some("-h" | "--help" | "help") => println!("{USAGE}"),
        None => run(),
        Some(other) => {
            eprintln!("lyra node: unknown {other:?}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system() -> lyra_system::System {
        lyra_system::System::new(lyra_system::Settings::default(), crate::config::expand_path)
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
    }
}
