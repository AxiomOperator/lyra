//! `lyra serve`: the same lyra (memory, skills, agents, plans, goals) with
//! no terminal, reached from the PWA over a WebSocket. The app state drives
//! both front ends; here it's mirrored to the devices as small updates
//! (a new message, text appended to one, the status line) and the devices'
//! messages and approvals come back in. Push notifications go out when
//! nobody has lyra open.


mod data;
mod get;
mod assistant;
#[path = "loop.rs"]
mod r#loop;
mod pushes;

use data::*;
pub use r#loop::run;
use pushes::*;

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

    fn call_trusted_folder(&self, user: &str, folder: &str, request: Value, timeout: Duration) -> Result<Value, String> {
        self.0.call_trusted_folder(user, folder, request, timeout)
    }

    fn folder_trusted(&self, user: &str, folder: &str) -> bool {
        self.0.folder_trusted(user, folder)
    }

    fn upload_path(&self, id: &str) -> Option<std::path::PathBuf> {
        self.0.upload(id).map(|(_, path)| path)
    }
}

/// The pages a member (not an admin) may ask for: their own conversations,
/// data and settings. Everything else is an admin's (machines, users, devices,
/// the server's settings, email services, Running now…).
pub(crate) const MEMBER_PAGES: &[&str] = &["sessions", "search", "agents", "skills", "activity", "about", "models", "do", "pmi", "routines", "routine_save", "routine_results", "routine_result", "goals", "memory", "calendar", "mail", "notes", "usage", "recap", "briefing", "watches", "everything", "templates", "template_put", "template_remove", "meetings", "meeting", "meeting_notes", "meeting_followup", "meeting_draft", "document", "document_save", "document_kind", "document_remove", "document_download", "document_onedrive", "document_mail", "document_ask", "me", "me_export", "me_clear_actions", "changelog", "feedback", "feedback_submit", "feedback_comment", "feedback_update", "feedback_seen", "feedback_analyze", "feedback_enhance", "qa", "qa_put", "qa_remove", "qa_promote", "email", "email_set", "email_test"];

pub(crate) fn member_may_get(what: &str) -> bool {
    MEMBER_PAGES.contains(&what)
}

/// Each person's quiet time (outside work hours, in a meeting), worked out
/// off the loop (it asks their Outlook calendar, which can take a while) and
/// kept for 5 minutes; the loop only reads what's known.
pub(crate) struct QuietTimes {
    seen: std::collections::HashMap<String, (bool, Instant)>,
    asking: std::collections::HashSet<String>,
    tx: std::sync::mpsc::Sender<(String, bool)>,
    rx: std::sync::mpsc::Receiver<(String, bool)>,
}

impl QuietTimes {
    pub(crate) fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self { seen: Default::default(), asking: Default::default(), tx, rx }
    }

    /// Whether they're in quiet time: what's known (asked again after 5
    /// minutes, off the loop), `None` while it's first being looked up.
    pub(crate) fn now(&mut self, user: &str) -> Option<bool> {
        while let Ok((u, q)) = self.rx.try_recv() {
            self.asking.remove(&u);
            self.seen.insert(u, (q, Instant::now()));
        }
        let fresh = self.seen.get(user).is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(300));
        if !fresh && self.asking.insert(user.to_string()) {
            let (tx, user) = (self.tx.clone(), user.to_string());
            crate::acting::spawn(move || {
                let q = crate::planner::quiet(&user);
                let _ = tx.send((user, q));
            });
        }
        self.seen.get(user).map(|(q, _)| *q)
    }
}

/// lyra was asked to stop (SIGTERM, Ctrl-C): nothing new starts while what's
/// running finishes (up to [`drain`]); runs still going start again after.
pub(crate) static STOPPING: std::sync::LazyLock<std::sync::Arc<std::sync::atomic::AtomicBool>> = std::sync::LazyLock::new(Default::default);

/// How long a stop waits for work in progress: 75 s (systemd's own limit is
/// 90 s), or `LYRA_DRAIN_SECONDS`.
pub(crate) fn drain() -> Duration {
    Duration::from_secs(std::env::var("LYRA_DRAIN_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(75))
}

pub(crate) fn stopping() -> bool {
    STOPPING.load(std::sync::atomic::Ordering::Relaxed)
}

/// Catch SIGTERM and SIGINT (a second one stops at once).
pub(crate) fn catch_stop() {
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        let _ = signal_hook::flag::register_conditional_shutdown(sig, 1, STOPPING.clone());
        let _ = signal_hook::flag::register(sig, STOPPING.clone());
    }
}

/// The status line, pending approvals, agents and connected machines.
fn status(app: &App, machines: &[String]) -> Value {
    let active = app.agents.as_ref().map(|a| a.active.lock().map(|v| v.clone()).unwrap_or_default()).unwrap_or_default();
    json!({
        "phase": crate::ui::phase_text(app),
        "waiting": app.waiting,
        "can_continue": app.can_continue && !app.waiting,
        // Keep going (after an error, with work done), Try again (nothing done), Continue (the limit).
        "continue_kind": match (app.continue_after_error, app.done_so_far().is_some()) {
            (false, _) => "limit",
            (true, true) => "error",
            (true, false) => "retry",
        },
        "chat_only": app.chat_only,
        "steps_wait": app.steps_wait,
        // The other model a reply can be asked again from.
        "other_model": app.other_model().map(|(_, m)| m),
        "asks": app.asks.iter().map(crate::asks::view).collect::<Vec<_>>(),
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
        // This conversation's task board (lyra's and the agents' shared tasks).
        "board": crate::board::current(&app.session_id),
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

/// A routine was asked to run now (checked every loop, cheaply).
fn routines_wanted() -> bool {
    crate::routines::has_requests()
}

/// " · coding: Claude Code 2.1.291, OpenCode 1.18.29" for a machine that has them.
pub(crate) fn coding_agents(m: &Value) -> String {
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
        for key in ["status", "machines_detail", "pairing", "online", "server_health", "server_harnesses", "diagnoses", "users_waiting"] {
            map.remove(key);
        }
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
pub(crate) fn machines_detail(hub: &Hub, node_build: Option<&str>) -> Vec<Value> {
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

pub(crate) const RULES_USAGE: &str = "/machines rules <name|server> [on|off | allow|write|deny|ssh add|remove <value>]";

/// A machine's rules, for the terminal.
pub(crate) fn rules_text(name: &str, r: &lyra_system::Settings) -> String {
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
pub(crate) fn edit_rules(rules: &mut lyra_system::Settings, change: &str) -> Result<(), String> {
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
pub(crate) fn set_server_rules(app: &mut App, wanted: &Value) -> Result<Value, String> {
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

    #[test]
    fn members_get_their_own_pages_and_never_an_admins() {
        for own in ["sessions", "routines", "routine_save", "me", "email", "email_set", "changelog", "notes", "document_save", "templates"] {
            assert!(member_may_get(own), "{own} is a member's own");
        }
        for admin in [
            "status", "users", "user_add", "user_password", "user_email", "settings", "settings_set", "email_providers", "email_provider_put", "email_provider_test", "running",
            "running_stop", "known_down_set", "devices", "machines", "rules", "set_rules", "backups",
        ] {
            assert!(!member_may_get(admin), "{admin} is an admin's");
        }
    }

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
