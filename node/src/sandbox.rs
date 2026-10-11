//! Disposable browser containers for lyra's agents: one container per task,
//! from an image this machine builds (`lyra-node sandbox setup`), removed
//! when the task ends, after a while idle, or on Stop. Each runs Chromium
//! under Playwright (`sandbox/driver.py`), spoken to in JSON lines on its
//! stdin and stdout, with CPU, memory and process limits, no saved state, a
//! read-only filesystem and no privileges, on its own network
//! (`lyra-sandbox`) that can't reach the LAN or this machine (firewall rules,
//! checked from inside a container before the sandbox counts as ready).
//!
//! What an action may do is decided in Rust before the browser does it
//! (`lyra_system::browser::check`, the same rule lyra applies before asking):
//! reading and moving about run, sending a form needs the person's yes,
//! payment details and file uploads never happen.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The image (bumped when the Dockerfile or the driver changes).
pub const IMAGE: &str = "lyra-sandbox:1";
const DOCKERFILE: &str = include_str!("../sandbox/Dockerfile");
const DRIVER: &str = include_str!("../sandbox/driver.py");
const NETWORK: &str = "lyra-sandbox";
const BRIDGE: &str = "br-lyra";
/// The firewall chain the network's traffic goes through.
const CHAIN: &str = "LYRA-SANDBOX";
/// Private and local addresses the containers may not reach (and, besides
/// these, every network this machine is on and its routers: a LAN can use
/// addresses that look public, such as 172.99.x.x).
const BLOCKED: [&str; 6] = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "169.254.0.0/16", "100.64.0.0/10", "127.0.0.0/8"];

/// `[sandbox]` (in node.toml, or lyra's config for the server's own).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Let the containers reach the LAN (for testing only: pages could then
    /// reach every machine here).
    pub allow_lan: bool,
    pub memory: String,
    pub cpus: String,
    /// A session nobody used for this long is closed.
    pub idle_minutes: u64,
    /// The most containers at once.
    pub max_sessions: usize,
    pub image: String,
    /// More networks the containers may not reach (inside ones behind the
    /// router that this machine isn't on, e.g. "10.20.0.0/16").
    pub block: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { allow_lan: false, memory: "2g".into(), cpus: "2".into(), idle_minutes: 10, max_sessions: 4, image: IMAGE.into(), block: Vec::new() }
    }
}

fn docker(args: &[&str]) -> Result<String, String> {
    let out = Command::new("docker").args(args).stdin(Stdio::null()).output().map_err(|e| format!("docker isn't here: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().lines().last().unwrap_or("docker failed").to_string())
    }
}

/// Whether containers here can't reach the LAN (checked at setup and at start).
static LAN_BLOCKED: Mutex<Option<bool>> = Mutex::new(None);

/// What this machine's sandbox can do now.
pub fn status(s: &Settings) -> Value {
    let version = docker(&["version", "--format", "{{.Server.Version}}"]);
    let image = version.is_ok() && docker(&["image", "inspect", "--format", "{{.Id}}", &s.image]).is_ok();
    let network = version.is_ok() && docker(&["network", "inspect", "--format", "{{.Id}}", NETWORK]).is_ok();
    let blocked = LAN_BLOCKED.lock().unwrap_or_else(|e| e.into_inner()).or_else(|| saved_check().map(|c| c.blocked));
    let lan_ok = s.allow_lan || blocked == Some(true);
    let why = match (&version, image, network, lan_ok) {
        (Err(e), ..) => e.clone(),
        (_, false, ..) => "the image isn't built yet: lyra-node sandbox setup".into(),
        (_, _, false, _) => "its network isn't made yet: lyra-node sandbox setup".into(),
        (_, _, _, false) if blocked.is_none() => "the LAN check hasn't run: lyra-node sandbox setup".into(),
        (_, _, _, false) => "containers could reach the LAN: run lyra-node sandbox setup as root to block it".into(),
        _ => String::new(),
    };
    json!({
        "docker": version.as_ref().ok(),
        "image": image.then(|| s.image.clone()),
        "network": network,
        "lan_blocked": blocked,
        "allow_lan": s.allow_lan,
        "ready": why.is_empty(),
        "why": why,
        "running": sessions().lock().unwrap_or_else(|e| e.into_inner()).len(),
    })
}

/// Build the image, make the network, block the LAN (as root, on Linux), and
/// check from inside a container that it can't reach it. What was done.
pub fn setup(s: &Settings) -> Result<String, String> {
    let version = docker(&["version", "--format", "{{.Server.Version}}"])?;
    let mut said = vec![format!("docker {version}")];
    let dir = std::env::temp_dir().join(format!("lyra-sandbox-build-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("Dockerfile"), DOCKERFILE).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("driver.py"), DRIVER).map_err(|e| e.to_string())?;
    let built = docker(&["build", "-q", "-t", &s.image, &dir.display().to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
    built?;
    said.push(format!("image {} built", s.image));
    if docker(&["network", "inspect", NETWORK]).is_err() {
        docker(&["network", "create", "--driver", "bridge", "-o", &format!("com.docker.network.bridge.name={BRIDGE}"), NETWORK])?;
        said.push(format!("network {NETWORK} made"));
    }
    match firewall(s) {
        Ok(nets) => said.push(format!("blocked from the containers: {}", nets.join(", "))),
        Err(e) => said.push(format!("couldn't block the LAN: {e}")),
    }
    let check = lan_check(s);
    for (target, how) in &check.seen {
        said.push(format!("  {target}: {how}"));
    }
    said.push(match check.blocked {
        true => "checked: a container can't reach this machine, its routers or its networks".into(),
        false if s.allow_lan => "LAN not blocked (allow_lan is on: for testing only)".into(),
        false => "a container could still reach the LAN (above): the sandbox stays off until it can't. Networks lyra doesn't know go in [sandbox] block in node.toml".into(),
    });
    Ok(said.join(" · "))
}

/// The networks this machine is directly on (`ip -4 route` lines with
/// `scope link`, not the sandbox's own) and the routers it uses.
fn local_nets(routes: &str) -> (Vec<String>, Vec<String>) {
    let mut nets = Vec::new();
    let mut routers = Vec::new();
    for line in routes.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.first() == Some(&"default") {
            if let Some(i) = words.iter().position(|w| *w == "via")
                && let Some(gw) = words.get(i + 1)
                && !routers.contains(&gw.to_string())
            {
                routers.push(gw.to_string());
            }
        } else if words.contains(&"link") && !words.contains(&BRIDGE) && words.first().is_some_and(|n| n.contains('/') || n.parse::<std::net::Ipv4Addr>().is_ok()) {
            let n = words[0].to_string();
            if !nets.contains(&n) {
                nets.push(n);
            }
        }
    }
    (nets, routers)
}

fn routes() -> String {
    Command::new("ip").args(["-4", "route", "show"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
}

/// Everything the containers may not reach: the private ranges, this
/// machine's networks and routers, and `[sandbox] block`.
fn blocked_nets(s: &Settings) -> Vec<String> {
    let (nets, routers) = local_nets(&routes());
    let mut all: Vec<String> = BLOCKED.iter().map(|n| n.to_string()).collect();
    for n in nets.into_iter().chain(routers.into_iter().map(|r| format!("{r}/32"))).chain(s.block.iter().map(|b| b.trim().to_string())) {
        if !n.is_empty() && !all.contains(&n) {
            all.push(n);
        }
    }
    all
}

fn ipt(args: &[&str]) -> Result<(), String> {
    let out = Command::new("iptables").args(args).stdin(Stdio::null()).output().map_err(|e| format!("iptables: {e}"))?;
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}

/// Firewall rules for the sandbox's bridge: nothing private, nothing this
/// machine is on, nothing on this machine. The networks blocked.
fn firewall(s: &Settings) -> Result<Vec<String>, String> {
    if !cfg!(target_os = "linux") {
        return Err("only on Linux".into());
    }
    let _ = ipt(&["-N", CHAIN]);
    ipt(&["-F", CHAIN]).map_err(|e| if e.contains("ermission") { "needs root".to_string() } else { e })?;
    let nets = blocked_nets(s);
    for net in &nets {
        ipt(&["-A", CHAIN, "-d", net, "-j", "DROP"]).map_err(|e| format!("blocking {net}: {e}"))?;
    }
    ipt(&["-A", CHAIN, "-j", "RETURN"])?;
    if ipt(&["-C", "DOCKER-USER", "-i", BRIDGE, "-j", CHAIN]).is_err() {
        ipt(&["-I", "DOCKER-USER", "-i", BRIDGE, "-j", CHAIN])?;
    }
    // This machine itself (its services, other containers' published ports).
    if ipt(&["-C", "INPUT", "-i", BRIDGE, "-j", "DROP"]).is_err() {
        ipt(&["-I", "INPUT", "-i", BRIDGE, "-j", "DROP"])?;
    }
    Ok(nets)
}

/// The rules are in place (a firewall reload drops them): put back if not.
fn firewall_in_place(s: &Settings) -> Result<(), String> {
    if s.allow_lan || (ipt(&["-C", "DOCKER-USER", "-i", BRIDGE, "-j", CHAIN]).is_ok() && ipt(&["-C", "INPUT", "-i", BRIDGE, "-j", "DROP"]).is_ok()) {
        return Ok(());
    }
    firewall(s).map(|_| ()).map_err(|e| format!("the sandbox's firewall rules are gone and couldn't be put back: {e}"))
}

/// What the LAN check found: blocked, and each address and how it answered.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LanCheck {
    pub blocked: bool,
    pub seen: Vec<(String, String)>,
    pub at: String,
}

fn check_path() -> Option<std::path::PathBuf> {
    crate::config_dir().map(|d| d.join("sandbox-check.json"))
}

/// The last check, kept on disk (so a `lyra-node sandbox setup` run counts for the service).
fn saved_check() -> Option<LanCheck> {
    serde_json::from_str(&std::fs::read_to_string(check_path()?).ok()?).ok()
}

/// From inside a container: this machine (the network's gateway), its
/// routers and an address on each network it's on must time out, not
/// answer (a refusal counts as reached). Kept for `status`.
fn lan_check(s: &Settings) -> LanCheck {
    let gateway = docker(&["network", "inspect", "--format", "{{range .IPAM.Config}}{{.Gateway}}{{end}}", NETWORK]).unwrap_or_default();
    let (nets, routers) = local_nets(&routes());
    let mut targets: Vec<String> = std::iter::once(gateway).chain(routers).filter(|t| !t.is_empty()).collect();
    // On each network this machine is on: its first address (often the router, else a neighbour).
    for n in &nets {
        if let Some(first) = n.split('/').next().and_then(|a| a.parse::<std::net::Ipv4Addr>().ok()).map(|a| std::net::Ipv4Addr::from(u32::from(a) + 1).to_string())
            && !targets.contains(&first)
        {
            targets.push(first);
        }
    }
    let probe = format!(
        "import socket\nfor h in {targets:?}:\n    s = socket.socket(); s.settimeout(3)\n    try:\n        s.connect((h, 53)); print('reached', h, 'answered')\n    except ConnectionRefusedError:\n        print('reached', h, 'refused (so it got there)')\n    except Exception as e:\n        print('blocked', h, type(e).__name__)\n"
    );
    let out = docker(&["run", "--rm", "--network", NETWORK, "--entrypoint", "python3", &s.image, "-c", &probe]);
    let mut seen: Vec<(String, String)> = Vec::new();
    match &out {
        Ok(text) => {
            for line in text.lines() {
                let mut w = line.splitn(3, ' ');
                let (how, host, why) = (w.next().unwrap_or(""), w.next().unwrap_or(""), w.next().unwrap_or(""));
                seen.push((host.to_string(), if how == "blocked" { format!("blocked ({why})") } else { format!("reached: {why}") }));
            }
        }
        Err(e) => seen.push(("the check".into(), format!("couldn't run: {e}"))),
    }
    let blocked = out.is_ok() && !targets.is_empty() && seen.len() == targets.len() && seen.iter().all(|(_, h)| h.starts_with("blocked"));
    let check = LanCheck { blocked, seen, at: chrono::Utc::now().to_rfc3339() };
    *LAN_BLOCKED.lock().unwrap_or_else(|e| e.into_inner()) = Some(blocked);
    if let Some(p) = check_path() {
        let _ = std::fs::write(p, serde_json::to_string_pretty(&check).unwrap_or_default());
    }
    check
}

/// At the node's start: the rules again (they don't survive a reboot), and the check.
pub fn prepare(s: &Settings) {
    if docker(&["image", "inspect", &s.image]).is_err() || docker(&["network", "inspect", NETWORK]).is_err() {
        return;
    }
    let _ = firewall(s);
    lan_check(s);
}

// ---- sessions

struct Session {
    child: Child,
    stdin: ChildStdin,
    answers: Receiver<String>,
    container: String,
    last: Instant,
    since: chrono::DateTime<chrono::Utc>,
    /// The last page's elements (to judge an action on one).
    elements: Vec<Value>,
    url: String,
    title: String,
}

fn sessions() -> &'static Mutex<HashMap<String, Session>> {
    static S: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn safe(key: &str) -> String {
    key.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(40).collect()
}

fn start(s: &Settings, key: &str) -> Result<Session, String> {
    // Unique enough: the key's start and the moment.
    let stamp = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64 ^ u64::from(std::process::id());
    let container = format!("lyra-sb-{}-{:x}", safe(key).chars().take(12).collect::<String>(), stamp & 0xff_ffff);
    let mut args: Vec<String> = [
        "run", "-i", "--rm", "--name", &container, "--network", NETWORK, "--memory", &s.memory, "--cpus", &s.cpus, "--pids-limit", "512", "--cap-drop", "ALL",
        "--security-opt", "no-new-privileges", "--read-only", "--tmpfs", "/tmp:rw,size=512m", "--tmpfs", "/home/pwuser:rw,size=256m,uid=1000", "--shm-size", "256m",
    ]
    .iter()
    .map(|a| a.to_string())
    .collect();
    if s.allow_lan {
        args.extend(["--add-host".to_string(), "host.docker.internal:host-gateway".to_string()]);
    }
    args.push(s.image.clone());
    let mut child = Command::new("docker").args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| format!("couldn't start a container: {e}"))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, answers) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut session = Session { child, stdin, answers, container, last: Instant::now(), since: chrono::Utc::now(), elements: Vec::new(), url: String::new(), title: String::new() };
    // The browser says it's ready (first start can take a moment).
    match session.answers.recv_timeout(Duration::from_secs(90)) {
        Ok(line) if line.contains("\"ready\"") => Ok(session),
        Ok(line) => {
            end(&mut session);
            Err(format!("the browser didn't start: {}", line.chars().take(200).collect::<String>()))
        }
        Err(_) => {
            end(&mut session);
            Err("the browser didn't start in time".into())
        }
    }
}

fn end(s: &mut Session) {
    let _ = writeln!(s.stdin, "{}", json!({ "action": "close" }));
    let _ = docker(&["rm", "-f", &s.container]);
    let _ = s.child.kill();
    let _ = s.child.wait();
}

/// Close sessions nobody used for `idle` (and say which).
pub fn reap(idle: Duration) -> Vec<String> {
    let mut all = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let old: Vec<String> = all.iter().filter(|(_, s)| s.last.elapsed() > idle).map(|(k, _)| k.clone()).collect();
    for k in &old {
        if let Some(mut s) = all.remove(k) {
            end(&mut s);
        }
    }
    old
}

/// Close a session (its container goes). Whether there was one.
pub fn stop(key: &str) -> bool {
    let taken = sessions().lock().unwrap_or_else(|e| e.into_inner()).remove(key);
    match taken {
        Some(mut s) => {
            end(&mut s);
            true
        }
        None => false,
    }
}

/// The browser containers running here (all of them, even ones a restart
/// left behind): name, how long, and with `stats` their CPU and memory now
/// (that takes a second or two), plus the session each belongs to.
pub fn containers(stats: bool) -> Vec<Value> {
    let Ok(out) = docker(&["ps", "--filter", "name=lyra-sb-", "--format", "{{json .}}"]) else { return Vec::new() };
    let mut list: Vec<Value> = out.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|c| c["Names"].as_str().is_some_and(|n| n.starts_with("lyra-sb-"))).collect();
    let usage: HashMap<String, Value> = if stats && !list.is_empty() {
        let names: Vec<String> = list.iter().filter_map(|c| c["Names"].as_str().map(str::to_string)).collect();
        let mut args = vec!["stats", "--no-stream", "--format", "{{json .}}"];
        args.extend(names.iter().map(String::as_str));
        docker(&args).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter_map(|v| Some((v["Name"].as_str()?.to_string(), v))).collect()
    } else {
        HashMap::new()
    };
    let all = sessions().lock().unwrap_or_else(|e| e.into_inner());
    for c in list.iter_mut() {
        let name = c["Names"].as_str().unwrap_or("").to_string();
        let session = all.iter().find(|(_, s)| s.container == name);
        let u = usage.get(&name);
        *c = json!({
            "name": name,
            "id": c["ID"],
            "running_for": c["RunningFor"],
            "status": c["Status"],
            "session": session.map(|(k, _)| k.clone()),
            "url": session.map(|(_, s)| s.url.clone()),
            "title": session.map(|(_, s)| s.title.clone()),
            "idle_seconds": session.map(|(_, s)| s.last.elapsed().as_secs()),
            "cpu": u.and_then(|u| u["CPUPerc"].as_str().map(str::to_string)),
            "memory": u.and_then(|u| u["MemUsage"].as_str().map(str::to_string)),
        });
    }
    list
}

/// Stop one container by its name (only lyra's: `lyra-sb-…`), and forget
/// its session. The session it was for, if any.
pub fn kill(name: &str) -> Result<Option<String>, String> {
    let name = name.trim();
    if !name.starts_with("lyra-sb-") || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("only lyra's browser containers (lyra-sb-…)".into());
    }
    let key = {
        let mut all = sessions().lock().unwrap_or_else(|e| e.into_inner());
        let key = all.iter().find(|(_, s)| s.container == name).map(|(k, _)| k.clone());
        if let Some(mut s) = key.as_ref().and_then(|k| all.remove(k)) {
            end(&mut s);
        }
        key
    };
    docker(&["rm", "-f", name]).map(|_| key)
}

/// Every open session: its key, page and since when.
pub fn list() -> Vec<Value> {
    sessions()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(k, s)| json!({ "key": k, "url": s.url, "title": s.title, "since": s.since.to_rfc3339(), "idle_seconds": s.last.elapsed().as_secs() }))
        .collect()
}

/// One action in the session `key` (started on its first action). The
/// action is judged first, here, whatever lyra decided: `approved` is the
/// person's yes for one that asks.
pub fn step(s: &Settings, key: &str, action: &str, args: &Value, approved: bool) -> Result<Value, String> {
    reap(Duration::from_secs(s.idle_minutes.max(1) * 60));
    let key = safe(key);
    if action == "close" {
        return Ok(json!({ "closed": stop(&key) }));
    }
    let element = {
        let all = sessions().lock().unwrap_or_else(|e| e.into_inner());
        let n = args["n"].as_u64();
        all.get(&key).and_then(|sess| n.and_then(|n| sess.elements.iter().find(|e| e["n"].as_u64() == Some(n)).cloned()))
    };
    match lyra_system::browser::check(action, element.as_ref(), approved) {
        lyra_system::Check::Forbidden(why) => return Err(why),
        lyra_system::Check::Ask { why, .. } => return Err(format!("needs the person's yes first: {why}")),
        lyra_system::Check::Auto => {}
    }
    if action == "open" {
        let url = args["url"].as_str().unwrap_or("");
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err("only http:// and https:// pages".into());
        }
    }
    // Its session: this one's, or a new container.
    let mut taken = sessions().lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
    if taken.is_none() {
        firewall_in_place(s)?;
        let st = status(s);
        if st["ready"] != true {
            return Err(format!("the sandbox isn't ready here: {}", st["why"].as_str().unwrap_or("")));
        }
        if sessions().lock().unwrap_or_else(|e| e.into_inner()).len() >= s.max_sessions.max(1) {
            return Err(format!("{} browser sessions are open already: try again when one ends", s.max_sessions));
        }
        taken = Some(start(s, &key)?);
    }
    let mut sess = taken.expect("a session");
    let answer = (|| {
        writeln!(sess.stdin, "{}", json!({ "action": action, "args": args })).map_err(|e| format!("the browser stopped: {e}"))?;
        let line = sess.answers.recv_timeout(Duration::from_secs(60)).map_err(|_| "the browser took too long".to_string())?;
        let v: Value = serde_json::from_str(&line).map_err(|_| "the browser said something odd".to_string())?;
        if v["ok"] == true { Ok(v["value"].clone()) } else { Err(v["error"].as_str().unwrap_or("the browser couldn't").to_string()) }
    })();
    // A browser that died with it: gone; otherwise kept for the next action.
    let alive = sess.child.try_wait().ok().flatten().is_none();
    if let Ok(v) = &answer {
        if let Some(list) = v["elements"].as_array() {
            sess.elements = list.clone();
        }
        if let Some(u) = v["url"].as_str() {
            sess.url = u.to_string();
        }
        if let Some(t) = v["title"].as_str() {
            sess.title = t.to_string();
        }
    }
    sess.last = Instant::now();
    if alive {
        sessions().lock().unwrap_or_else(|e| e.into_inner()).insert(key, sess);
    } else {
        end(&mut sess);
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `docker` that answers like the real one, and "runs" a container
    /// that answers like the driver (a form page: a field, a send button, a
    /// card field). PIDs of its "containers" go in `pids`.
    fn fake_docker() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lyra-fake-docker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let driver = r#"
import json, os, sys
open(os.environ["FAKE_PIDS"], "a").write(str(os.getpid()) + "\n")
print(json.dumps({"ok": True, "value": {"ready": True}}), flush=True)
page = {"url": "https://form.example/", "title": "Form", "text": "Write to us", "elements": [
    {"n": 1, "kind": "field", "label": "Name"},
    {"n": 2, "kind": "button", "label": "Send", "submits": True},
    {"n": 3, "kind": "field", "label": "Card number", "payment": True}]}
for line in sys.stdin:
    req = json.loads(line)
    if req["action"] == "close":
        break
    out = dict(page) if req["action"] in ("open", "read", "click", "submit") else {"filled": req["args"].get("n")}
    print(json.dumps({"ok": True, "value": out}), flush=True)
"#;
        std::fs::write(dir.join("driver.py"), driver).unwrap();
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  version) echo 29.0.0 ;;\n  image|network|rm) echo ok ;;\n  run) exec python3 -u {} ;;\n  ps) echo '{{\"Names\":\"lyra-sb-left-1\",\"ID\":\"abc123\",\"RunningFor\":\"20 minutes ago\",\"Status\":\"Up 20 minutes\"}}'; echo '{{\"Names\":\"someone-else\",\"ID\":\"def\"}}' ;;\n  stats) echo '{{\"Name\":\"lyra-sb-left-1\",\"CPUPerc\":\"1.5%\",\"MemUsage\":\"300MiB / 2GiB\"}}' ;;\n  *) exit 1 ;;\nesac\n",
            dir.join("driver.py").display()
        );
        let docker = dir.join("docker");
        std::fs::write(&docker, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    #[test]
    fn this_machines_networks_and_routers_are_blocked_too() {
        let routes = "default via 172.99.99.1 dev ens18 proto static metric 100\n\
default via 192.1.2.10 dev ens19 proto dhcp src 192.1.1.227 metric 101\n\
172.17.0.0/16 dev docker0 proto kernel scope link src 172.17.0.1 linkdown\n\
172.18.0.0/16 dev br-lyra proto kernel scope link src 172.18.0.1\n\
172.99.99.0/24 dev ens18 proto kernel scope link src 172.99.99.40 metric 100\n\
192.1.0.0/22 dev ens19 proto kernel scope link src 192.1.1.227 metric 101\n";
        let (nets, routers) = local_nets(routes);
        assert_eq!(nets, ["172.17.0.0/16", "172.99.99.0/24", "192.1.0.0/22"], "its own networks, not the sandbox's");
        assert_eq!(routers, ["172.99.99.1", "192.1.2.10"]);
    }

    #[test]
    fn a_session_opens_reads_asks_before_sending_never_takes_a_card_and_goes_when_stopped() {
        let dir = fake_docker();
        let pids = dir.join("pids");
        // SAFETY: tests that change the environment run here only.
        unsafe {
            std::env::set_var("PATH", format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default()));
            std::env::set_var("FAKE_PIDS", &pids);
        }
        let s = Settings { allow_lan: true, ..Default::default() };
        assert_eq!(status(&s)["ready"], true, "{}", status(&s));
        let page = step(&s, "conv-1", "open", &json!({ "url": "https://form.example/" }), false).unwrap();
        assert_eq!(page["title"], "Form");
        assert!(step(&s, "conv-1", "open", &json!({ "url": "file:///etc/passwd" }), false).unwrap_err().contains("http"));
        // Sending: only with the person's yes, and checked here whatever lyra said.
        assert!(step(&s, "conv-1", "click", &json!({ "n": 2 }), false).unwrap_err().contains("yes"));
        assert!(step(&s, "conv-1", "click", &json!({ "n": 2 }), true).is_ok());
        assert!(step(&s, "conv-1", "fill", &json!({ "n": 1, "text": "Dana" }), false).is_ok());
        assert!(step(&s, "conv-1", "fill", &json!({ "n": 3, "text": "4111" }), true).unwrap_err().contains("payment"));
        assert_eq!(list().len(), 1);
        // Stopped: the session and its "container" are gone.
        assert!(stop("conv-1"));
        assert!(list().is_empty() && !stop("conv-1"));
        std::thread::sleep(Duration::from_millis(300));
        let pid = std::fs::read_to_string(&pids).unwrap().lines().next().unwrap().trim().to_string();
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists() || std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|st| st.contains(") Z ")), "its process ended");
        // Every lyra container here (only lyra's), with its CPU and memory; and stopping one.
        let running = containers(true);
        assert_eq!(running.len(), 1, "{running:?}");
        assert_eq!((running[0]["name"].as_str(), running[0]["cpu"].as_str(), running[0]["session"].is_null()), (Some("lyra-sb-left-1"), Some("1.5%"), true), "a left-over one");
        assert_eq!(kill("lyra-sb-left-1"), Ok(None));
        assert!(kill("someone-else").is_err() && kill("lyra-sb-x; rm -rf /").is_err(), "only lyra's, by a plain name");
        // Idle: closed by itself.
        step(&s, "conv-2", "read", &json!({}), false).unwrap();
        assert_eq!(reap(Duration::ZERO), vec!["conv-2".to_string()]);
        assert!(list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
