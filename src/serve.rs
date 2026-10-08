//! `lyra serve`: the same lyra (memory, skills, agents, plans, goals) with
//! no terminal, reached from the PWA over a WebSocket. The app state drives
//! both front ends; here it's mirrored to the devices as small updates
//! (a new message, text appended to one, the status line) and the devices'
//! messages and approvals come back in. Push notifications go out when
//! nobody has lyra open.

use std::time::{Duration, Instant};

use lyra_web::{Hub, Inbound, Notification, To, Who};
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
        // The numbers, for the app's context meter.
        "usage": m.stats.as_ref().map(|s| json!({
            "input": s.input, "cached": s.cached, "output": s.output, "ms": s.elapsed.as_millis() as u64, "estimated": s.estimated,
            "cost": app.pricing.cost(s.input, s.cached, s.output), "currency": app.pricing.currency,
        })),
        "memories": m.memories,
        "memory_notes": m.memories.iter().filter_map(|id| app.memory_texts.get(id).map(|t| json!({ "id": id, "text": t }))).collect::<Vec<_>>(),
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

    fn call_streaming(&self, machine: &str, request: Value, timeout: Duration, cancel: &std::sync::atomic::AtomicBool, progress: &dyn Fn(Value)) -> Result<Value, String> {
        self.0.call_machine_streaming(machine, request, timeout, cancel, progress)
    }

    fn windows_machines(&self) -> Vec<String> {
        self.0.machines().into_iter().filter(|m| m.os.to_lowercase().contains("windows")).map(|m| m.name).collect()
    }

    fn harnesses(&self) -> Vec<(String, Value)> {
        self.0.machines().into_iter().filter(|m| m.harnesses.as_object().is_some_and(|h| !h.is_empty())).map(|m| (m.name, m.harnesses)).collect()
    }

    fn folders(&self, user: &str) -> Vec<(String, lyra_web::Folder)> {
        self.0.folders(user)
    }

    fn call_folder(&self, user: &str, folder: &str, request: Value, timeout: Duration) -> Result<Value, String> {
        self.0.call_folder(user, folder, request, timeout)
    }

    fn upload_path(&self, id: &str) -> Option<std::path::PathBuf> {
        self.0.upload(id).map(|(_, path)| path)
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
        "title": title(app),
        "backup": crate::backup::last().map(|b| json!({ "name": b.name, "made": b.made.to_rfc3339(), "size": b.size })),
        "backing_up": crate::backup::running(),
        "decide": crate::decide::model().map(|model| {
            let (decided, to_chat, ms) = crate::decide::stats();
            json!({ "model": model, "decided": decided, "to_chat": to_chat, "ms": ms })
        }),
        "context_window": crate::stats::CONTEXT_WINDOW.load(std::sync::atomic::Ordering::Relaxed),
        "version": crate::changelog::version(),
        // Bug reports and feature requests with news for this person.
        "feedback_rev": crate::feedback::revision(),
        "feedback_news": crate::feedback::badge(&crate::feedback::Who { user: app.owner.clone(), name: String::new(), admin: app.admin }),
        // The plan this conversation is running (the app's plan card).
        "plan": app.current_plan.as_ref().map(|p| json!({
            "id": lyra_execution::short(p.id), "version": p.version, "status": p.status, "busy": app.plan_busy,
            "goal": app.current_goal.as_ref().map(|g| g.description.clone()),
            "steps": p.steps.iter().map(|s| json!({
                "key": s.key, "title": s.title, "description": s.description, "status": s.status,
                "needs_approval": s.needs_approval(), "error": s.last_error, "attempts": s.attempts,
            })).collect::<Vec<_>>(),
            "budget": lyra_execution::budget::describe(&p.budget, &p.usage),
            "note": p.note,
        })),
        "approvals": app.approvals.iter().map(|r| json!({
            "id": r.id, "agent": r.agent, "what": r.what, "detail": r.detail, "why": r.why, "dangerous": r.dangerous, "tool": r.tool,
        })).collect::<Vec<_>>(),
        "machines": machines,
        "groups": app.caps.as_ref().map(|c| {
            let mut g: Vec<Value> = c.groups.iter().map(|(name, members)| json!({ "name": name, "machines": members })).collect();
            g.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            g
        }),
        "agents": app.agents_panel.iter().map(|a| json!({ "title": a.title, "working": active.contains(&a.title), "enabled": a.enabled })).collect::<Vec<_>>(),
    })
    .as_object()
    .map(|m| {
        // A member's devices: nothing about machines, backups or the decision model.
        let keep = |k: &str| app.admin || !matches!(k, "groups" | "backup" | "backing_up" | "decide" | "machines");
        json!(m.iter().filter(|(k, _)| keep(k)).map(|(k, v)| (k.clone(), v.clone())).collect::<serde_json::Map<_, _>>())
    })
    .unwrap_or_default()
}

/// A machine's (or the server's) health report: new problems are logged and
/// pushed, cleared ones logged and pushed as fixed.
fn health_report(app: &mut App, hub: &Hub, alerts: &mut crate::health::Alerts, name: &str, h: &Value) {
    let change = alerts.report(name, crate::health::problems(h, &crate::health::settings()));
    if !change.new.is_empty() {
        alert(app, hub, name, &format!("{name}: {}", change.new.join("; ")), true);
    }
    if !change.cleared.is_empty() {
        alert(app, hub, name, &format!("{name} is fine again: {}", change.cleared.join("; ")), false);
    }
    // New problems are researched by themselves; cleared ones are marked so.
    let d = app.diagnose.clone();
    for (key, text) in &change.new_keys {
        // Nothing runs on another machine unless the user asks: theirs wait for "look into it".
        if d.enabled && d.auto && name.eq_ignore_ascii_case(crate::caps::HERE) {
            crate::diagnose::queue(&format!("{}:{key}", name.to_lowercase()), name, text, false);
        }
    }
    for key in &change.cleared_keys {
        crate::diagnose::resolve(&format!("{}:{key}", name.to_lowercase()));
    }
}

/// Log a health alert, and push it when `[health] notify` is on.
fn alert(app: &mut App, hub: &Hub, machine: &str, text: &str, problem: bool) {
    app.log(if problem { Level::Error } else { Level::Agent }, format!("{} {text}", if problem { "⚠" } else { "✓" }));
    if crate::health::settings().notify {
        hub.notify(Notification {
            title: if problem { format!("⚠ {machine} needs a look") } else { format!("✓ {machine}") },
            body: text.to_string(),
            tag: format!("health-{}", machine.to_lowercase()),
            approval: None,
            url: None, actions: vec![], reference: None,
        to: To::Admins,
        });
    }
}

/// A routine was asked to run now (checked every loop, cheaply).
fn routines_wanted() -> bool {
    crate::routines::has_requests()
}

/// " · coding: Claude Code 2.1.291, OpenCode 1.18.29" for a machine that has them.
fn coding_agents(m: &Value) -> String {
    let list: Vec<String> = m["harnesses"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(h, v)| {
            let name = if h == "claude" { "Claude Code" } else if h == "opencode" { "OpenCode" } else { h.as_str() };
            format!("{name} {}", v.as_str().unwrap_or("").split_whitespace().next().unwrap_or(""))
        })
        .collect();
    if list.is_empty() { String::new() } else { format!(" · coding: {}", list.join(", ")) }
}

/// A conversation's title: its first message's first line.
fn title(app: &App) -> Option<String> {
    app.messages.iter().find(|m| m.role == "user").map(|m| m.content.lines().next().unwrap_or("").chars().take(60).collect::<String>())
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

/// Text files lyra reads whole (up to this size).
const READ_UP_TO: u64 = 200 * 1024;

fn size_text(n: u64) -> String {
    match n {
        n if n >= 1024 * 1024 => format!("{:.1} MB", n as f64 / 1024.0 / 1024.0),
        n if n >= 1024 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} bytes"),
    }
}

/// What a message says about its attached files: a text file's content, or
/// what the file is (the Operator can put it on a machine); images go to a
/// model that can see.
pub fn attachments(hub: &Hub, files: &[String], vision: bool, who: &Who) -> (String, Vec<String>, Vec<(String, String, std::path::PathBuf)>) {
    use base64::Engine;
    let mut text = String::new();
    let mut images = Vec::new();
    // For the vision model, in the turn (not here: it takes a while).
    let mut looks = Vec::new();
    for id in files {
        // Someone else's upload isn't there, as far as this person knows.
        let Some((up, path)) = hub.upload(id).filter(|(up, _)| who.admin || up.user.as_deref().unwrap_or(lyra_web::users::OWNER) == who.user) else {
            text += &format!("\n\n(an attachment, {id}, couldn't be found)");
            continue;
        };
        let name = &up.name;
        let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
        let texty = up.mime.starts_with("text/")
            || ["json", "xml", "yaml", "x-yaml", "toml", "csv", "x-sh", "javascript"].iter().any(|t| up.mime.ends_with(t))
            || ["txt", "md", "log", "conf", "cfg", "ini", "toml", "yaml", "yml", "json", "csv", "sh", "py", "rs", "js", "ts", "sql", "xml", "html", "css", "env"].contains(&ext.as_str());
        let head = format!("**Attached: {name}** ({}, {}, upload `{}`)", up.mime, size_text(up.size), up.id);
        if texty && up.size <= READ_UP_TO
            && let Ok(content) = std::fs::read_to_string(&path)
        {
            let lang = if ext.len() <= 5 { ext.as_str() } else { "" };
            text += &format!("\n\n{head}\n```{lang}\n{}\n```", content.trim_end());
        } else if (up.mime == "application/pdf" || ext == "pdf") && up.size <= 20 * 1024 * 1024
            && let Some(content) = std::fs::read(&path).ok().and_then(|b| crate::vision::pdf_text(&b))
        {
            text += &format!("\n\n{head}\n```\n{}\n```", content.chars().take(40_000).collect::<String>().trim_end());
        } else if up.mime.starts_with("image/") && vision {
            if let Ok(bytes) = std::fs::read(&path) {
                images.push(format!("data:{};base64,{}", up.mime, base64::engine::general_purpose::STANDARD.encode(bytes)));
            }
            text += &format!("\n\n{head} — the image is attached.");
        } else if crate::vision::available() && (up.mime.starts_with("image/") && crate::vision::image_type(name).is_some() || ext == "pdf") && up.size <= 25 * 1024 * 1024 {
            // A picture for a chat model that can't see, or a scanned PDF.
            text += &format!("\n\n{head}");
            looks.push((name.clone(), up.mime.clone(), path.clone()));
        } else {
            text += &format!(
                "\n\n{head} — you can't see its contents{}; the Operator can put it on a machine (upload_place).",
                if up.mime.starts_with("image/") { " (this model has no vision)" } else { "" }
            );
        }
    }
    (text, images, looks)
}

/// One person's PMI, as lyra serve follows it.
struct PmiUser {
    state: crate::pmi::State,
    view: Value,
    busy: bool,
    due: Option<Instant>,
    last: Instant,
    nags: crate::pmi::Nags,
}

impl Default for PmiUser {
    fn default() -> Self {
        Self { state: Default::default(), view: Value::Null, busy: false, due: None, last: Instant::now(), nags: Default::default() }
    }
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
    /// A routine running here, and since when.
    routine: Option<(crate::routines::Routine, Instant)>,
    /// A diagnosis running here (its key).
    diagnosis: Option<String>,
}

impl Conv {
    fn new(app: App) -> Conv {
        let printed = app.logged;
        Conv { app, mirror: Mirror::default(), printed, changed: true, last_status: Instant::now(), quiet_since: Instant::now(), routine: None, diagnosis: None }
    }
}

/// Unopened conversations nobody looks at are put away after this long.
const IDLE: Duration = Duration::from_secs(15 * 60);

/// A person's conversations act with the role they have now (an admin made
/// a member loses admin rights at once, also where they were already talking).
fn sync_role(convs: &mut [Conv], who: &Who) {
    for c in convs.iter_mut().filter(|c| c.app.owner == who.user) {
        c.app.admin = who.admin;
    }
}

/// What a conversation's devices are sent: their own conversations only, and
/// for a member nothing about machines, devices, the server's health or data
/// that's still the owner's alone.
fn for_viewer(extra: &Value, app: &App) -> Value {
    let mut v = extra.clone();
    if let Some(list) = v["conversations"].as_array() {
        v["conversations"] = json!(list.iter().filter(|c| c["owner"].as_str() == Some(app.owner.as_str())).cloned().collect::<Vec<_>>());
    }
    if app.admin {
        return v;
    }
    if let Some(map) = v.as_object_mut() {
        for key in ["machines_detail", "pairing", "online", "server_health", "server_harnesses", "diagnoses", "users_waiting"] {
            map.remove(key);
        }
    }
    if let Some(rows) = v["status"]["rows"].as_array() {
        v["status"]["rows"] = json!(rows.iter().filter(|r| r["group"] != "Machines").cloned().collect::<Vec<_>>());
    }
    v
}

/// One of `who`'s conversations for `session`: open already, or loaded from
/// their saved file. Nobody opens anyone else's.
fn find_conv(convs: &mut Vec<Conv>, session: &str, who: &Who) -> Option<usize> {
    let session = session.trim();
    if session.is_empty() {
        return None;
    }
    let mine = |c: &Conv| c.app.owner == who.user;
    if let Some(i) = convs.iter().position(|c| mine(c) && c.app.session_id == session) {
        return Some(i);
    }
    let saved = crate::sessions::dir().and_then(|d| crate::sessions::find_for(&d, session, &who.user).ok())?;
    if let Some(i) = convs.iter().position(|c| mine(c) && c.app.session_id == saved.id) {
        return Some(i);
    }
    let mut app = convs[0].app.fork_for(who);
    app.resume_session(saved);
    convs.push(Conv::new(app));
    Some(convs.len() - 1)
}

/// The conversation for `session`, else the user's own default: the primary
/// for its owner, a member's latest open one, or a new one of theirs.
fn conv_for(convs: &mut Vec<Conv>, session: &str, who: &Who) -> usize {
    if let Some(i) = find_conv(convs, session, who) {
        return i;
    }
    if convs[0].app.owner == who.user {
        return 0;
    }
    if let Some(i) = convs.iter().rposition(|c| c.app.owner == who.user && c.routine.is_none() && c.diagnosis.is_none()) {
        return i;
    }
    convs.push(Conv::new(convs[0].app.fork_for(who)));
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
    let mut last_open = Value::Null;
    // Machine health: what's been reported, and the server's own checkups.
    let mut alerts = crate::health::Alerts::load();
    // Status: everything lyra depends on, checked off this loop every minute.
    let serving_since = Instant::now();
    let (status_tx, status_rx) = crate::status::worker(crate::config::home().unwrap_or_default().join("status"));
    let mut status_alerts = crate::status::Alerts::load();
    let mut status_view = Value::Null;
    let mut status_busy = false;
    let mut last_status: Option<Instant> = None;
    // The coding agents installed here (the server's card shows them like a machine's).
    let mut server_harnesses = lyra_node::coding::available();
    let (health_tx, health_rx) = std::sync::mpsc::channel::<Value>();
    let mut server_health = Value::Null;
    let mut last_checkup: Option<Instant> = None;
    // Routines: when they were last looked at, and finished runs coming back.
    let mut last_routines = Instant::now() - Duration::from_secs(60);
    let (routine_tx, routine_rx) = std::sync::mpsc::channel::<(String, crate::routines::Routine, crate::routines::Run)>();
    // Each person's routines, for their devices.
    let mut routines_views: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    // Diagnoses: when they were last looked at, and what the panels show.
    crate::diagnose::requeue_running();
    let mut last_diag = Instant::now() - Duration::from_secs(60);
    let mut diag_view = Value::Null;
    // The daily briefing: the last one (it survives restarts), when the next is due.
    let (brief_tx, brief_rx) = std::sync::mpsc::channel::<(String, crate::briefing::Briefing)>();
    let mut last_brief = crate::briefing::last();
    // Each person's latest briefing, for their devices (members get theirs: their tasks).
    let mut brief_views: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    if let Some(b) = &last_brief {
        brief_views.insert(lyra_web::users::OWNER.to_string(), json!(b));
    }
    let mut brief_busy = false;
    let mut last_brief_check = Instant::now() - Duration::from_secs(60);
    // PMI: its live events (a thread), and the view read after each change.
    let (pmi_events_tx, pmi_events) = std::sync::mpsc::channel::<(String, crate::pmi::Event)>();
    let (pmi_tx, pmi_rx) = std::sync::mpsc::channel::<(String, Result<crate::pmi::State, String>)>();
    // Each person with a PMI token: their live view, followed while lyra runs.
    let mut pmi: std::collections::HashMap<String, PmiUser> = std::collections::HashMap::new();
    let mut last_pmi_scan = Instant::now() - Duration::from_secs(120);
    let mut last_nag = Instant::now();
    // Plan my day: re-planned every 15 minutes in each person's working day;
    // what changed is told when they aren't in quiet time (held until then).
    let (plan_tx, plan_rx) = std::sync::mpsc::channel::<(String, Result<Vec<String>, String>)>();
    let mut last_plan = Instant::now() - Duration::from_secs(3600);
    let mut planning: std::collections::HashSet<String> = Default::default();
    let mut held: std::collections::HashMap<String, Vec<String>> = Default::default();
    let mut quiet_seen: std::collections::HashMap<String, (bool, Instant)> = Default::default();
    let mut last_held = Instant::now();
    // Proactive help: a pass every 5 minutes in the working day (mail every other one).
    let (pro_tx, pro_rx) = std::sync::mpsc::channel::<(String, crate::proactive::Pass)>();
    let mut last_pro = Instant::now() - Duration::from_secs(3600);
    let mut pro_round: u64 = 0;
    let mut pro_busy: std::collections::HashSet<String> = Default::default();
    let (action_tx, action_rx) = std::sync::mpsc::channel::<(String, String, String, Result<String, String>)>();
    // The end-of-day recap: looked for once a minute, made once a day per person.
    let (recap_tx, recap_rx) = std::sync::mpsc::channel::<(String, crate::recap::Recap)>();
    let mut last_recap_look = Instant::now() - Duration::from_secs(120);
    let mut recapping: std::collections::HashSet<String> = Default::default();
    // "Tell me when …": each person's watches, looked at every two minutes.
    let (watch_tx, watch_rx) = std::sync::mpsc::channel::<(String, Vec<(String, String)>)>();
    let mut last_watch = Instant::now();
    let mut watching: std::collections::HashSet<String> = Default::default();
    let started = Instant::now();
    let mut convs = vec![Conv::new(primary)];
    loop {
        let mut everyone = false;
        // What the devices ask for (waiting a little when there's nothing).
        let mut next = inbound.recv_timeout(Duration::from_millis(40)).ok();
        while let Some(msg) = next.take() {
            match msg {
                Inbound::Send { text, device, who, session, conn, files } => {
                    sync_role(&mut convs, &who);
                    let i = conv_for(&mut convs, &session, &who);
                    // Attached files: described (or read) in the message itself.
                    let (about, images, looks) = attachments(hub, &files, convs[i].app.vision, &who);
                    let text = format!("{text}{about}").trim().to_string();
                    convs[i].app.attach_images = images;
                    convs[i].app.attach_looks = looks;
                    // A new conversation, or another one, for this device only.
                    if text == "/new" {
                        let app = convs[0].app.fork_for(&who);
                        let id = app.session_id.clone();
                        convs.push(Conv::new(app));
                        hub.attach(conn, &id);
                        convs[0].app.log(Level::Info, format!("{device} ({}) started a new conversation", who.name));
                    } else if let Some(key) = text.strip_prefix("/resume ").map(str::trim).filter(|k| !k.is_empty()) {
                        let open = convs.iter().position(|c| c.app.owner == who.user && c.app.session_id.starts_with(key));
                        match open.or_else(|| find_conv(&mut convs, key, &who)) {
                            Some(j) => {
                                let id = convs[j].app.session_id.clone();
                                hub.attach(conn, &id);
                            }
                            None => {
                                convs[i].app.messages.push(Message::new("error", format!("> {text}\nno saved conversation {key:?}")));
                                convs[i].changed = true;
                            }
                        }
                    } else {
                        let c = &mut convs[i];
                        if c.app.waiting && !text.starts_with('/') && c.app.approvals.is_empty() {
                            c.app.messages.push(Message::new("info", "lyra is still answering here — send it again when the reply is done (or start a new conversation)".into()));
                        } else {
                            c.app.log(Level::Info, format!("from {device}: {}", crate::shown(&text).chars().take(80).collect::<String>()));
                            c.app.input = text;
                            c.app.send();
                        }
                        c.changed = true;
                    }
                }
                Inbound::Stop { device, who, session } => {
                    sync_role(&mut convs, &who);
                    let i = conv_for(&mut convs, &session, &who);
                    convs[i].app.log(Level::Info, format!("{device} pressed stop"));
                    let _ = convs[i].app.stop();
                    convs[i].changed = true;
                }
                Inbound::Approve { id, answer, device, who } => {
                    sync_role(&mut convs, &who);
                    // Only in the user's own conversations.
                    if let Some(c) = convs.iter_mut().find(|c| c.app.owner == who.user && c.app.approvals.iter().any(|r| r.id == id)) {
                        c.app.log(Level::Agent, format!("{device} answered approval {id}: {answer}"));
                        c.app.answer_approval_id(id, &answer);
                        c.changed = true;
                    }
                }
                Inbound::Action { action, reference, device, who } => {
                    if who.user == convs[0].app.owner {
                        convs[0].app.log(Level::Info, format!("{device} pressed {action} on a reminder"));
                    }
                    let tx = action_tx.clone();
                    std::thread::spawn(move || {
                        // In their own PMI account.
                        let r = crate::pmi::as_user(&who.user, || crate::pmi::push_action(&action, &reference));
                        let _ = tx.send((who.user, action, reference, r));
                    });
                }
                Inbound::Note(text) => {
                    convs[0].app.log(Level::Info, text);
                    extra = hub_status(hub, node_build.as_deref());
                    everyone = true;
                }
                Inbound::SignIn { name, email } => {
                    convs[0].app.log(Level::Agent, format!("👤 {name} <{email}> signed in with Microsoft and waits to be let in: /users approve {email}"));
                    if notify {
                        hub.notify(Notification {
                            title: format!("{name} wants to use lyra"),
                            body: format!("{email} signed in with Microsoft. Let them in on the Users page, or /users approve {email}."),
                            tag: format!("signin-{email}"),
                            approval: None,
                            url: None, actions: vec![], reference: None,
                        to: To::Admins,
                        });
                    }
                    everyone = true;
                }
                Inbound::Connected { user, service, token, scope } => {
                    crate::calendar::forget_access(&user);
                    match crate::secrets::set_token_for(&service, &token, &user).and_then(|_| crate::calendar::keep_scope(&user, &scope)) {
                        Ok(()) if user == convs[0].app.owner => convs[0].app.log(Level::Info, format!("your Outlook is connected ({})", crate::calendar::granted_text(&user))),
                        Ok(()) => {}
                        Err(e) => convs[0].app.log(Level::Error, format!("couldn't keep a calendar connection: {e}")),
                    }
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
                            url: None, actions: vec![], reference: None,
                        to: To::Admins,
                        });
                    }
                }
                Inbound::Get { what, arg, session, who, reply } => {
                    sync_role(&mut convs, &who);
                    // Pages a member may open; the rest are admins' (or still the owner's data).
                    if !who.admin && !matches!(what.as_str(), "sessions" | "search" | "status" | "agents" | "skills" | "activity" | "about" | "models" | "do" | "pmi" | "routines" | "goals" | "memory" | "calendar" | "mail" | "notes" | "usage" | "recap" | "watches" | "everything" | "changelog" | "feedback" | "feedback_submit" | "feedback_comment" | "feedback_update" | "feedback_seen" | "feedback_analyze" | "feedback_enhance") {
                        let _ = reply.send(json!({ "error": "that's for admins" }));
                        continue;
                    }
                    let loaded: Loaded = convs.iter().filter(|c| c.app.owner == who.user).map(|c| (c.app.session_id.clone(), c.app.waiting)).collect();
                    let i = conv_for(&mut convs, &session, &who);
                    let machine = arg["machine"].as_str().unwrap_or("server").to_string();
                    match what.as_str() {
                        // A machine's rules: it answers (and checks changes) itself, off this loop.
                        "rules" | "set_rules" if machine != "server" => {
                            let hub = hub.clone();
                            let mut request = json!({ "type": "rules" });
                            if what == "set_rules" {
                                request["set"] = arg["system"].clone();
                                convs[0].app.log(Level::Agent, format!("rules for {machine} changed from the app"));
                            }
                            std::thread::spawn(move || {
                                let answer = hub.call_machine(&machine, request, Duration::from_secs(20));
                                let _ = reply.send(answer.unwrap_or_else(|e| json!({ "error": e })));
                            });
                        }
                        // A coding job's changes: `git diff` in its folder, read-only, on its machine.
                        "coding_diff" => {
                            let (dir, caps) = (arg["dir"].as_str().unwrap_or("").to_string(), convs[0].app.caps.clone());
                            std::thread::spawn(move || {
                                let shell = json!({ "command": "git diff --stat HEAD~0 && git diff && git status --short", "cwd": dir });
                                let answer = if machine == crate::caps::HERE {
                                    caps.as_ref().and_then(|c| c.system.as_ref()).ok_or("system access is off".to_string()).and_then(|s| s.call("shell_run", &shell))
                                } else {
                                    caps.as_ref().and_then(|c| c.remote()).ok_or("no machines".to_string()).and_then(|r| r.call(&machine, json!({ "type": "call", "tool": "shell_run", "args": shell, "approved": false }), Duration::from_secs(30)))
                                };
                                let _ = reply.send(answer.map(|v| json!({ "diff": v["stdout"], "error": v["error"] })).unwrap_or_else(|e| json!({ "error": e })));
                            });
                        }
                        "set_rules" => {
                            let answer = set_server_rules(&mut convs[0].app, &arg["system"]);
                            let _ = reply.send(answer.unwrap_or_else(|e| json!({ "error": e })));
                        }
                        // A clearer draft for the feedback form: off this loop (the model takes a moment).
                        "feedback_enhance" => {
                            let (kind, title, details, user) = (arg["kind"].as_str().unwrap_or("bug").to_string(), arg["title"].as_str().unwrap_or("").to_string(), arg["details"].as_str().unwrap_or("").to_string(), convs[i].app.owner.clone());
                            std::thread::spawn(move || {
                                let answer = crate::acting::run(&user, || crate::feedback::enhance(&kind, &title, &details));
                                let _ = reply.send(answer.map_or_else(|e| json!({ "error": e }), |text| json!({ "text": text })));
                            });
                        }
                        // One search for everything (Ctrl-K): off this loop, their own things only.
                        "everything" => {
                            let (query, user) = (arg["query"].as_str().unwrap_or("").to_string(), convs[i].app.owner.clone());
                            let mem = convs[i].app.tools.as_ref().map(|t| t.mem.clone());
                            std::thread::spawn(move || {
                                let _ = reply.send(crate::search::everything(&query, &user, mem));
                            });
                        }
                        "models" => {
                            let (url, current) = (convs[i].app.base_url.clone(), convs[i].app.model.clone());
                            std::thread::spawn(move || {
                                let answer = match crate::models(&url) {
                                    Ok(list) => json!({ "current": current, "models": list }),
                                    Err(e) => json!({ "current": current, "models": [], "error": e }),
                                };
                                let _ = reply.send(answer);
                            });
                        }
                        // A page's button: memory, skills, goals, model commands, answered to the page.
                        "do" => {
                            let line = arg["command"].as_str().unwrap_or("").trim().to_string();
                            let result = convs[i].app.quiet_command(&line);
                            if result.is_ok() && line.starts_with("/model ") {
                                let model = convs[i].app.model.clone();
                                for c in convs.iter_mut() {
                                    c.app.model = model.clone();
                                }
                            }
                            for c in convs.iter_mut() {
                                c.changed = true;
                            }
                            let _ = reply.send(match result {
                                Ok(text) => json!({ "ok": true, "text": text }),
                                Err(e) => json!({ "ok": false, "text": e }),
                            });
                        }
                        _ => {
                            let _ = reply.send(data(&mut convs[i].app, hub, &what, &arg, node_build.as_deref(), &loaded));
                        }
                    }
                }
                Inbound::MachineHealth { name, health } => {
                    health_report(&mut convs[0].app, hub, &mut alerts, &name, &health);
                    extra = hub_status(hub, node_build.as_deref());
                    everyone = true;
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
                Inbound::Snapshot { session, who, reply } => {
                    sync_role(&mut convs, &who);
                    let i = conv_for(&mut convs, &session, &who);
                    let c = &mut convs[i];
                    let mut all = extra.clone();
                    all["conversations"] = last_open.clone();
                    all["server_health"] = server_health.clone();
                    all["server_harnesses"] = server_harnesses.clone();
                    all["status"] = status_view.clone();
                    all["diagnoses"] = diag_view.clone();
                    c.mirror.machines = if c.app.admin { machines.clone() } else { Vec::new() };
                    c.mirror.extra = for_viewer(&all, &c.app);
                    c.mirror.extra["pmi"] = pmi.get(&c.app.owner).map_or(Value::Null, |p| p.view.clone());
                c.mirror.extra["briefing"] = brief_views.get(&c.app.owner).cloned().unwrap_or(Value::Null);
                c.mirror.extra["routines"] = routines_views.get(&c.app.owner).cloned().unwrap_or(Value::Null);
                    c.mirror.extra["briefing"] = brief_views.get(&c.app.owner).cloned().unwrap_or(Value::Null);
                    c.mirror.extra["routines"] = routines_views.get(&c.app.owner).cloned().unwrap_or(Value::Null);
                    for u in c.mirror.updates(&mut c.app) {
                        hub.publish(Some(&c.app.session_id), u);
                    }
                    c.changed = false;
                    let _ = reply.send(c.mirror.snapshot(hub.seq(), hub.app_version()));
                }
            }
            next = inbound.try_recv().ok();
        }
        // Decisions made for any conversation are logged with the primary.
        convs[0].app.decide_notes();
        // Each conversation's own work: replies streaming, agents, plans…
        let many = convs.len() > 1;
        let mut diagnosed: Vec<String> = Vec::new();
        for c in convs.iter_mut() {
            while let Ok(event) = c.app.rx.try_recv() {
                let note = notification(&event);
                let done = matches!(event, StreamEvent::Done(_) | StreamEvent::Error(_));
                c.app.handle(event);
                c.changed = true;
                // A diagnosis only looks: changes it asks for are declined; its write-up is kept.
                if let Some(key) = c.diagnosis.clone() {
                    let asked: Vec<u64> = c.app.approvals.iter().map(|a| a.id).collect();
                    for id in asked {
                        c.app.answer_approval_id(id, "n");
                    }
                    if done && !c.app.waiting {
                        c.diagnosis = None;
                        let reply = c.app.messages.iter().rev().find(|m| matches!(m.role.as_str(), "assistant" | "error")).map(|m| (m.role == "assistant", m.content.clone()));
                        let (ok, text) = reply.unwrap_or((false, "no answer".into()));
                        crate::diagnose::finish(&key, &text, ok);
                        diagnosed.push(key);
                    }
                    continue;
                }
                // A routine's run: judged and told by its own rules, not "lyra replied".
                if let Some((r, started)) = c.routine.clone() {
                    let may_change = r.changes;
                    // One that only looks: anything asking to change something is refused at once.
                    if !r.changes {
                        let asked: Vec<u64> = c.app.approvals.iter().map(|a| a.id).collect();
                        for id in asked {
                            c.app.answer_approval_id(id, "n");
                            c.app.log(Level::Plan, format!("routine {}: declined a change (it only looks; /routine edit {} changes on lets it ask)", r.name, r.name));
                        }
                    }
                    if done && !c.app.waiting {
                        c.routine = None;
                        let reply = c.app.messages.iter().rev().find(|m| matches!(m.role.as_str(), "assistant" | "error")).map(|m| (m.role.clone(), m.content.clone()));
                        let outcome = match &reply {
                            Some((role, _)) if role == "error" => "error",
                            Some((_, content)) if content.ends_with("_(stopped)_") => "stopped",
                            _ => "ok",
                        };
                        let text = reply.map(|m| m.1).unwrap_or_default();
                        let (url, model, session, tx) = (format!("{}/chat/completions", c.app.base_url.trim_end_matches('/')), c.app.model.clone(), c.app.session_id.clone(), routine_tx.clone());
                        let whose = c.app.owner.clone();
                        std::thread::spawn(move || {
                            let (needs_user, decided_by) = if outcome == "ok" { crate::routines::needs_user(&url, &model, &r, &text) } else { (true, "it didn't finish".into()) };
                            let summary: String = text.trim().chars().take(400).collect();
                            let run = crate::routines::Run { at: chrono::Utc::now(), seconds: started.elapsed().as_secs(), needs_user, outcome: outcome.into(), summary, session, decided_by };
                            let _ = tx.send((whose, r, run));
                        });
                    }
                    // Only a routine allowed to change things asks the user.
                    if let Some(mut n) = note.filter(|_| may_change) {
                        n.to = To::User(c.app.owner.clone());
                        hub.notify(n);
                    }
                    continue;
                }
                // Their own conversation, and only when they aren't looking at lyra.
                if !notify || hub.watching(&c.app.owner) {
                    continue;
                }
                // With several conversations, say which one.
                let about = |title: String| match c.app.messages.iter().find(|m| m.role == "user") {
                    Some(m) if many => format!("{title} · {}", m.content.lines().next().unwrap_or("").chars().take(40).collect::<String>()),
                    _ => title,
                };
                if done {
                    let body = c.app.messages.iter().rev().find(|m| m.role == "assistant").map(|m| preview(&m.content)).unwrap_or_default();
                    hub.notify(Notification { title: about("lyra replied".into()), body, tag: format!("reply-{}", c.app.session_id), approval: None, url: None, actions: vec![], reference: None, to: To::User(c.app.owner.clone()) });
                }
                if let Some(mut n) = note {
                    n.title = about(n.title);
                    n.to = To::User(c.app.owner.clone());
                    hub.notify(n);
                }
            }
        }
        // Finished write-ups: logged and told.
        for key in diagnosed {
            if let Some(d) = crate::diagnose::all().into_iter().find(|d| d.key == key) {
                let head = crate::diagnose::headline(&d.summary);
                convs[0].app.log(if d.state == "done" { Level::Agent } else { Level::Error }, format!("🔎 {}: {} — {head}", d.machine, d.problem));
                if crate::health::settings().notify && d.state == "done" {
                    hub.notify(Notification { title: format!("🔎 {}: {}", d.machine, d.problem), body: head, tag: format!("diag-{}", d.key), approval: None, url: None, actions: vec![], reference: None, to: To::Admins });
                }
            }
            last_diag = Instant::now() - Duration::from_secs(60);
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
        // Routines: due ones and ones asked for, each in its own conversation.
        if last_routines.elapsed() >= Duration::from_secs(20) || routines_wanted() {
            last_routines = Instant::now();
            // Everyone's: each runs in its person's own conversation, with their rights.
            let mut start: Vec<(String, crate::routines::Routine)> = crate::routines::due_all(chrono::Local::now());
            for (user, name) in crate::routines::take_requests() {
                if let Ok(r) = crate::acting::run(&user, || crate::routines::find(&name))
                    && !start.iter().any(|(u, x)| *u == user && x.name == r.name)
                {
                    start.push((user, r));
                }
            }
            for (user, r) in start {
                if convs.iter().any(|c| c.app.owner == user && c.routine.as_ref().is_some_and(|(x, _)| x.name == r.name)) {
                    continue;
                }
                let mine = user == convs[0].app.owner;
                let mut app = if mine {
                    convs[0].app.fork()
                } else {
                    // Someone else's: only while they may use lyra.
                    let Some(who) = hub.users().who(Some(&user)) else { continue };
                    convs[0].app.fork_for(&who)
                };
                app.input = crate::routines::message(&r);
                app.send();
                if mine {
                    convs[0].app.log(Level::Plan, format!("routine {} started", r.name));
                }
                let mut c = Conv::new(app);
                c.routine = Some((r, Instant::now()));
                convs.push(c);
            }
        }
        while let Ok((user, r, run)) = routine_rx.try_recv() {
            let verdict = if run.outcome != "ok" { format!("{} ({})", run.outcome, run.decided_by) } else if run.needs_user { "needs you".into() } else { "all clear".into() };
            let first = run.summary.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(140).collect::<String>();
            // The owner's Activity tells the owner's routines only.
            if user == convs[0].app.owner {
                convs[0].app.log(if run.needs_user { Level::Error } else { Level::Plan }, format!("routine {}: {verdict} ({}s, by {}) — {first}", r.name, run.seconds, run.decided_by));
            }
            let tell = match r.notify {
                crate::routines::Notify::Always => true,
                crate::routines::Notify::Problems => run.needs_user,
                crate::routines::Notify::Never => false,
            };
            if tell {
                hub.notify(Notification {
                    title: format!("{} {}", if run.needs_user { "⚠" } else { "✓" }, r.name),
                    body: preview(&run.summary),
                    tag: format!("routine-{}", r.name),
                    approval: None,
                    url: None, actions: vec![], reference: None,
                    to: To::User(user.clone()),
                });
            }
            crate::acting::run(&user, || crate::routines::record(&r.name, run));
            everyone = true;
        }
        // PMI, per person with a token: follow their live events (a thread each,
        // started as tokens appear), read again shortly after a change and every 5 minutes.
        if last_pmi_scan.elapsed() >= Duration::from_secs(60) {
            last_pmi_scan = Instant::now();
            let people: Vec<String> = hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active && !pmi.contains_key(&u.id) && crate::pmi::configured_for(&u.id)).map(|u| u.id).collect();
            for user in people {
                let tx = pmi_events_tx.clone();
                let who = user.clone();
                std::thread::spawn(move || crate::pmi::follow(who, tx));
                pmi.insert(user.clone(), PmiUser { due: Some(Instant::now()), last: Instant::now(), nags: crate::pmi::Nags::load_for(&user), ..Default::default() });
            }
        }
        while let Ok((user, e)) = pmi_events.try_recv() {
            let Some(p) = pmi.get_mut(&user) else { continue };
            match e {
                crate::pmi::Event::Live(live) => {
                    if live != p.state.live {
                        p.state.live = live;
                        if user == convs[0].app.owner {
                            convs[0].app.log(Level::Info, if live { "PMI: following its live updates".to_string() } else { "PMI: can't follow its live updates, retrying".to_string() });
                        }
                        p.view = json!(p.state);
                        everyone = true;
                        // Back after an outage: what changed meanwhile.
                        if live {
                            p.due = Some(Instant::now());
                        }
                    }
                    // Their token is gone: forget them until one is set again.
                    if !live && !crate::pmi::configured_for(&user) {
                        pmi.remove(&user);
                        everyone = true;
                    }
                }
                crate::pmi::Event::Changed(areas) => {
                    if areas.iter().any(|a| matches!(a.as_str(), "tasks" | "projects" | "resync" | "structure")) {
                        let soon = Instant::now() + Duration::from_secs(5);
                        p.due = Some(p.due.map_or(soon, |d| d.min(soon)));
                    }
                }
            }
        }
        for user in crate::pmi::take_stale() {
            if let Some(p) = pmi.get_mut(&user) {
                p.due = Some(Instant::now() + Duration::from_secs(2));
            } else {
                // A token was just set: start following them now.
                last_pmi_scan = Instant::now() - Duration::from_secs(120);
            }
        }
        for (user, p) in pmi.iter_mut() {
            if !p.busy && (p.due.is_some_and(|d| Instant::now() >= d) || p.last.elapsed() >= Duration::from_secs(300)) {
                p.busy = true;
                p.due = None;
                p.last = Instant::now();
                let (tx, user) = (pmi_tx.clone(), user.clone());
                std::thread::spawn(move || {
                    let r = crate::pmi::as_user(&user, crate::pmi::snapshot);
                    let _ = tx.send((user, r));
                });
            }
        }
        while let Ok((user, r)) = pmi_rx.try_recv() {
            let Some(p) = pmi.get_mut(&user) else { continue };
            p.busy = false;
            let mine = user == convs[0].app.owner;
            match r {
                Ok(mut s) => {
                    s.live = p.state.live;
                    if mine && p.state.error.is_some() {
                        convs[0].app.log(Level::Info, format!("PMI: back ({})", s.line(chrono::Local::now().date_naive())));
                    }
                    p.state = s;
                }
                Err(e) => {
                    if mine && p.state.error.as_deref() != Some(e.as_str()) {
                        convs[0].app.log(Level::Error, format!("PMI: {e}"));
                    }
                    p.state.error = Some(e);
                }
            }
            let view = json!(p.state);
            if view != p.view {
                p.view = view;
                everyone = true;
            }
        }
        // Nags: each person's, checked with each new view and every minute.
        if last_nag.elapsed() >= Duration::from_secs(60) || everyone {
            last_nag = Instant::now();
            let s = crate::pmi::settings();
            let owner = convs[0].app.owner.clone();
            for (user, p) in pmi.iter_mut().filter(|(_, p)| p.state.at.is_some() && p.state.error.is_none()) {
                // Not in their quiet time (outside work hours, in a meeting): nags wait.
                let quiet = match quiet_seen.get(user) {
                    Some((q, at)) if at.elapsed() < Duration::from_secs(300) => *q,
                    _ => {
                        let q = crate::planner::quiet(user);
                        quiet_seen.insert(user.clone(), (q, Instant::now()));
                        q
                    }
                };
                if quiet {
                    continue;
                }
                let (due, changed) = p.nags.due(&p.state, &s, chrono::Utc::now());
                for n in due {
                    // Only the owner's own reminders in the owner's Activity.
                    if *user == owner {
                        convs[0].app.log(Level::Plan, format!("⏰ still to do: {} (reminder {} of {})", n.title, n.sent, s.nag_max));
                    }
                    hub.notify(Notification {
                        title: format!("⏰ Still to do: {}", n.title),
                        body: format!("The reminder went off {}. Done, or later?", n.fired.with_timezone(&chrono::Local).format("%a %H:%M")),
                        tag: format!("nag-{}", n.task),
                        approval: None,
                        url: Some("/?page=tasks".into()),
                        actions: vec![("done".into(), "Done".into()), ("snooze1h".into(), "In 1 hour".into()), ("tomorrow".into(), "Tomorrow".into())],
                        reference: Some(n.task.clone()),
                        to: To::User(user.clone()),
                    });
                }
                if changed {
                    p.nags.save_for(user);
                }
            }
        }
        while let Ok((user, action, task, r)) = action_rx.try_recv() {
            match r {
                Ok(what) => {
                    if user == convs[0].app.owner {
                        convs[0].app.log(Level::Plan, format!("reminder: {what}"));
                    }
                    if let Some(p) = pmi.get_mut(&user) {
                        p.nags.items.retain(|_, n| n.task != task);
                        p.nags.save_for(&user);
                    }
                }
                Err(e) => {
                    if user == convs[0].app.owner {
                        convs[0].app.log(Level::Error, format!("reminder {action} failed: {e}"));
                    }
                    hub.notify(Notification { title: "Couldn't update the task".into(), body: e, tag: format!("nag-{task}"), approval: None, url: Some("/?page=tasks".into()), actions: vec![], reference: None, to: To::User(user) });
                }
            }
        }
        // Plan my day, for everyone with Outlook and PMI, in their working day.
        if last_plan.elapsed() >= Duration::from_secs(15 * 60) && crate::planner::settings().enabled && crate::planner::settings().working_now(chrono::Local::now()) {
            last_plan = Instant::now();
            for u in hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active).map(|u| u.id) {
                if planning.contains(&u) || !crate::calendar::connected_for(&u) || !crate::pmi::configured_for(&u) {
                    continue;
                }
                planning.insert(u.clone());
                let tx = plan_tx.clone();
                std::thread::spawn(move || {
                    let r = crate::acting::run(&u, crate::planner::run);
                    let _ = tx.send((u, r));
                });
            }
        }
        while let Ok((user, r)) = plan_rx.try_recv() {
            planning.remove(&user);
            let mine = user == convs[0].app.owner;
            match r {
                Ok(did) if !did.is_empty() => {
                    if mine {
                        for d in &did {
                            convs[0].app.log(Level::Plan, format!("🗓 {d}"));
                        }
                    }
                    held.entry(user).or_default().extend(did);
                }
                Ok(_) => {}
                Err(e) if mine => convs[0].app.log(Level::Error, format!("plan my day: {e}")),
                Err(_) => {}
            }
        }
        // Proactive help, for everyone with Outlook connected.
        if last_pro.elapsed() >= Duration::from_secs(5 * 60) && crate::proactive::settings().enabled && crate::planner::settings().working_now(chrono::Local::now()) {
            last_pro = Instant::now();
            pro_round += 1;
            let mail_due = pro_round % 2 == 1;
            let (url, model) = (format!("{}/chat/completions", convs[0].app.base_url.trim_end_matches('/')), convs[0].app.model.clone());
            for u in hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active).map(|u| u.id) {
                if pro_busy.contains(&u) || !crate::calendar::connected_for(&u) {
                    continue;
                }
                pro_busy.insert(u.clone());
                let (tx, url, model) = (pro_tx.clone(), url.clone(), model.clone());
                std::thread::spawn(move || {
                    let p = crate::acting::run(&u, || crate::proactive::pass(&url, &model, mail_due));
                    let _ = tx.send((u, p));
                });
            }
        }
        if last_recap_look.elapsed() >= Duration::from_secs(60) && crate::recap::settings().enabled {
            last_recap_look = Instant::now();
            let now = chrono::Local::now();
            for u in hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active).map(|u| u.id) {
                if recapping.contains(&u) || !(crate::calendar::connected_for(&u) || crate::pmi::configured_for(&u)) || !crate::recap::due(&u, now) {
                    continue;
                }
                recapping.insert(u.clone());
                let tx = recap_tx.clone();
                std::thread::spawn(move || {
                    let r = crate::acting::run(&u, || crate::recap::gather(chrono::Utc::now()));
                    let _ = tx.send((u, r));
                });
            }
        }
        // Bug reports and feature requests: who should hear about what.
        for n in crate::feedback::take_notices() {
            let to = if n.to_admins { To::Admins } else { To::User(n.user.clone().unwrap_or_default()) };
            hub.notify(Notification { title: n.title, body: n.body, tag: "feedback".into(), approval: None, url: Some("/?page=feedback".into()), actions: vec![], reference: None, to });
            everyone = true;
        }
        if last_watch.elapsed() >= Duration::from_secs(120) {
            last_watch = Instant::now();
            for u in hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active).map(|u| u.id) {
                if watching.contains(&u) || !crate::watches::any(&u) {
                    continue;
                }
                watching.insert(u.clone());
                let tx = watch_tx.clone();
                std::thread::spawn(move || {
                    let fired = crate::acting::run(&u, crate::watches::pass);
                    let _ = tx.send((u, fired));
                });
            }
        }
        while let Ok((user, fired)) = watch_rx.try_recv() {
            watching.remove(&user);
            for (title, body) in fired {
                if user == convs[0].app.owner {
                    convs[0].app.log(Level::Plan, format!("👀 {title}: {body}"));
                }
                hub.notify(Notification { title, body, tag: "watch".into(), approval: None, url: None, actions: vec![], reference: None, to: To::User(user.clone()) });
            }
            everyone = true;
        }
        while let Ok((user, r)) = recap_rx.try_recv() {
            recapping.remove(&user);
            crate::recap::save_for(&user, &r);
            if user == convs[0].app.owner {
                convs[0].app.log(Level::Plan, format!("end of day: {}", crate::recap::push_body(&r)));
            }
            if crate::recap::settings().notify && !r.parts.is_empty() {
                hub.notify(Notification { title: "End of day".into(), body: crate::recap::push_body(&r), tag: "recap".into(), approval: None, url: Some("/?page=status".into()), actions: vec![], reference: None, to: To::User(user.clone()) });
            }
            everyone = true;
        }
        while let Ok((user, p)) = pro_rx.try_recv() {
            pro_busy.remove(&user);
            let mine = user == convs[0].app.owner;
            // Meeting prep can't wait: it goes now.
            for (title, body) in p.prep {
                if mine {
                    convs[0].app.log(Level::Plan, format!("{title} — {}", body.replace('\n', " · ")));
                }
                hub.notify(Notification { title, body, tag: "prep".into(), approval: None, url: Some("/?page=tasks".into()), actions: vec![], reference: None, to: To::User(user.clone()) });
            }
            if !p.done.is_empty() {
                if mine {
                    for d in &p.done {
                        convs[0].app.log(Level::Plan, format!("✨ {d}"));
                    }
                }
                held.entry(user).or_default().extend(p.done);
            }
        }
        // What the planner did, told once they're out of quiet time.
        if !held.is_empty() && last_held.elapsed() >= Duration::from_secs(60) {
            last_held = Instant::now();
            let ready: Vec<String> = held.keys().filter(|u| !quiet_seen.get(*u).is_some_and(|(q, at)| *q && at.elapsed() < Duration::from_secs(300))).cloned().collect();
            for user in ready {
                if crate::planner::quiet(&user) {
                    quiet_seen.insert(user.clone(), (true, Instant::now()));
                    continue;
                }
                let did = held.remove(&user).unwrap_or_default();
                hub.notify(Notification {
                    title: "✨ lyra took care of".into(),
                    body: did.join("\n").chars().take(400).collect(),
                    tag: "plan".into(),
                    approval: None,
                    url: Some("/?page=tasks".into()),
                    actions: vec![],
                    reference: None,
                    to: To::User(user),
                });
            }
        }
        // The daily briefing: on schedule or asked for, gathered and written off the loop.
        let wanted = crate::briefing::take_request();
        if !brief_busy && (wanted || last_brief_check.elapsed() >= Duration::from_secs(20)) {
            last_brief_check = Instant::now();
            let s = crate::briefing::settings();
            let now = chrono::Local::now();
            let due = s.enabled && crate::briefing::next(&s.schedule, last_brief.as_ref().map(|b| b.at), now).is_some_and(|t| t <= now);
            if wanted || due {
                brief_busy = true;
                let at = chrono::Utc::now();
                let since = crate::briefing::window_start(last_brief.as_ref().map(|b| b.at), at);
                let mut inputs = crate::briefing::local_inputs(convs[0].app.goals.as_deref(), at, since);
                inputs.machines = machines_detail(hub, node_build.as_deref());
                inputs.server_health = (!server_health.is_null()).then(|| server_health.clone());
                inputs.pmi = pmi.get(&convs[0].app.owner).filter(|p| p.state.at.is_some()).map(|p| p.state.clone());
                let (url, model, tx) = (format!("{}/chat/completions", convs[0].app.base_url.trim_end_matches('/')), convs[0].app.model.clone(), brief_tx.clone());
                let owner = convs[0].app.owner.clone();
                // Everyone else with PMI: their own, with just their tasks.
                // Everyone else with PMI or a calendar connected: their own.
                let others: Vec<(String, crate::briefing::Inputs)> = hub
                    .users()
                    .list()
                    .into_iter()
                    .filter(|u| u.status == lyra_web::Status::Active && u.id != owner)
                    .map(|u| u.id)
                    .filter(|u| pmi.get(u).is_some_and(|p| p.state.at.is_some()) || crate::calendar::connected_for(u))
                    .map(|u| {
                        let since = crate::briefing::window_start(crate::briefing::last_for(&u).map(|b| b.at), at);
                        // Their tasks, routines and goals.
                        let (goals, goal_events) = crate::goals::for_user(&u).map_or_else(Default::default, |g| (g.manager.all().unwrap_or_default(), g.manager.events(None, 300).unwrap_or_default()));
                        let runs = crate::acting::run(&u, crate::routines::runs);
                        let pmi = pmi.get(&u).filter(|p| p.state.at.is_some()).map(|p| p.state.clone());
                        (u, crate::briefing::Inputs { now: at, since, pmi, runs, goals, goal_events, ..Default::default() })
                    })
                    .collect();
                std::thread::spawn(move || {
                    for (user, mut inputs) in std::iter::once((owner, inputs)).chain(others) {
                        // Their calendar today, when they've connected it.
                        if crate::calendar::connected_for(&user) {
                            inputs.calendar = crate::acting::run(&user, crate::calendar::today).ok();
                        }
                        if crate::teams::connected_for(&user) {
                            inputs.teams = crate::acting::run(&user, || crate::teams::chats(25)).ok().map(|c| c.into_iter().filter(|x| x["unread"] == true).collect());
                        }
                        if crate::mail::connected_for(&user) {
                            let since = inputs.since;
                            inputs.mail = crate::acting::run(&user, || crate::mail::glance(since)).ok();
                        }
                        let mut b = crate::briefing::gather(&inputs);
                        if s.summary {
                            b.takeaway = crate::acting::run(&user, || crate::briefing::takeaway(&url, &model, &b));
                        }
                        let _ = tx.send((user, b));
                    }
                });
            }
        }
        while let Ok((user, b)) = brief_rx.try_recv() {
            let owner = user == convs[0].app.owner;
            crate::briefing::save_for(&user, &b);
            if owner {
                brief_busy = false;
                convs[0].app.log(if b.attention > 0 { Level::Error } else { Level::Plan }, format!("briefing: {}{}", b.headline, b.takeaway.as_ref().map(|t| format!(" — {t}")).unwrap_or_default()));
            }
            if crate::briefing::settings().notify {
                hub.notify(Notification {
                    title: format!("☀ Briefing: {}", b.headline),
                    body: crate::briefing::push_body(&b),
                    tag: "briefing".into(),
                    approval: None,
                    url: Some("/?page=status".into()),
                    actions: vec![],
                    reference: None,
                    to: To::User(user.clone()),
                });
            }
            brief_views.insert(user, json!(b));
            if owner {
                last_brief = Some(b);
            }
            everyone = true;
        }
        // Diagnoses: one at a time, each in its own conversation; their write-ups for the panels.
        if last_diag.elapsed() >= Duration::from_secs(5) {
            last_diag = Instant::now();
            let d = convs[0].app.diagnose.clone();
            if d.enabled && !convs.iter().any(|c| c.diagnosis.is_some()) && crate::diagnose::has_queued() {
                let mut app = convs[0].app.fork();
                let session = app.session_id.clone();
                if let Some(next) = crate::diagnose::start_next(|_| session.clone()) {
                    app.input = crate::diagnose::message(&next);
                    app.send();
                    convs[0].app.log(Level::Agent, format!("🔎 looking into {}: {}", next.machine, next.problem));
                    let mut c = Conv::new(app);
                    c.diagnosis = Some(next.key);
                    convs.push(c);
                }
            }
            let now = crate::diagnose::view();
            if now != diag_view {
                diag_view = now;
                everyone = true;
            }
        }
        // What the panels show: each routine's next run, last result, running now.
        if last_routines.elapsed() < Duration::from_millis(50) || everyone {
            for user in crate::routines::people() {
                let running: Vec<String> = convs.iter().filter(|c| c.app.owner == user).filter_map(|c| c.routine.as_ref().map(|(r, _)| r.name.clone())).collect();
                let now = crate::acting::run(&user, || crate::routines::view(&running, 1));
                if routines_views.get(&user) != Some(&now) {
                    routines_views.insert(user, now);
                    everyone = true;
                }
            }
        }
        let ss = convs[0].app.status.clone();
        if ss.enabled && !status_busy && (last_status.is_none_or(|t| t.elapsed() >= Duration::from_secs(ss.every_seconds.max(15))) || crate::status::take_request()) {
            last_status = Some(Instant::now());
            let mut i = convs[0].app.status_inputs();
            let (sent, failed, error) = hub.take_push_counts();
            i.push = Some((hub.push_devices(), sent, failed, error));
            let up = serving_since.elapsed().as_secs();
            i.serving = Some(format!(
                "v{} · up {}d {}h {}m · {} conversation{} open · {} device{} connected",
                env!("CARGO_PKG_VERSION"),
                up / 86400,
                up % 86400 / 3600,
                up % 3600 / 60,
                convs.len(),
                if convs.len() == 1 { "" } else { "s" },
                hub.connections(),
                if hub.connections() == 1 { "" } else { "s" }
            ));
            i.machines = machines_detail(hub, node_build.as_deref());
            i.server_health = server_health.clone();
            status_busy = status_tx.send(i).is_ok();
            server_harnesses = lyra_node::coding::available();
        }
        while let Ok(board) = status_rx.try_recv() {
            status_busy = false;
            match board {
                Ok(board) => {
                    for (id, problem, text) in crate::status::alerts(&mut status_alerts, &board, &ss) {
                        let d = &convs[0].app.diagnose;
                        if problem && d.enabled && d.auto {
                            crate::diagnose::queue(&format!("status:{id}"), crate::caps::HERE, &text, false);
                        } else if !problem {
                            crate::diagnose::resolve(&format!("status:{id}"));
                        }
                        convs[0].app.log(if problem { Level::Error } else { Level::Agent }, format!("{} {text}", if problem { "⚠" } else { "✓" }));
                        if ss.notify {
                            hub.notify(Notification {
                                title: if problem { "⚠ lyra needs a look".into() } else { "✓ lyra".into() },
                                body: text,
                                tag: "status".into(),
                                approval: None,
                                url: None, actions: vec![], reference: None,
                            to: To::Admins,
                            });
                        }
                    }
                    status_view = json!(board);
                    everyone = true;
                }
                Err(e) => convs[0].app.log(Level::Error, format!("status: {e}")),
            }
        }
        // The server's own health, like a machine's.
        if last_checkup.is_none_or(|t| t.elapsed() >= lyra_node::health::EVERY) {
            last_checkup = Some(Instant::now());
            let tx = health_tx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(lyra_node::health::report());
            });
        }
        while let Ok(h) = health_rx.try_recv() {
            health_report(&mut convs[0].app, hub, &mut alerts, "server", &h);
            server_health = crate::health::view(&h);
            everyone = true;
        }
        // Pairing requests expire and devices come and go: refresh now and then.
        if last_extra.elapsed() > Duration::from_secs(10) {
            last_extra = Instant::now();
            let s = crate::health::settings();
            if s.enabled {
                let paired: Vec<String> = hub.devices().list().into_iter().filter(|d| d.kind == "node").map(|d| d.name).collect();
                let (gone, back) = alerts.connected(&paired, &machines, Duration::from_secs(s.offline_minutes * 60));
                for m in gone {
                    alert(&mut convs[0].app, hub, &m, &format!("{m} has been offline for {} minute{}", s.offline_minutes, if s.offline_minutes == 1 { "" } else { "s" }), true);
                }
                for m in back {
                    alert(&mut convs[0].app, hub, &m, &format!("{m} is back online"), false);
                }
            }
            let now = hub_status(hub, node_build.as_deref());
            if now != extra {
                extra = now;
                everyone = true;
            }
        }
        // The conversations loaded now and which are answering, for every device.
        let open = json!(convs.iter().map(|c| json!({ "session": c.app.session_id, "title": title(&c.app), "answering": c.app.waiting, "owner": c.app.owner })).collect::<Vec<_>>());
        if open != last_open {
            last_open = open.clone();
            everyone = true;
        }
        let mut extra_now = extra.clone();
        extra_now["conversations"] = open;
        extra_now["server_health"] = server_health.clone();
        extra_now["server_harnesses"] = server_harnesses.clone();
        extra_now["status"] = status_view.clone();
        extra_now["diagnoses"] = diag_view.clone();
        let attached = hub.attached_sessions();
        for c in convs.iter_mut() {
            // The phase timer ("thinking 4s") ticks while something is happening.
            if everyone || c.changed || (c.app.waiting && c.last_status.elapsed() > Duration::from_secs(1)) {
                c.last_status = Instant::now();
                c.changed = false;
                c.mirror.machines = if c.app.admin { machines.clone() } else { Vec::new() };
                c.mirror.extra = for_viewer(&extra_now, &c.app);
                c.mirror.extra["pmi"] = pmi.get(&c.app.owner).map_or(Value::Null, |p| p.view.clone());
                c.mirror.extra["briefing"] = brief_views.get(&c.app.owner).cloned().unwrap_or(Value::Null);
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
        "users_waiting": hub.users().list().iter().filter(|u| u.status == lyra_web::Status::Pending).count(),
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
            // Each platform against its own build (Windows nodes against lyra-node.exe).
            let build = |m: &lyra_web::MachineInfo| if m.os.to_lowercase().contains("windows") { hub.node_build_windows() } else { node_build.map(str::to_string) };
            let update = m.is_some_and(|m| m.self_update && build(m).is_some_and(|b| b != m.build));
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
                "health": m.and_then(|m| m.health.as_ref()).map(crate::health::view),
                "harnesses": m.map(|m| m.harnesses.clone()),
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

const RULES_USAGE: &str = "/machines rules <name|server> [on|off | allow|write|deny|ssh add|remove <value>]";

/// A machine's rules, for the terminal.
fn rules_text(name: &str, r: &lyra_system::Settings) -> String {
    let list = |v: &[String]| if v.is_empty() { "—".to_string() } else { v.join(", ") };
    format!(
        "rules on {name}: system access {}\n  run without asking (allow): {}\n  write without asking (write): {}\n  off limits (deny): {}\n  ssh hosts (ssh): {}\n  commands time out after {}s · approvals wait {}s\n{RULES_USAGE}",
        if r.enabled { "on" } else { "off" },
        list(&r.allow_commands),
        list(&r.write_roots),
        list(&r.deny_paths),
        list(&r.ssh_hosts),
        r.timeout_seconds,
        r.approval_timeout_seconds
    )
}

/// `on|off` or `<list> add|remove <value>`, checked like the app's dialog.
fn edit_rules(rules: &mut lyra_system::Settings, change: &str) -> Result<(), String> {
    // On a copy: a refused change leaves the rules as they were.
    let mut next = rules.clone();
    let r = &mut next;
    let mut words = change.splitn(3, ' ');
    let (what, op, value) = (words.next().unwrap_or(""), words.next().unwrap_or(""), words.next().unwrap_or("").trim());
    match (what, op) {
        ("on", "") => r.enabled = true,
        ("off", "") => r.enabled = false,
        (list @ ("allow" | "write" | "deny" | "ssh"), op @ ("add" | "remove")) if !value.is_empty() => {
            let v = match list {
                "allow" => &mut r.allow_commands,
                "write" => &mut r.write_roots,
                "deny" => &mut r.deny_paths,
                _ => &mut r.ssh_hosts,
            };
            if op == "add" {
                if !v.iter().any(|x| x == value) {
                    v.push(value.to_string());
                }
            } else {
                let before = v.len();
                v.retain(|x| x != value);
                if v.len() == before {
                    return Err(format!("{value:?} isn't in {list}"));
                }
            }
        }
        _ => return Err(format!("usage: {RULES_USAGE}")),
    }
    *rules = next.cleaned()?;
    Ok(())
}

/// The server's own `[system]` rules changed from the app: checked, saved
/// to config.toml, in effect at once.
fn set_server_rules(app: &mut App, wanted: &Value) -> Result<Value, String> {
    let system = app.caps.as_ref().and_then(|c| c.system.as_ref()).ok_or("system access isn't set up")?;
    let wanted: lyra_system::Settings = serde_json::from_value(wanted.clone()).map_err(|e| format!("those rules don't read: {e}"))?;
    let wanted = wanted.cleaned()?;
    let path = crate::config::update(|doc| {
        crate::config::set_system(doc, &wanted);
        Ok(())
    })?;
    let switched = system.settings().enabled != wanted.enabled;
    system.set_settings(wanted)?;
    if switched && let Some(caps) = &app.caps {
        caps.refresh();
    }
    let answer = json!({ "system": system.settings(), "path": crate::context::show(&path) });
    app.log(Level::Agent, "the server's rules changed from the app".to_string());
    Ok(answer)
}

/// The lists the web app's pages show.
/// Conversations loaded now: (session, answering).
type Loaded = Vec<(String, bool)>;

fn data(app: &mut App, hub: &Hub, what: &str, arg: &Value, node_build: Option<&str>, loaded: &Loaded) -> Value {
    let text = |r: Result<String, String>| json!({ "text": r.unwrap_or_else(|e| e) });
    let page = |r: Result<Value, String>| r.unwrap_or_else(|e| json!({ "error": e }));
    match what {
        "sessions" => {
            let Some(dir) = crate::sessions::dir() else { return json!([]) };
            let all = crate::sessions::list_for(&dir, &app.owner);
            let metas = crate::sessions::metas(&dir);
            let folders = crate::sessions::folders(&dir, &app.owner);
            // A folder for a few new ones (the decision model, once each, in the background).
            if !folders.is_empty() && crate::decide::model().is_some() {
                let todo: Vec<String> = all.iter().filter(|s| s.id != app.session_id && s.updated > chrono::Utc::now() - chrono::Duration::days(14) && metas.get(&s.id).is_none_or(|m| !m.looked && m.folder.is_none() && !m.archived)).take(3).map(|s| s.id.clone()).collect();
                if !todo.is_empty() {
                    let (dir, owner) = (dir.clone(), app.owner.clone());
                    std::thread::spawn(move || {
                        crate::acting::set(&owner);
                        for id in todo {
                            let Ok(s) = crate::sessions::find_for(&dir, &id, &owner) else { continue };
                            let pick = crate::sessions::suggest_folder(&s, &folders);
                            let _ = crate::sessions::set_meta(&dir, &id, &owner, |m| {
                                m.looked = true;
                                m.suggested = pick;
                            });
                        }
                    });
                }
            }
            // The latest 300, and every pinned or filed one however old.
            let keep = |s: &crate::sessions::Session| metas.get(&s.id).is_some_and(|m| m.pinned || m.folder.is_some());
            json!(all.iter().enumerate().filter(|(i, s)| *i < 300 || keep(s)).map(|(_, s)| {
                let m = metas.get(&s.id).cloned().unwrap_or_default();
                json!({
                    "id": s.id, "title": s.title, "turns": s.user_turns(), "updated": s.updated, "current": s.id == app.session_id,
                    "open": loaded.iter().any(|(id, _)| *id == s.id), "answering": loaded.iter().any(|(id, w)| *id == s.id && *w),
                    "pinned": m.pinned, "archived": m.archived, "folder": m.folder, "suggested": m.suggested,
                })
            }).collect::<Vec<_>>())
        }
        "search" => {
            let query = arg["query"].as_str().unwrap_or("").trim();
            let all = crate::sessions::dir().map(|d| crate::sessions::list_for(&d, &app.owner)).unwrap_or_default();
            json!(crate::sessions::search(&all, query, 30).iter().map(|h| json!({
                "id": h.id, "title": h.title, "updated": h.updated, "role": h.role, "snippet": h.snippet, "score": h.score,
                "current": h.id == app.session_id,
            })).collect::<Vec<_>>())
        }
        "routines" => crate::acting::run(&app.owner, || crate::routines::view(&[], 10)),
        "briefing" => crate::briefing::last_for(&app.owner).map_or(Value::Null, |b| json!(b)),
        "recap" => crate::recap::last_for(&app.owner).map_or(Value::Null, |r| json!(r)),
        "changelog" => json!({ "version": crate::changelog::version(), "releases": crate::changelog::all() }),
        // Feedback: everyone sends and follows their own; admins see and move everyone's.
        "feedback" | "feedback_submit" | "feedback_comment" | "feedback_update" | "feedback_seen" | "feedback_analyze" => {
            let who = crate::feedback::Who { user: app.owner.clone(), name: hub.users().get(&app.owner).map_or_else(|| "the owner".to_string(), |u| u.name), admin: app.admin };
            let id = arg["id"].as_u64().unwrap_or(0);
            let s = |k: &str| arg[k].as_str().map(str::to_string);
            let done = match what {
                "feedback_submit" => {
                    // Their own uploads only.
                    let ups: Vec<_> = arg["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str()).filter_map(|id| hub.upload(id)).filter(|(up, _)| up.user.as_deref().unwrap_or(lyra_web::users::OWNER) == who.user).collect();
                    let files = ups.iter().map(|(up, _)| crate::feedback::File { id: up.id.clone(), name: up.name.clone(), mime: up.mime.clone() }).collect();
                    let paths = ups.into_iter().map(|(up, path)| (up.name, path)).collect();
                    crate::feedback::submit(&who, &s("kind").unwrap_or_default(), &s("title").unwrap_or_default(), &s("details").unwrap_or_default(), s("severity").as_deref(), files, &s("page").unwrap_or_default()).map(|i| {
                        // lyra's read of it, in the background.
                        crate::feedback::analyze(i.id, &who.user, paths);
                        json!({ "ok": true, "id": i.id })
                    })
                }
                // Admins: read it again (after replies added detail).
                "feedback_analyze" if who.admin => {
                    let found = crate::feedback::list(&who).into_iter().find(|i| i.id == id);
                    match found {
                        Some(i) => {
                            let paths = i.files.iter().filter_map(|f| hub.upload(&f.id).map(|(up, path)| (up.name, path))).collect();
                            crate::feedback::analyze(id, &i.user, paths);
                            Ok(json!({ "ok": true }))
                        }
                        None => Err(format!("no feedback #{id}")),
                    }
                }
                "feedback_comment" => crate::feedback::comment(&who, id, &s("text").unwrap_or_default()).map(|_| json!({ "ok": true })),
                "feedback_update" => crate::feedback::update(&who, id, s("status").as_deref(), s("priority").as_deref(), s("shipped_in").as_deref()).map(|_| json!({ "ok": true })),
                "feedback_seen" => crate::feedback::seen(&who, id).map(|_| json!({ "ok": true })),
                _ => Ok(json!({
                    "admin": who.admin, "me": who.user, "version": crate::changelog::version(),
                    "items": crate::feedback::list(&who).iter().map(|i| crate::feedback::view(i, &who)).collect::<Vec<_>>(),
                })),
            };
            done.unwrap_or_else(|e| json!({ "error": e }))
        }
        "watches" => json!(crate::watches::list_for(&app.owner)),
        // AI usage: everyone's and each person's for admins, their own for members.
        // The current plan's recovery points (admins; members have no plans).
        "plan_checkpoints" if app.admin => match (app.current_plan.as_ref(), app.engine.as_ref()) {
            (Some(p), Some(engine)) => match engine.checkpoints(p.id) {
                Ok(list) => json!(list.iter().rev().map(|c| json!({
                    "at": c.created_at, "version": c.plan_version, "reason": c.reason,
                    "done": c.completed_steps.iter().filter_map(|id| p.step(*id)).map(|s| s.key.clone()).collect::<Vec<_>>(),
                })).collect::<Vec<_>>()),
                Err(e) => json!({ "error": e }),
            },
            _ => json!([]),
        },
        "usage" => {
            let days = arg["days"].as_i64().unwrap_or(7);
            let mut v = crate::usage::summary(days, (!app.admin).then_some(app.owner.as_str()));
            let users = hub.users().list();
            if let Some(list) = v["users"].as_array_mut() {
                for u in list {
                    let id = u["user"].as_str().unwrap_or("").to_string();
                    u["name"] = json!(users.iter().find(|x| x.id == id).map_or(id, |x| x.name.clone()));
                }
            }
            v
        }
        // Their notes and lists.
        "notes" => crate::acting::run(&app.owner, crate::notes::page),
        // Their inbox at a glance (the last day).
        "mail" => match crate::mail::connected_for(&app.owner) {
            false => json!({ "connected": false }),
            true => crate::acting::run(&app.owner, || crate::mail::glance(chrono::Utc::now() - chrono::Duration::days(1))).map_or_else(|e| json!({ "connected": true, "error": e }), |mut v| {
                v["connected"] = json!(true);
                v
            }),
        },
        // Today's calendar (theirs), or how to connect it.
        "calendar" => {
            if !crate::calendar::available() {
                json!({ "available": false })
            } else if !crate::calendar::connected_for(&app.owner) {
                json!({ "available": true, "connected": false })
            } else {
                match crate::acting::run(&app.owner, crate::calendar::today) {
                    Ok(mut v) => {
                        v["available"] = json!(true);
                        v["connected"] = json!(true);
                        v["mail"] = json!(crate::mail::connected_for(&app.owner));
                        v["teams"] = json!(crate::teams::connected_for(&app.owner));
                        v["meetings"] = json!(crate::meetings::ready(&app.owner));
                        v["meetings_available"] = json!(crate::calendar::meetings_enabled());
                        v
                    }
                    Err(e) => json!({ "available": true, "connected": true, "error": e }),
                }
            }
        }
        "pmi" => crate::pmi::as_user(&app.owner, crate::pmi::snapshot).map_or_else(|e| json!({ "error": e }), |s| json!(s)),
        "coding" => json!(crate::coding::jobs()),
        // The people who use lyra (admins: the gate is in the loop).
        "users" => {
            let devices = hub.devices().list();
            json!(hub.users().list().iter().map(|u| json!({
                "id": u.id, "name": u.name, "email": u.email, "role": u.role, "status": u.status,
                "created": u.created, "last_seen": u.last_seen, "microsoft": !u.oid.is_empty(),
                "devices": devices.iter().filter(|d| d.user.as_deref() == Some(u.id.as_str())).map(|d| json!({ "name": d.name, "last_seen": d.last_seen })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>())
        }
        "status" => {
            let mut v = crate::status::latest().map_or(Value::Null, |b| json!(b));
            // A member's view leaves the machines out.
            if !app.admin
                && let Some(rows) = v["rows"].as_array()
            {
                v["rows"] = json!(rows.iter().filter(|r| r["group"] != "Machines").cloned().collect::<Vec<_>>());
            }
            v
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
        "goals" => page(app.goals.clone().ok_or("goals are off ([goals] enabled)".to_string()).and_then(|g| crate::goals::page(&g))),
        "skills" => page(app.learning.clone().ok_or("learning is off".to_string()).and_then(|l| l.page_for(app.personal().as_deref(), app.admin))),
        "memory" => {
            let mine = app.personal().map(|u| format!("user:{u}"));
            page(app.mem().ok_or("memory is off".to_string()).and_then(|m| m.page(arg["query"].as_str().unwrap_or(""), arg["scope"].as_str().unwrap_or(""), mine.as_deref())))
        }
        "rules" => json!({
            "system": app.caps.as_ref().and_then(|c| c.system.as_ref()).map(|s| s.settings()).unwrap_or_default(),
            "path": crate::config::path().map(|p| crate::context::show(&p)),
        }),
        "about" => json!({
            "backups": crate::config::home().map(|home| {
                let dir = app.backup.dir(&home);
                let all = crate::backup::list(&dir);
                json!({
                    "dir": crate::context::show(&dir), "enabled": app.backup.enabled, "at": app.backup.at, "keep": app.backup.keep,
                    "count": all.len(), "last": all.first().map(|b| json!({ "name": b.name, "made": b.made.to_rfc3339(), "size": b.size })),
                })
            }),
            "lyra": env!("CARGO_PKG_VERSION"),
            "app": hub.app_version(),
            "node_build": node_build.map(|b| b.chars().take(12).collect::<String>()),
            "model": app.model,
            "decide": crate::decide::model(),
            "session": app.session_id,
            "devices": hub.devices().list().len(),
        })
        .as_object()
        .map(|m| {
            // A member sees what lyra is, not the server's backups or devices.
            let keep = |k: &str| app.admin || !matches!(k, "backups" | "devices" | "node_build");
            json!(m.iter().filter(|(k, _)| keep(k)).map(|(k, v)| (k.clone(), v.clone())).collect::<serde_json::Map<_, _>>())
        })
        .unwrap_or_default(),
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
                let here = coding_agents(&json!({ "harnesses": lyra_node::coding::available() }));
                let server = format!("● server — where lyra runs{}", if here.is_empty() { " · coding: none installed".to_string() } else { here });
                if all.is_empty() {
                    return Ok(format!("{server}\n\nno machines yet. On a machine: curl -fsSL <lyra url>/install.sh | sh  (or lyra-node pair <url>)"));
                }
                Ok(server + "\n" + &all
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
                        ) + &coding_agents(m) + &match m["health"].as_object() {
                            Some(h) => {
                                let problems: Vec<&str> = h.get("problems").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
                                format!("\n    {}{}", h.get("summary").and_then(Value::as_str).unwrap_or(""), if problems.is_empty() { String::new() } else { format!(" · ⚠ {}", problems.join("; ")) })
                            }
                            None => String::new(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n\n/machines health [name] · /machines update <name|all> · /machines remove <name> · add one: curl -fsSL <lyra url>/install.sh | sh (Linux) · irm <lyra url>/install.ps1 | iex (Windows, as administrator)")
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
                if sub == "update" && node_build.is_none() && hub.node_build_windows().is_none() {
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
            // Disks, memory, load, failed units, updates (from its last report).
            "health" => {
                if rest.is_empty() || rest.eq_ignore_ascii_case("server") {
                    let mut out = vec![crate::health::describe("server", &lyra_node::health::report())];
                    out.extend(crate::diagnose::lines_for("server"));
                    if rest.is_empty() {
                        for m in hub.machines() {
                            out.push(match &m.health {
                                Some(h) => crate::health::describe(&m.name, h),
                                None => format!("{}: no report yet", m.name),
                            });
                            out.extend(crate::diagnose::lines_for(&m.name));
                        }
                    }
                    return Ok(out.join("\n\n"));
                }
                let m = hub.machines().into_iter().find(|m| m.name.eq_ignore_ascii_case(rest)).ok_or_else(|| format!("{rest} isn't connected (/machines lists them)"))?;
                let mut text = m.health.as_ref().map_or(format!("{}: no report yet (it sends one every 5 minutes)", m.name), |h| crate::health::describe(&m.name, h));
                for line in crate::diagnose::lines_for(&m.name) {
                    text += &format!("\n{line}");
                }
                Ok(text)
            }
            // What runs without asking there; changed like the app's Rules dialog.
            "rules" => {
                let (name, change) = rest.split_once(' ').map_or((rest, ""), |(n, c)| (n, c.trim()));
                if name.is_empty() {
                    return Err(format!("usage: {RULES_USAGE}"));
                }
                if name.eq_ignore_ascii_case("server") {
                    let system = self.caps.as_ref().and_then(|c| c.system.as_ref()).ok_or("system access isn't set up")?;
                    let mut rules = system.settings();
                    if change.is_empty() {
                        return Ok(rules_text("server", &rules));
                    }
                    edit_rules(&mut rules, change)?;
                    let answer = set_server_rules(self, &json!(rules)).map_err(|e| format!("not changed: {e}"))?;
                    let rules: lyra_system::Settings = serde_json::from_value(answer["system"].clone()).map_err(|e| e.to_string())?;
                    return Ok(format!("saved to config.toml, in effect now\n{}", rules_text("server", &rules)));
                }
                let Some(name) = all.iter().filter_map(|m| m["name"].as_str()).find(|n| n.eq_ignore_ascii_case(name)).map(str::to_string) else {
                    return Err(format!("no machine {name:?} (/machines lists them)"));
                };
                // Check the change before asking the machine, so a typo fails here.
                if !change.is_empty() {
                    edit_rules(&mut lyra_system::Settings::default(), change)?;
                }
                let (tx, change) = (self.tx.clone(), change.to_string());
                std::thread::spawn(move || {
                    let ask = |set: Option<&lyra_system::Settings>| {
                        let mut request = json!({ "type": "rules" });
                        if let Some(set) = set {
                            request["set"] = json!(set);
                        }
                        hub.call_machine(&name, request, Duration::from_secs(20))
                            .and_then(|v| v["system"].is_object().then(|| v.clone()).ok_or_else(|| v["error"].as_str().unwrap_or("no rules in the answer").to_string()))
                            .and_then(|v| serde_json::from_value::<lyra_system::Settings>(v["system"].clone()).map(|r| (r, v["path"].as_str().unwrap_or("").to_string())).map_err(|e| e.to_string()))
                    };
                    let note = match ask(None) {
                        Err(e) => format!("✗ {name}'s rules: {e}"),
                        Ok((rules, _)) if change.is_empty() => rules_text(&name, &rules),
                        Ok((mut rules, _)) => match edit_rules(&mut rules, &change).and_then(|()| ask(Some(&rules))) {
                            Ok((rules, path)) => format!("✓ {name} saved its rules ({path}), in effect now\n{}", rules_text(&name, &rules)),
                            Err(e) => format!("✗ {name}'s rules weren't changed: {e}"),
                        },
                    };
                    let _ = tx.send(crate::StreamEvent::Notice(note));
                });
                Ok(format!("asking {}…", rest.split_whitespace().next().unwrap_or("")))
            }
            _ => Err(format!("usage: /machines [update <name|all> | remove <name> | health [name|server]] · {RULES_USAGE}")),
        }
    }

    /// `/users [approve|decline|admin|member|disable|enable <who>]`: the
    /// people who use lyra (only with `lyra serve`, admins only).
    pub(crate) fn users_command(&mut self, arg: &str) -> Result<String, String> {
        use lyra_web::{Role, Status};
        let hub = self.hub.clone().ok_or("users sign in to `lyra serve`; this lyra isn't serving")?;
        let users = hub.users();
        let (sub, rest) = arg.trim().split_once(' ').map_or((arg.trim(), ""), |(a, b)| (a, b.trim()));
        let change = |role: Option<Role>, status: Option<Status>, what: &str| -> Result<String, String> {
            if rest.is_empty() {
                return Err(format!("usage: /users {sub} <name or email>"));
            }
            let u = users.update(rest, role, status)?;
            Ok(format!("{} ({}) {what}", u.name, if u.email.is_empty() { u.id.clone() } else { u.email.clone() }))
        };
        match sub {
            "" | "list" => {
                let devices = hub.devices().list();
                let mut out: Vec<String> = users
                    .list()
                    .iter()
                    .map(|u| {
                        let n = devices.iter().filter(|d| d.user.as_deref() == Some(u.id.as_str())).count();
                        let status = match u.status {
                            Status::Active => "",
                            Status::Pending => " · ⏳ waiting to be let in (/users approve)",
                            Status::Disabled => " · disabled",
                        };
                        format!(
                            "{} {}{} · {} · {n} device{}{status}",
                            if u.role == Role::Admin { "★" } else { "·" },
                            u.name,
                            if u.email.is_empty() { String::new() } else { format!(" <{}>", u.email) },
                            if u.role == Role::Admin { "admin" } else { "member" },
                            if n == 1 { "" } else { "s" }
                        )
                    })
                    .collect();
                out.push("/users approve|decline|admin|member|disable|enable <name or email>".into());
                Ok(out.join("\n"))
            }
            "approve" | "enable" => change(None, Some(Status::Active), "can use lyra"),
            "decline" | "disable" => change(None, Some(Status::Disabled), "can't use lyra (their devices stop working)"),
            "admin" => change(Some(Role::Admin), None, "is an admin"),
            "member" => change(Some(Role::Member), None, "is a member"),
            _ => Err("usage: /users [approve|decline|admin|member|disable|enable <name or email>]".into()),
        }
    }

    /// `/usage [days]`: everyone's and each person's for admins, one's own for members.
    pub(crate) fn usage_command(&mut self, arg: &str) -> Result<String, String> {
        let days = arg.trim().parse::<i64>().unwrap_or(7);
        let users = self.hub.as_ref().map(|h| h.users().list()).unwrap_or_default();
        let name = |id: &str| users.iter().find(|u| u.id == id).map_or_else(|| id.to_string(), |u| u.name.clone());
        let only = (!self.admin).then(|| self.owner.clone());
        Ok(crate::usage::describe(days, only.as_deref(), &name))
    }

    /// `/whoami`: who this conversation belongs to.
    pub(crate) fn whoami(&self) -> String {
        let name = self.hub.as_ref().and_then(|h| h.users().get(&self.owner)).map_or_else(|| self.owner.clone(), |u| if u.email.is_empty() { u.name } else { format!("{} <{}>", u.name, u.email) });
        format!("{name} · {}", if self.admin { "admin" } else { "member" })
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
                    out.push(format!("? {} ({}, {}) asks to pair — code {}: /devices approve {}{} · /devices deny {}", p.name, p.hostname, p.kind, p.code, p.code, if p.kind == "node" { "" } else { " for <whose>" }, p.code));
                }
                if out.is_empty() {
                    out.push("no paired devices".into());
                }
                out.push(String::new());
                out.push("pair with a code: `lyra pair` on the server · headless: lyra-node pair <url> (then approve here)".into());
                Ok(out.join("\n"))
            }
            // "/devices approve CODE for dana@…": a terminal is someone's; a machine isn't.
            "approve" | "deny" => {
                let (code, user) = rest.split_once(" for ").map_or((rest, None), |(c, u)| (c.trim(), Some(u.trim())));
                hub.answer_pair(code, sub == "approve", user)
            }
            "remove" => {
                let d = hub.devices().remove(rest)?;
                Ok(format!("removed {} ({}); it has to pair again{}", d.name, d.id, if d.kind == "node" { " — to uninstall lyra-node too, use /machines remove while it's online" } else { "" }))
            }
            _ => Err("usage: /devices [approve <code> [for <name or email>] | deny <code> | remove <name|id>]".into()),
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
            url: None, actions: vec![], reference: None,
        to: To::Admins,
        }),
        StreamEvent::Error(e) => Some(Notification { title: "lyra hit an error".into(), body: preview(e), tag: "error".into(), approval: None, url: None, actions: vec![], reference: None, to: To::Admins }),
        StreamEvent::PlanFinished(result) => Some(Notification {
            title: "Plan update".into(),
            body: match result {
                Ok(_) => "A plan stopped running — open lyra to see how it went.".into(),
                Err(e) => preview(e),
            },
            tag: "plan".into(),
            approval: None,
            url: None, actions: vec![], reference: None,
        to: To::Admins,
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
    fn rules_change_from_the_terminal_like_the_dialog() {
        let mut r = lyra_system::Settings::default();
        edit_rules(&mut r, "allow add git pull").unwrap();
        edit_rules(&mut r, "allow add git pull").unwrap();
        assert_eq!(r.allow_commands, vec!["git pull"], "added once");
        edit_rules(&mut r, "write add ~/Projects").unwrap();
        edit_rules(&mut r, "off").unwrap();
        assert!(!r.enabled && r.write_roots == vec!["~/Projects"]);
        assert!(edit_rules(&mut r, "write add /").is_err(), "checked like the app's dialog");
        assert_eq!(r.write_roots, vec!["~/Projects"], "and left as it was");
        assert!(edit_rules(&mut r, "deny remove ~/nothing").is_err());
        assert!(edit_rules(&mut r, "allow").is_err());
        assert!(rules_text("desktop", &r).contains("git pull"));
    }

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
