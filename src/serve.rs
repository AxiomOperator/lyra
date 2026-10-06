//! `lyra serve`: the same lyra (memory, skills, agents, plans, goals) with
//! no terminal, reached from the PWA over a WebSocket. The app state drives
//! both front ends; here it's mirrored to the devices as small updates
//! (a new message, text appended to one, the status line) and the devices'
//! messages and approvals come back in. Push notifications go out when
//! nobody has lyra open.

use std::time::{Duration, Instant};

use lyra_web::{Hub, Inbound, Notification};
use serde_json::{Value, json};

use crate::{App, Level, Message, StreamEvent};

/// A message as the PWA shows it.
fn web_message(app: &App, m: &Message) -> Value {
    let tools: Vec<String> = m
        .tool_calls
        .iter()
        .map(|c| format!("{} {}", c.function.name, c.function.arguments.chars().take(160).collect::<String>()))
        .collect();
    let calls: Vec<Value> = m.tool_calls.iter().map(|c| json!({ "id": c.id, "name": c.function.name, "arguments": c.function.arguments })).collect();
    json!({
        "role": m.role,
        "content": m.content,
        "reasoning": m.reasoning,
        "tools": tools,
        "calls": calls,
        "tool_call_id": m.tool_call_id,
        "stats": m.stats.as_ref().map(|s| crate::ui::reply_stats(s, &app.pricing)),
        "memories": m.memories,
        "skills": m.skills,
        "agents": m.agents,
    })
}

/// Other machines (`lyra node`) as `caps` sees them: through the hub.
pub struct HubRemote(pub Hub);

impl crate::caps::Remote for HubRemote {
    fn machines(&self) -> Vec<(String, bool)> {
        let online: Vec<String> = self.0.machines().into_iter().map(|m| m.name.to_lowercase()).collect();
        let mut all: Vec<(String, bool)> = self
            .0
            .devices()
            .list()
            .into_iter()
            .filter(|d| d.kind == "node")
            .map(|d| {
                let on = online.contains(&d.name.to_lowercase());
                (d.name, on)
            })
            .collect();
        all.sort();
        all
    }

    fn call(&self, machine: &str, request: Value, timeout: Duration) -> Result<Value, String> {
        self.0.call_machine(machine, request, timeout)
    }
}

/// The status line, pending approvals, agents and connected machines.
fn status(app: &App, machines: &[String]) -> Value {
    let active = app.agents.as_ref().map(|a| a.active.lock().map(|v| v.clone()).unwrap_or_default()).unwrap_or_default();
    json!({
        "phase": crate::ui::phase_text(app),
        "waiting": app.waiting,
        "model": app.model,
        "session": app.session_id,
        "title": app.messages.iter().find(|m| m.role == "user").map(|m| m.content.lines().next().unwrap_or("").chars().take(60).collect::<String>()),
        "approvals": app.approvals.iter().map(|r| json!({
            "id": r.id, "agent": r.agent, "what": r.what, "detail": r.detail, "why": r.why, "dangerous": r.dangerous,
        })).collect::<Vec<_>>(),
        "machines": machines,
        "agents": app.agents_panel.iter().map(|a| json!({ "title": a.title, "working": active.contains(&a.title), "enabled": a.enabled })).collect::<Vec<_>>(),
    })
}

/// What the devices were last sent, to send only what changed.
#[derive(Default)]
pub struct Mirror {
    sent: Vec<Value>,
    status: Value,
    session: String,
    /// Machines connected (`lyra node`), for the status.
    pub machines: Vec<String>,
    /// Devices online, pairing requests and machine details (from the hub).
    pub extra: Value,
}

impl Mirror {
    /// The updates that bring the devices up to date with the app.
    pub fn updates(&mut self, app: &mut App) -> Vec<Value> {
        let touched = app.touched.take();
        let mut out = Vec::new();
        if self.session != app.session_id {
            // Another conversation (resumed or new): send it whole.
            self.session = app.session_id.clone();
            self.sent = app.messages.iter().map(|m| web_message(app, m)).collect();
            out.push(json!({ "type": "reset", "messages": self.sent }));
        } else {
            if app.messages.len() < self.sent.len() {
                self.sent.truncate(app.messages.len());
                out.push(json!({ "type": "truncate", "length": app.messages.len() }));
            }
            // Only the last few messages ever change (the reply being written).
            // …and anything changed in place further back (an agent's card).
            let from = self.sent.len().saturating_sub(3).min(touched.unwrap_or(usize::MAX));
            for (i, m) in app.messages.iter().enumerate().skip(from) {
                let now = web_message(app, m);
                match self.sent.get(i) {
                    None => out.push(json!({ "type": "add", "index": i, "message": now })),
                    Some(before) if *before != now => out.push(change(i, before, &now)),
                    Some(_) => continue,
                }
                if i < self.sent.len() {
                    self.sent[i] = now;
                } else {
                    self.sent.push(now);
                }
            }
        }
        let mut s = status(app, &self.machines);
        if let (Some(map), Some(extra)) = (s.as_object_mut(), self.extra.as_object()) {
            map.extend(extra.clone());
        }
        if s != self.status {
            self.status = s.clone();
            out.push(json!({ "type": "status", "status": s }));
        }
        out
    }

    /// Everything, for a device that just connected (call after `updates`).
    pub fn snapshot(&self, seq: u64, app_version: &str) -> Value {
        let commands: Vec<Value> = crate::commands::all()
            .iter()
            .map(|e| json!({ "usage": e.usage, "description": e.description, "completion": crate::commands::completion(e) }))
            .collect();
        json!({ "type": "snapshot", "seq": seq, "messages": self.sent, "status": self.status, "commands": commands, "app_version": app_version, "session_id": self.session })
    }
}

/// Text added to the end of a message (a reply streaming in) is sent as just
/// the new text; anything else replaces the message.
fn change(i: usize, before: &Value, now: &Value) -> Value {
    let (old_c, new_c) = (before["content"].as_str().unwrap_or(""), now["content"].as_str().unwrap_or(""));
    let (old_r, new_r) = (before["reasoning"].as_str().unwrap_or(""), now["reasoning"].as_str().unwrap_or(""));
    let mut same_rest = now.clone();
    same_rest["content"] = before["content"].clone();
    same_rest["reasoning"] = before["reasoning"].clone();
    if same_rest == *before && new_c.starts_with(old_c) && new_r.starts_with(old_r) {
        json!({ "type": "append", "index": i, "text": &new_c[old_c.len()..], "reasoning": &new_r[old_r.len()..] })
    } else {
        json!({ "type": "replace", "index": i, "message": now })
    }
}

/// A short, plain preview of a reply for a notification.
fn preview(text: &str) -> String {
    let plain: String = text.replace(['*', '`', '#', '_'], "").split_whitespace().collect::<Vec<_>>().join(" ");
    if plain.chars().count() > 160 { format!("{}…", plain.chars().take(160).collect::<String>()) } else { plain }
}

/// One open conversation: its lyra and what its devices were last sent.
struct Conv {
    app: App,
    mirror: Mirror,
    printed: u64,
    changed: bool,
    last_status: Instant,
    /// Since when no device has shown it and it isn't answering.
    quiet_since: Instant,
}

impl Conv {
    fn new(app: App) -> Conv {
        let printed = app.logged;
        Conv { app, mirror: Mirror::default(), printed, changed: true, last_status: Instant::now(), quiet_since: Instant::now() }
    }
}

/// Unopened conversations nobody looks at are put away after this long.
const IDLE: Duration = Duration::from_secs(15 * 60);

/// The conversation for `session`: open already, or loaded from its saved
/// file; "" or an unknown one is the primary's.
fn conv_for(convs: &mut Vec<Conv>, session: &str) -> usize {
    if session.is_empty() {
        return 0;
    }
    if let Some(i) = convs.iter().position(|c| c.app.session_id == session) {
        return i;
    }
    let Some(saved) = crate::sessions::dir().and_then(|d| crate::sessions::find(&d, session).ok()) else { return 0 };
    if let Some(i) = convs.iter().position(|c| c.app.session_id == saved.id) {
        return i;
    }
    let mut app = convs[0].app.fork();
    app.resume_session(saved);
    convs.push(Conv::new(app));
    convs.len() - 1
}

/// Run lyra for the devices until the process is stopped: one lyra per open
/// conversation (each device shows one; several can answer at once), all
/// sharing memory, skills, agents and tools. The first one also runs lyra's
/// background work.
pub fn run(primary: App, hub: &Hub, inbound: std::sync::mpsc::Receiver<Inbound>, notify: bool) {
    // The lyra-node this server hands out (compared with machines' builds).
    let node_build = hub.node_build();
    let mut machines: Vec<String> = hub.machines().into_iter().map(|m| m.name).collect();
    let mut extra = hub_status(hub, node_build.as_deref());
    let mut last_extra = Instant::now();
    let started = Instant::now();
    let mut convs = vec![Conv::new(primary)];
    loop {
        let mut everyone = false;
        // What the devices ask for (waiting a little when there's nothing).
        let mut next = inbound.recv_timeout(Duration::from_millis(40)).ok();
        while let Some(msg) = next.take() {
            match msg {
                Inbound::Send { text, device, session, conn } => {
                    let i = conv_for(&mut convs, &session);
                    // A new conversation, or another one, for this device only.
                    if text == "/new" {
                        let app = convs[0].app.fork();
                        let id = app.session_id.clone();
                        convs.push(Conv::new(app));
                        hub.attach(conn, &id);
                        convs[0].app.log(Level::Info, format!("{device} started a new conversation"));
                    } else if let Some(key) = text.strip_prefix("/resume ").map(str::trim).filter(|k| !k.is_empty()) {
                        let before = convs.len();
                        let j = conv_for(&mut convs, key);
                        if j == 0 && convs.len() == before && !convs[0].app.session_id.starts_with(key) {
                            convs[i].app.messages.push(Message::new("error", format!("> {text}\nno saved conversation {key:?}")));
                            convs[i].changed = true;
                        } else {
                            let id = convs[j].app.session_id.clone();
                            hub.attach(conn, &id);
                        }
                    } else {
                        let c = &mut convs[i];
                        if c.app.waiting && !text.starts_with('/') && c.app.approvals.is_empty() {
                            c.app.messages.push(Message::new("info", "lyra is still answering here — send it again when the reply is done (or start a new conversation)".into()));
                        } else {
                            c.app.log(Level::Info, format!("from {device}: {}", text.chars().take(80).collect::<String>()));
                            c.app.input = text;
                            c.app.send();
                        }
                        c.changed = true;
                    }
                }
                Inbound::Stop { device, session } => {
                    let i = conv_for(&mut convs, &session);
                    convs[i].app.log(Level::Info, format!("{device} pressed stop"));
                    let _ = convs[i].app.stop();
                    convs[i].changed = true;
                }
                Inbound::Approve { id, answer, device } => {
                    if let Some(c) = convs.iter_mut().find(|c| c.app.approvals.iter().any(|r| r.id == id)) {
                        c.app.log(Level::Agent, format!("{device} answered approval {id}: {answer}"));
                        c.app.answer_approval_id(id, &answer);
                        c.changed = true;
                    }
                }
                Inbound::Note(text) => {
                    convs[0].app.log(Level::Info, text);
                    extra = hub_status(hub, node_build.as_deref());
                    everyone = true;
                }
                Inbound::DevicesChanged => {
                    extra = hub_status(hub, node_build.as_deref());
                    everyone = true;
                }
                Inbound::PairRequested(p) => {
                    let what = if p.kind == "node" { "machine" } else { "device" };
                    convs[0].app.log(Level::Agent, format!("🔗 {} ({}) asks to pair as a {what}, code {}: /devices approve {}", p.name, p.hostname, p.code, p.code));
                    extra = hub_status(hub, node_build.as_deref());
                    everyone = true;
                    if notify {
                        hub.notify(Notification {
                            title: format!("{} wants to pair", p.name),
                            body: format!("A {what} ({}) asks to pair. Check its code is {} and approve it in lyra.", p.hostname, p.code),
                            tag: format!("pair-{}", p.code),
                            approval: None,
                        });
                    }
                }
                Inbound::Get { what, arg: _, session, reply } => {
                    let loaded: Loaded = convs.iter().map(|c| (c.app.session_id.clone(), c.app.waiting)).collect();
                    let i = conv_for(&mut convs, &session);
                    let _ = reply.send(data(&mut convs[i].app, hub, &what, node_build.as_deref(), &loaded));
                }
                Inbound::MachinesChanged => {
                    let now: Vec<String> = hub.machines().into_iter().map(|m| m.name).collect();
                    for m in now.iter().filter(|m| !machines.contains(m)) {
                        convs[0].app.log(Level::Agent, format!("machine {m} connected: the Operator can work on it"));
                    }
                    for m in machines.iter().filter(|m| !now.contains(m)) {
                        convs[0].app.log(Level::Agent, format!("machine {m} disconnected"));
                    }
                    machines = now;
                    extra = hub_status(hub, node_build.as_deref());
                    everyone = true;
                    // The system tools' `machine` choices follow.
                    if let Some(caps) = convs[0].app.caps.clone() {
                        std::thread::spawn(move || {
                            caps.refresh();
                        });
                    }
                }
                Inbound::Health(reply) => {
                    let _ = reply.send(json!({
                        "version": env!("CARGO_PKG_VERSION"),
                        "uptime_seconds": started.elapsed().as_secs(),
                        "busy": convs.iter().any(|c| c.app.waiting),
                        "conversations": convs.len(),
                        "approvals_waiting": convs.iter().map(|c| c.app.approvals.len()).sum::<usize>(),
                    }));
                }
                Inbound::Snapshot { session, reply } => {
                    let i = conv_for(&mut convs, &session);
                    let c = &mut convs[i];
                    c.mirror.machines = machines.clone();
                    c.mirror.extra = extra.clone();
                    for u in c.mirror.updates(&mut c.app) {
                        hub.publish(Some(&c.app.session_id), u);
                    }
                    c.changed = false;
                    let _ = reply.send(c.mirror.snapshot(hub.seq(), hub.app_version()));
                }
            }
            next = inbound.try_recv().ok();
        }
        // Each conversation's own work: replies streaming, agents, plans…
        let many = convs.len() > 1;
        for c in convs.iter_mut() {
            while let Ok(event) = c.app.rx.try_recv() {
                let note = notification(&event);
                let done = matches!(event, StreamEvent::Done(_));
                c.app.handle(event);
                c.changed = true;
                if !notify || hub.someone_watching() {
                    continue;
                }
                // With several conversations, say which one.
                let about = |title: String| match c.app.messages.iter().find(|m| m.role == "user") {
                    Some(m) if many => format!("{title} · {}", m.content.lines().next().unwrap_or("").chars().take(40).collect::<String>()),
                    _ => title,
                };
                if done {
                    let body = c.app.messages.iter().rev().find(|m| m.role == "assistant").map(|m| preview(&m.content)).unwrap_or_default();
                    hub.notify(Notification { title: about("lyra replied".into()), body, tag: format!("reply-{}", c.app.session_id), approval: None });
                }
                if let Some(mut n) = note {
                    n.title = about(n.title);
                    hub.notify(n);
                }
            }
        }
        // lyra's background work, on the primary.
        {
            let p = &mut convs[0];
            if !p.app.waiting && p.app.last_schedule_check.elapsed() > Duration::from_secs(10 * 60) {
                p.app.scheduled();
                p.changed = true;
            }
            if !p.app.waiting && !p.app.plan_busy && p.app.goals_checked.elapsed() > p.app.goals_every {
                p.app.goals_tick();
                p.changed = true;
            }
        }
        // Pairing requests expire and devices come and go: refresh now and then.
        if last_extra.elapsed() > Duration::from_secs(10) {
            last_extra = Instant::now();
            let now = hub_status(hub, node_build.as_deref());
            if now != extra {
                extra = now;
                everyone = true;
            }
        }
        let attached = hub.attached_sessions();
        for c in convs.iter_mut() {
            // The phase timer ("thinking 4s") ticks while something is happening.
            if everyone || c.changed || (c.app.waiting && c.last_status.elapsed() > Duration::from_secs(1)) {
                c.last_status = Instant::now();
                c.changed = false;
                c.mirror.machines = machines.clone();
                c.mirror.extra = extra.clone();
                for u in c.mirror.updates(&mut c.app) {
                    hub.publish(Some(&c.app.session_id), u);
                }
            }
            // The activity log goes to stdout (the systemd journal).
            if c.app.logged > c.printed {
                let new = ((c.app.logged - c.printed) as usize).min(c.app.activity.len());
                for a in &c.app.activity[c.app.activity.len() - new..] {
                    println!("{} {}", a.time, a.text);
                }
                c.printed = c.app.logged;
            }
            if c.app.waiting || attached.contains(&c.app.session_id) {
                c.quiet_since = Instant::now();
            }
        }
        // Put away conversations nobody has looked at for a while (saved first).
        let mut i = 1;
        while i < convs.len() {
            if convs[i].quiet_since.elapsed() > IDLE {
                let mut c = convs.remove(i);
                c.app.save_session();
                println!("put away conversation {} (idle)", c.app.session_id);
            } else {
                i += 1;
            }
        }
    }
}

/// What the hub knows for the status: devices online, pairing requests,
/// and every paired machine with its version.
fn hub_status(hub: &Hub, node_build: Option<&str>) -> Value {
    json!({
        "online": hub.online_devices().into_iter().map(|(id, name)| json!({ "id": id, "name": name })).collect::<Vec<_>>(),
        "pairing": hub.pair_requests().iter().map(|p| json!({ "id": p.id, "code": p.code, "name": p.name, "kind": p.kind, "hostname": p.hostname, "os": p.os })).collect::<Vec<_>>(),
        "machines_detail": machines_detail(hub, node_build),
    })
}

/// Every paired machine: online or not, what it runs, and whether the
/// server has a newer `lyra-node` for it.
fn machines_detail(hub: &Hub, node_build: Option<&str>) -> Vec<Value> {
    let online = hub.machines();
    hub.devices()
        .list()
        .into_iter()
        .filter(|d| d.kind == "node")
        .map(|d| {
            let m = online.iter().find(|m| m.name.eq_ignore_ascii_case(&d.name));
            let update = m.is_some_and(|m| m.self_update && node_build.is_some_and(|b| b != m.build));
            json!({
                "name": d.name,
                "id": d.id,
                "online": m.is_some(),
                "hostname": m.map(|m| m.hostname.clone()),
                "os": m.map(|m| m.os.clone()),
                "user": m.map(|m| m.user.clone()),
                "version": m.map(|m| m.version.clone()),
                "build": m.map(|m| m.build.chars().take(12).collect::<String>()),
                "self_update": m.is_some_and(|m| m.self_update),
                "update_available": update,
                "last_seen": d.last_seen,
            })
        })
        .collect()
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::Info => "info",
        Level::Tool => "tool",
        Level::Learn => "learn",
        Level::Memory => "memory",
        Level::Plan => "plan",
        Level::Evolve => "evolve",
        Level::Agent => "agent",
        Level::Error => "error",
    }
}

/// The lists the web app's pages show.
/// Conversations loaded now: (session, answering).
type Loaded = Vec<(String, bool)>;

fn data(app: &mut App, hub: &Hub, what: &str, node_build: Option<&str>, loaded: &Loaded) -> Value {
    let text = |r: Result<String, String>| json!({ "text": r.unwrap_or_else(|e| e) });
    match what {
        "sessions" => {
            let all = crate::sessions::dir().map(|d| crate::sessions::list(&d)).unwrap_or_default();
            json!(all.iter().take(60).map(|s| json!({
                "id": s.id, "title": s.title, "turns": s.user_turns(), "updated": s.updated, "current": s.id == app.session_id,
                "open": loaded.iter().any(|(id, _)| *id == s.id), "answering": loaded.iter().any(|(id, w)| *id == s.id && *w),
            })).collect::<Vec<_>>())
        }
        "devices" => {
            let online: Vec<String> = hub.online_devices().into_iter().map(|(id, _)| id).collect();
            let machines: Vec<String> = hub.machines().into_iter().map(|m| m.name.to_lowercase()).collect();
            json!(hub.devices().list().into_iter().map(|d| json!({
                "id": d.id, "name": d.name, "kind": d.kind,
                "online": if d.kind == "node" { machines.contains(&d.name.to_lowercase()) } else { online.contains(&d.id) },
                "created": d.created, "last_seen": d.last_seen, "push": d.push.is_some(),
            })).collect::<Vec<_>>())
        }
        "machines" => json!(machines_detail(hub, node_build)),
        "activity" => json!(app.activity.iter().rev().take(200).map(|a| json!({ "time": a.time, "level": level_name(a.level), "text": a.text })).collect::<Vec<_>>()),
        "agents" => text(app.agents.as_deref().map(crate::agents::list).ok_or("agents are off".into())),
        "goals" => text(app.goals_command("/goals", "")),
        "skills" => text(app.learning.clone().ok_or("learning is off".to_string()).and_then(|l| l.describe())),
        "memory" => text(app.mem().ok_or("memory is off".to_string()).and_then(|m| m.command(""))),
        "about" => json!({
            "lyra": env!("CARGO_PKG_VERSION"),
            "app": hub.app_version(),
            "node_build": node_build.map(|b| b.chars().take(12).collect::<String>()),
            "model": app.model,
            "session": app.session_id,
            "devices": hub.devices().list().len(),
        }),
        other => json!({ "error": format!("nothing called {other:?}") }),
    }
}

impl App {
    /// `/machines [update|remove <name|all>]` (only with `lyra serve`).
    pub(crate) fn machines_command(&mut self, arg: &str) -> Result<String, String> {
        let hub = self.hub.clone().ok_or("machines connect to `lyra serve`; this lyra isn't serving")?;
        let node_build = hub.node_build();
        let (sub, rest) = arg.trim().split_once(' ').unwrap_or((arg.trim(), ""));
        let rest = rest.trim();
        let all = machines_detail(&hub, node_build.as_deref());
        match sub {
            "" | "list" => {
                if all.is_empty() {
                    return Ok("no machines yet. On a machine: curl -fsSL <lyra url>/install.sh | sh  (or lyra-node pair <url>)".into());
                }
                Ok(all
                    .iter()
                    .map(|m| {
                        format!(
                            "{} {} — {}{}{}{}",
                            if m["online"] == true { "●" } else { "○" },
                            m["name"].as_str().unwrap_or(""),
                            if m["online"] == true { "online" } else { "offline" },
                            m["hostname"].as_str().map_or(String::new(), |h| format!(" · {h}")),
                            m["version"].as_str().map_or(String::new(), |v| if m["self_update"] == true { format!(" · lyra-node {v} ({})", m["build"].as_str().unwrap_or("")) } else { format!(" · built into lyra {v}") }),
                            if m["update_available"] == true { " · update available (/machines update)" } else { "" },
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n\n/machines update <name|all> · /machines remove <name> · add one: curl -fsSL <lyra url>/install.sh | sh")
            }
            "update" | "remove" => {
                if rest.is_empty() {
                    return Err(format!("usage: /machines {sub} <name{}>", if sub == "update" { "|all" } else { "" }));
                }
                let targets: Vec<String> = all
                    .iter()
                    .filter(|m| (rest == "all" && sub == "update" && m["online"] == true) || m["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(rest)))
                    .filter_map(|m| m["name"].as_str().map(str::to_string))
                    .collect();
                if targets.is_empty() {
                    return Err(format!("no machine {rest:?} (/machines lists them)"));
                }
                if sub == "update" && node_build.is_none() {
                    return Err("this server has no lyra-node to hand out (build it next to lyra: see the README)".into());
                }
                let tx = self.tx.clone();
                for name in targets.clone() {
                    let (hub, tx) = (hub.clone(), tx.clone());
                    let sub = sub.to_string();
                    std::thread::spawn(move || {
                        let request = json!({ "type": if sub == "update" { "update" } else { "uninstall" } });
                        let result = hub.call_machine(&name, request, Duration::from_secs(180));
                        let note = match (sub.as_str(), result) {
                            ("update", Ok(v)) if v["updated"] == true => format!("✓ {name} updated ({} → {}); it restarted into the new version", v["from"].as_str().unwrap_or("?"), v["to"].as_str().unwrap_or("?")),
                            ("update", Ok(v)) => format!("{name}: {}", v["why"].as_str().unwrap_or("nothing to update")),
                            ("update", Err(e)) => format!("✗ {name} wasn't updated: {e}"),
                            (_, result) => {
                                let removed = hub.devices().remove(&name).is_ok();
                                match result {
                                    Ok(v) => {
                                        let files: Vec<String> = v["removed"]
                                            .as_array()
                                            .into_iter()
                                            .flatten()
                                            .filter_map(|x| x.as_str())
                                            .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
                                            .collect();
                                        format!(
                                            "✓ {name} removed: lyra-node uninstalled itself{}{}",
                                            if files.is_empty() { String::new() } else { format!(" ({})", files.join(", ")) },
                                            if removed { "" } else { "; its pairing was already gone" }
                                        )
                                    }
                                    Err(e) => format!("{name} unpaired{} — it couldn't uninstall itself ({e}); its files stay on that machine until removed there", if removed { "" } else { " (already)" }),
                                }
                            }
                        };
                        let _ = tx.send(crate::StreamEvent::Notice(note));
                    });
                }
                Ok(format!("{} {}…", if sub == "update" { "updating" } else { "removing" }, targets.join(", ")))
            }
            _ => Err("usage: /machines [update <name|all> | remove <name>]".into()),
        }
    }

    /// `/devices [approve|deny <code> | remove <name|id>]` (only with `lyra serve`).
    pub(crate) fn devices_command(&mut self, arg: &str) -> Result<String, String> {
        let hub = self.hub.clone().ok_or("devices pair with `lyra serve`; this lyra isn't serving")?;
        let (sub, rest) = arg.trim().split_once(' ').unwrap_or((arg.trim(), ""));
        let rest = rest.trim();
        match sub {
            "" | "list" => {
                let online: Vec<String> = hub.online_devices().into_iter().map(|(id, _)| id).collect();
                let machines: Vec<String> = hub.machines().into_iter().map(|m| m.name.to_lowercase()).collect();
                let mut out: Vec<String> = hub
                    .devices()
                    .list()
                    .into_iter()
                    .map(|d| {
                        let on = if d.kind == "node" { machines.contains(&d.name.to_lowercase()) } else { online.contains(&d.id) };
                        format!(
                            "{} {} ({}) — {} · last seen {}{}",
                            if on { "●" } else { "○" },
                            d.name,
                            if d.kind == "node" { "machine" } else { "device" },
                            if on { "online" } else { "offline" },
                            d.last_seen.with_timezone(&chrono::Local).format("%m-%d %H:%M"),
                            if d.push.is_some() { " · notifications on" } else { "" }
                        )
                    })
                    .collect();
                for p in hub.pair_requests() {
                    out.push(format!("? {} ({}, {}) asks to pair — code {}: /devices approve {} · /devices deny {}", p.name, p.hostname, p.kind, p.code, p.code, p.code));
                }
                if out.is_empty() {
                    out.push("no paired devices".into());
                }
                out.push(String::new());
                out.push("pair with a code: `lyra pair` on the server · headless: lyra-node pair <url> (then approve here)".into());
                Ok(out.join("\n"))
            }
            "approve" | "deny" => hub.answer_pair(rest, sub == "approve"),
            "remove" => {
                let d = hub.devices().remove(rest)?;
                Ok(format!("removed {} ({}); it has to pair again{}", d.name, d.id, if d.kind == "node" { " — to uninstall lyra-node too, use /machines remove while it's online" } else { "" }))
            }
            _ => Err("usage: /devices [approve <code> | deny <code> | remove <name|id>]".into()),
        }
    }
}

/// A notification an event deserves (approvals, failures), if any.
fn notification(event: &StreamEvent) -> Option<Notification> {
    match event {
        StreamEvent::Approval(r) => Some(Notification {
            title: format!("{} needs your OK", r.agent),
            body: format!("{}: {}{}", r.what, r.detail.lines().next().unwrap_or(""), if r.dangerous { format!(" — {}", r.why) } else { String::new() }),
            tag: format!("approval-{}", r.id),
            approval: Some(r.id),
        }),
        StreamEvent::Error(e) => Some(Notification { title: "lyra hit an error".into(), body: preview(e), tag: "error".into(), approval: None }),
        StreamEvent::PlanFinished(result) => Some(Notification {
            title: "Plan update".into(),
            body: match result {
                Ok(_) => "A plan stopped running — open lyra to see how it went.".into(),
                Err(e) => preview(e),
            },
            tag: "plan".into(),
            approval: None,
        }),
        _ => None,
    }
}

/// A systemd unit for running `lyra serve` all the time (a user service, or
/// a system one when lyra runs as root).
pub fn service_unit(binary: &str, system: bool) -> String {
    let install = if system { "multi-user.target" } else { "default.target" };
    let user = if system { "User=root\nEnvironment=HOME=/root\n" } else { "" };
    format!(
        "[Unit]\n\
         Description=lyra (web and phone access)\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         {user}ExecStart={binary} serve\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         # Environment=LYRA_HOME=%h/.lyra\n\
         \n\
         [Install]\n\
         WantedBy={install}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_text_is_sent_as_appends() {
        let before = json!({ "role": "assistant", "content": "Hel", "reasoning": "", "tools": [] });
        let now = json!({ "role": "assistant", "content": "Hello", "reasoning": "", "tools": [] });
        assert_eq!(change(3, &before, &now), json!({ "type": "append", "index": 3, "text": "lo", "reasoning": "" }));
        let footer = json!({ "role": "assistant", "content": "Hello", "reasoning": "", "tools": [], "stats": "ttft 1s" });
        assert_eq!(change(3, &now, &footer)["type"], "replace", "other fields changed");
        let rewritten = json!({ "role": "assistant", "content": "Bye", "reasoning": "", "tools": [] });
        assert_eq!(change(3, &now, &rewritten)["type"], "replace");
        assert_eq!(preview("**Bold** and `code`\n\nnext"), "Bold and code next");
        assert!(service_unit("/home/me/.cargo/bin/lyra", false).contains("ExecStart=/home/me/.cargo/bin/lyra serve"));
        let system = service_unit("/usr/local/bin/lyra", true);
        assert!(system.contains("User=root") && system.contains("WantedBy=multi-user.target"));
    }
}
