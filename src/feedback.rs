//! Feedback: bug reports and feature requests people send the admins, and
//! following them through review. Each submission has a kind, a title and
//! details, how bad it is (bugs), files (uploads: only the sender and admins
//! may open them), the lyra version and page it came from, a status (New →
//! Reviewing → Planned → Done / Won't do), a priority, the version it shipped
//! in, and a comment thread. Members see their own; admins see everyone's.
//! Kept in `~/.lyra/feedback/feedback.json`. What someone should hear about
//! (a new submission, a status change, a comment) waits in `take_notices` for
//! lyra serve to push.

use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const KINDS: &[&str] = &["bug", "feature"];
pub const STATUSES: &[&str] = &["new", "reviewing", "planned", "done", "wontdo"];
pub const PRIORITIES: &[&str] = &["low", "normal", "high", "urgent"];
pub const SEVERITIES: &[&str] = &["minor", "annoying", "blocking"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct File {
    /// The upload's id (`/api/files/<id>`).
    pub id: String,
    pub name: String,
    pub mime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Comment {
    pub by: String,
    pub name: String,
    pub admin: bool,
    pub text: String,
    pub at: DateTime<Utc>,
    /// lyra's own note (a status change), not someone's words.
    #[serde(default)]
    pub system: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Item {
    pub id: u64,
    pub kind: String,
    pub title: String,
    pub details: String,
    /// Bugs: how bad it is for them (minor, annoying, blocking).
    #[serde(default)]
    pub severity: Option<String>,
    pub status: String,
    pub priority: String,
    pub user: String,
    pub name: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    /// lyra's version, and the page they were on.
    pub version: String,
    #[serde(default)]
    pub page: String,
    #[serde(default)]
    pub files: Vec<File>,
    /// The version it shipped in (Done).
    #[serde(default)]
    pub shipped_in: Option<String>,
    #[serde(default)]
    pub comments: Vec<Comment>,
    /// Something the sender hasn't seen yet (a status change, an admin's comment).
    #[serde(default)]
    pub news_for_sender: bool,
    /// Something the admins haven't seen yet (new, or the sender's comment).
    #[serde(default)]
    pub news_for_admins: bool,
    /// lyra's read of it (made in the background after it's sent).
    #[serde(default)]
    pub analysis: Option<Analysis>,
    /// Being analyzed now.
    #[serde(default)]
    pub analyzing: bool,
}

/// What lyra makes of a submission: a summary for everyone; the likely cause
/// and possible fixes (bugs) or a possible implementation (features), the
/// size and questions to ask, for the admins.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Analysis {
    pub summary: String,
    /// Bugs: what's probably behind it.
    #[serde(default)]
    pub cause: Option<String>,
    /// Possible fixes (bugs) or implementation steps (features).
    #[serde(default)]
    pub approach: Vec<String>,
    /// small, medium or large.
    #[serde(default)]
    pub effort: Option<String>,
    /// The part of lyra it's about (chat, calendar, memory, the app …).
    #[serde(default)]
    pub area: Option<String>,
    /// What to ask the sender.
    #[serde(default)]
    pub questions: Vec<String>,
    pub at: DateTime<Utc>,
    #[serde(default)]
    pub error: Option<String>,
}

/// The chat model's address, for analyses (set when lyra starts).
static MODEL: Mutex<Option<(String, String)>> = Mutex::new(None);

pub fn configure(url: &str, model: &str) {
    *MODEL.lock().unwrap_or_else(|e| e.into_inner()) = Some((format!("{}/chat/completions", url.trim_end_matches('/')), model.to_string()));
}

const README: &str = include_str!("../README.md");

/// The parts of lyra's README most about this (by shared words), for context.
fn background(text: &str) -> String {
    let words: Vec<String> = text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 3).map(str::to_string).collect();
    let mut sections: Vec<(usize, &str)> = README
        .split("\n### ")
        .map(|s| {
            let low = s.to_lowercase();
            (words.iter().filter(|w| low.contains(w.as_str())).count(), s)
        })
        .collect();
    sections.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    sections.iter().filter(|(n, _)| *n > 0).take(3).map(|(_, s)| s.chars().take(3000).collect::<String>()).collect::<Vec<_>>().join("\n\n### ")
}

const ANALYST: &str = "You help the admins of lyra, a self-hosted AI assistant (a Rust server with a web app, \
memory, skills, agents, plans, machines, Outlook and Teams, PMI tasks), triage what users send them. \
Read the bug report or feature request and lyra's documentation excerpts, then answer with only a JSON object:
{\"summary\": \"what they're reporting or asking for, in two or three plain sentences\",
 \"cause\": \"bugs only: what's most likely behind it, or null\",
 \"approach\": [\"bugs: possible fixes, most likely first; features: implementation steps, in order\"],
 \"effort\": \"small | medium | large\",
 \"area\": \"the part of lyra it's about, in a few words\",
 \"questions\": [\"what to ask the sender if something's unclear (may be empty)\"]}
Be concrete and brief: 2 to 5 approach items. Say what you'd check or change, not code. Don't invent features lyra doesn't have.";

/// Ask the model about it (blocking): the item, what's attached, lyra's docs.
fn analyze_now(item: &Item, seen: &[String]) -> Analysis {
    let now = Utc::now();
    let Some((url, model)) = MODEL.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        return Analysis { at: now, error: Some("no model".into()), ..Default::default() };
    };
    let mut prompt = format!(
        "{}: {}\n\n{}\n\nFrom: {} · lyra {} · page: {}{}\n",
        if item.kind == "bug" { "Bug report" } else { "Feature request" },
        item.title,
        if item.details.trim().is_empty() { "(no details)" } else { item.details.trim() },
        item.name,
        item.version,
        if item.page.is_empty() { "?" } else { &item.page },
        item.severity.as_deref().map(|s| format!(" · how bad: {s}")).unwrap_or_default(),
    );
    for s in seen {
        prompt += &format!("\nWhat an attached screenshot shows: {s}\n");
    }
    let replies: Vec<String> = item.comments.iter().filter(|c| !c.system).map(|c| format!("{}: {}", c.name, c.text)).collect();
    if !replies.is_empty() {
        prompt += &format!("\nThe conversation since:\n{}\n", replies.join("\n"));
    }
    prompt += &format!("\nlyra's documentation (the most related parts):\n### {}", background(&format!("{} {}", item.title, item.details)));
    let reply = crate::learn::complete(&url, &model, ANALYST, &prompt).map(|(r, _)| r);
    let parsed = reply.and_then(|r| {
        let start = r.find('{').ok_or("no answer")?;
        let end = r.rfind('}').ok_or("no answer")?;
        serde_json::from_str::<Value>(&r[start..=end]).map_err(|e| e.to_string())
    });
    match parsed {
        Ok(v) => {
            let list = |k: &str| v[k].as_array().into_iter().flatten().filter_map(|x| x.as_str()).map(str::to_string).filter(|x| !x.trim().is_empty()).take(6).collect::<Vec<_>>();
            let text = |k: &str| v[k].as_str().map(str::trim).filter(|x| !x.is_empty() && *x != "null").map(str::to_string);
            Analysis {
                summary: text("summary").unwrap_or_default(),
                cause: text("cause").filter(|_| item.kind == "bug"),
                approach: list("approach"),
                effort: text("effort").map(|e| e.to_lowercase()).filter(|e| ["small", "medium", "large"].contains(&e.as_str())),
                area: text("area"),
                questions: list("questions"),
                at: now,
                error: None,
            }
        }
        Err(e) => Analysis { at: now, error: Some(e), ..Default::default() },
    }
}

/// Analyze it in the background (for the sender: their usage), and keep the result.
/// `seen`: what attached pictures show, if anything read them already.
pub fn analyze(id: u64, user: &str, files: Vec<(String, std::path::PathBuf)>) {
    {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = load();
        if let Some(i) = all.iter_mut().find(|i| i.id == id) {
            i.analyzing = true;
            let _ = save(&all);
        }
    }
    let user = user.to_string();
    std::thread::spawn(move || {
        crate::acting::set(&user);
        let Some(item) = load().into_iter().find(|i| i.id == id) else { return };
        // Screenshots: what they show, when a vision model can look.
        let seen: Vec<String> = if crate::vision::available() {
            files.iter().filter(|(name, _)| crate::vision::image_type(name).is_some()).take(3).filter_map(|(name, path)| std::fs::read(path).ok().and_then(|b| crate::vision::read(name, &b, Some("Describe what this screenshot shows, any error text exactly, and which page or screen it is.")).ok())).collect()
        } else {
            vec![]
        };
        let a = analyze_now(&item, &seen);
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = load();
        if let Some(i) = all.iter_mut().find(|i| i.id == id) {
            i.analysis = Some(a);
            i.analyzing = false;
            let _ = save(&all);
        }
    });
}

const ENHANCER: &str = "You help someone write a clearer bug report or feature request for lyra, a self-hosted \
AI assistant. Rewrite their description so it's clear and well organised, keeping their meaning and their \
facts exactly. Never invent details they didn't give: no steps, errors, devices, versions or numbers of your own. \
Where something useful is missing, add a short line in square brackets saying what to add, e.g. [Which page were \
you on?]. Bugs: what they did, what happened, what they expected. Features: what they'd like, what for, how they \
picture it. Plain sentences or short lists; no headings, no greeting, no sign-off. Answer with only the rewritten description.";

/// A clearer draft of what someone is writing (the form's Enhance button), by the chat model.
pub fn enhance(kind: &str, title: &str, details: &str) -> Result<String, String> {
    let details = details.trim();
    if details.chars().count() < 5 {
        return Err("write a little first, then lyra can make it clearer".into());
    }
    let (url, model) = MODEL.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or("lyra's model isn't set up")?;
    let prompt = format!(
        "{}{}\n\nTheir description:\n{}",
        if kind == "bug" { "A bug report" } else { "A feature request" },
        if title.trim().is_empty() { String::new() } else { format!(" titled \"{}\"", title.trim()) },
        details.chars().take(6000).collect::<String>()
    );
    let (reply, _) = crate::learn::complete(&url, &model, ENHANCER, &prompt)?;
    let text = reply.trim().trim_matches('"').trim();
    // Thinking models: only what follows their thoughts.
    let text = text.rsplit_once("</think>").map_or(text, |(_, t)| t).trim();
    if text.is_empty() {
        return Err("lyra didn't come up with anything; try again".into());
    }
    Ok(text.to_string())
}

/// What `who` gets of an item: everything for admins; the sender sees the
/// summary of lyra's read, not the admins' notes (cause, fixes, questions).
pub fn view(item: &Item, who: &Who) -> Value {
    let mut v = serde_json::to_value(item).unwrap_or(Value::Null);
    if !who.admin {
        v["analysis"] = item.analysis.as_ref().filter(|a| !a.summary.is_empty()).map_or(Value::Null, |a| json!({ "summary": a.summary, "at": a.at }));
    }
    v
}

/// Changes so far (the app looks again when it moves).
pub fn revision() -> u64 {
    REVISION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Who's acting.
#[derive(Debug, Clone)]
pub struct Who {
    pub user: String,
    pub name: String,
    pub admin: bool,
}

/// A push for lyra serve to send: to the admins, or to one person.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub to_admins: bool,
    pub user: Option<String>,
    pub title: String,
    pub body: String,
}

static LOCK: Mutex<()> = Mutex::new(());
static REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static NOTICES: Mutex<Vec<Notice>> = Mutex::new(Vec::new());

fn notify(n: Notice) {
    NOTICES.lock().unwrap_or_else(|e| e.into_inner()).push(n);
}

/// What should be pushed now (lyra serve takes them).
pub fn take_notices() -> Vec<Notice> {
    std::mem::take(&mut *NOTICES.lock().unwrap_or_else(|e| e.into_inner()))
}

#[cfg(test)]
thread_local! {
    /// A test's own file (tests run side by side).
    static TEST_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

fn path() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(d) = TEST_DIR.with(|d| d.borrow().clone()) {
        return Some(d.join("feedback.json"));
    }
    Some(crate::config::home()?.join("feedback").join("feedback.json"))
}

fn load() -> Vec<Item> {
    path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(all: &[Item]) -> Result<(), String> {
    let p = path().ok_or("no home directory")?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(all).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())?;
    REVISION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

fn kind_word(k: &str) -> &'static str {
    if k == "bug" { "Bug report" } else { "Feature request" }
}

pub fn status_word(s: &str) -> &'static str {
    match s {
        "new" => "New",
        "reviewing" => "Reviewing",
        "planned" => "Planned",
        "done" => "Done",
        "wontdo" => "Won't do",
        _ => "?",
    }
}

/// Send one in.
pub fn submit(who: &Who, kind: &str, title: &str, details: &str, severity: Option<&str>, files: Vec<File>, page: &str) -> Result<Item, String> {
    let kind = kind.trim().to_lowercase();
    if !KINDS.contains(&kind.as_str()) {
        return Err("a bug report or a feature request?".into());
    }
    let title = title.trim();
    if title.len() < 3 {
        return Err("give it a short title".into());
    }
    let severity = severity.map(str::to_lowercase).filter(|s| SEVERITIES.contains(&s.as_str()));
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = load();
    let now = Utc::now();
    let item = Item {
        id: all.iter().map(|i| i.id).max().unwrap_or(0) + 1,
        kind: kind.clone(),
        title: title.chars().take(140).collect(),
        details: details.trim().chars().take(8000).collect(),
        priority: match severity.as_deref() {
            Some("blocking") => "high".into(),
            _ => "normal".into(),
        },
        severity: if kind == "bug" { severity } else { None },
        status: "new".into(),
        user: who.user.clone(),
        name: who.name.clone(),
        created: now,
        updated: now,
        version: crate::changelog::version().to_string(),
        page: page.trim().chars().take(60).collect(),
        files: files.into_iter().take(10).collect(),
        shipped_in: None,
        comments: vec![],
        news_for_sender: false,
        news_for_admins: true,
        analysis: None,
        analyzing: false,
    };
    all.push(item.clone());
    save(&all)?;
    notify(Notice { to_admins: true, user: None, title: format!("{} #{}: {}", kind_word(&item.kind), item.id, item.title), body: format!("from {}{}", item.name, item.severity.as_deref().map(|s| format!(" · {s}")).unwrap_or_default()) });
    Ok(item)
}

/// What `who` may see: their own, or (admins) everyone's. Newest change first.
pub fn list(who: &Who) -> Vec<Item> {
    let mut v: Vec<Item> = load().into_iter().filter(|i| who.admin || i.user == who.user).collect();
    v.sort_by_key(|i| std::cmp::Reverse(i.updated));
    v
}

fn find<'a>(all: &'a mut [Item], id: u64, who: &Who) -> Result<&'a mut Item, String> {
    all.iter_mut().find(|i| i.id == id && (who.admin || i.user == who.user)).ok_or_else(|| format!("no feedback #{id}"))
}

/// They've looked: what was new for them isn't any more.
pub fn seen(who: &Who, id: u64) -> Result<(), String> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = load();
    let item = find(&mut all, id, who)?;
    let mut changed = false;
    if item.user == who.user && item.news_for_sender {
        item.news_for_sender = false;
        changed = true;
    }
    if who.admin && item.news_for_admins {
        item.news_for_admins = false;
        changed = true;
    }
    if changed {
        save(&all)?;
    }
    Ok(())
}

/// Something to say on it.
pub fn comment(who: &Who, id: u64, text: &str) -> Result<Item, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("say something".into());
    }
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = load();
    let item = find(&mut all, id, who)?;
    let now = Utc::now();
    item.comments.push(Comment { by: who.user.clone(), name: who.name.clone(), admin: who.admin, text: text.chars().take(4000).collect(), at: now, system: false });
    item.updated = now;
    let mine = item.user == who.user;
    if mine {
        item.news_for_admins = true;
    } else {
        item.news_for_sender = true;
    }
    let out = item.clone();
    save(&all)?;
    if mine {
        notify(Notice { to_admins: true, user: None, title: format!("{} commented on #{}", who.name, out.id), body: text.chars().take(160).collect() });
    } else {
        notify(Notice { to_admins: false, user: Some(out.user.clone()), title: format!("{} replied on your {} #{}", who.name, kind_word(&out.kind).to_lowercase(), out.id), body: text.chars().take(160).collect() });
    }
    Ok(out)
}

/// An admin moves it along: status, priority, the version it shipped in.
pub fn update(who: &Who, id: u64, status: Option<&str>, priority: Option<&str>, shipped_in: Option<&str>) -> Result<Item, String> {
    if !who.admin {
        return Err("only an admin changes its status".into());
    }
    if let Some(s) = status.filter(|s| !STATUSES.contains(s)) {
        return Err(format!("{s:?}: new, reviewing, planned, done or wontdo"));
    }
    if let Some(p) = priority.filter(|p| !PRIORITIES.contains(p)) {
        return Err(format!("{p:?}: low, normal, high or urgent"));
    }
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = load();
    let item = find(&mut all, id, who)?;
    let now = Utc::now();
    let mut said = Vec::new();
    if let Some(s) = status.filter(|s| *s != item.status) {
        said.push(format!("{} → {}", status_word(&item.status), status_word(s)));
        item.status = s.to_string();
        // Done ships in this version unless they said otherwise.
        if s == "done" && item.shipped_in.is_none() && shipped_in.is_none() {
            item.shipped_in = Some(crate::changelog::version().to_string());
        }
    }
    if let Some(p) = priority.filter(|p| *p != item.priority) {
        said.push(format!("priority {p}"));
        item.priority = p.to_string();
    }
    if let Some(v) = shipped_in.map(str::trim).filter(|v| !v.is_empty()) {
        said.push(format!("shipped in {v}"));
        item.shipped_in = Some(v.to_string());
    }
    if said.is_empty() {
        return Ok(item.clone());
    }
    let note = said.join(" · ");
    item.comments.push(Comment { by: who.user.clone(), name: who.name.clone(), admin: true, text: note.clone(), at: now, system: true });
    item.updated = now;
    // The sender hears about status changes (not a priority shuffle).
    let tell = status.is_some();
    if tell && item.user != who.user {
        item.news_for_sender = true;
    }
    let out = item.clone();
    save(&all)?;
    if tell && out.user != who.user {
        let shipped = out.shipped_in.as_deref().filter(|_| out.status == "done").map(|v| format!(" · in lyra {v}")).unwrap_or_default();
        notify(Notice { to_admins: false, user: Some(out.user.clone()), title: format!("Your {} #{}: {}", kind_word(&out.kind).to_lowercase(), out.id, status_word(&out.status)), body: format!("{}{shipped}", out.title) });
    }
    Ok(out)
}

/// How many have news for them (the rail's badge): new ones for admins,
/// replies and status changes on their own for everyone.
pub fn badge(who: &Who) -> usize {
    load().iter().filter(|i| (who.admin && i.news_for_admins) || (i.user == who.user && i.news_for_sender)).count()
}

// ---- from the chat

pub fn capabilities() -> Vec<Capability> {
    let mut c = Capability::new(
        "feedback_submit",
        CapabilityKind::NativeTool,
        "Send the admins a bug report or a feature request for lyra, for the user: a short title and the details (what happened and what they expected, or what they'd like and why). They follow it on the Feedback page.",
        RiskLevel::LowWrite,
    );
    c.input_schema = json!({
        "type": "object",
        "properties": {
            "kind": { "type": "string", "enum": ["bug", "feature"] },
            "title": { "type": "string" },
            "details": { "type": "string" },
            "severity": { "type": "string", "enum": ["minor", "annoying", "blocking"], "description": "Bugs: how bad it is for them." },
        },
        "required": ["kind", "title", "details"],
    });
    c.source = "feedback".into();
    c.tags = ["bug", "report", "feature", "request", "feedback", "suggestion", "broken", "idea", "admin"].iter().map(|t| t.to_string()).collect();
    let mut l = Capability::new("feedback_list", CapabilityKind::NativeTool, "The user's bug reports and feature requests and where each stands (admins: everyone's).", RiskLevel::ReadOnly);
    l.input_schema = json!({ "type": "object", "properties": {}, "required": [] });
    l.source = "feedback".into();
    l.tags = c.tags.clone();
    vec![c, l]
}

/// The person this turn is for, as feedback sees them.
fn current() -> Who {
    let user = crate::acting::current();
    let users = lyra_web::Users::open(&crate::config::home().unwrap_or_default().join("web"));
    match users.get(&user) {
        Some(u) => Who { name: u.name.clone(), admin: u.role == lyra_web::Role::Admin, user },
        None => Who { name: "the owner".into(), admin: crate::acting::is_owner(&user), user },
    }
}

pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    let who = current();
    match name {
        "feedback_submit" => {
            let i = submit(&who, args["kind"].as_str().unwrap_or(""), args["title"].as_str().unwrap_or(""), args["details"].as_str().unwrap_or(""), args["severity"].as_str(), vec![], "chat")?;
            analyze(i.id, &who.user, vec![]);
            Ok(json!({ "sent": format!("{} #{}", kind_word(&i.kind), i.id), "title": i.title, "note": "the admins were told; the user follows it on the Feedback page" }))
        }
        "feedback_list" => Ok(json!({ "items": list(&who).iter().take(30).map(|i| json!({ "id": i.id, "kind": i.kind, "title": i.title, "status": status_word(&i.status), "from": i.name, "shipped_in": i.shipped_in })).collect::<Vec<_>>() })),
        other => Err(format!("{other} isn't a feedback tool")),
    }
}

/// `/feedback`: what's open (yours, or everyone's for admins).
pub fn command(who: &Who) -> String {
    let all = list(who);
    if all.is_empty() {
        return "No bug reports or feature requests yet (the app's Feedback page, or tell lyra \"report a bug: …\").".into();
    }
    all.iter()
        .map(|i| format!("#{} [{}] {} · {} · {}{}", i.id, status_word(&i.status), kind_word(&i.kind), i.title, i.name, if i.comments.iter().any(|c| !c.system) { format!(" · {} comments", i.comments.iter().filter(|c| !c.system).count()) } else { String::new() }))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submissions_are_followed_by_their_sender_and_the_admins() {
        let dir = std::env::temp_dir().join(format!("lyra-feedback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        TEST_DIR.with(|d| *d.borrow_mut() = Some(dir.clone()));
        let dana = Who { user: "dana".into(), name: "Dana".into(), admin: false };
        let juan = Who { user: "juan".into(), name: "Juan".into(), admin: false };
        let admin = Who { user: "owner".into(), name: "Garrett".into(), admin: true };
        let i = submit(&dana, "bug", "Calendar shows yesterday", "Open Today; it shows Tuesday.", Some("blocking"), vec![], "status").unwrap();
        assert_eq!((i.id, i.status.as_str(), i.priority.as_str(), i.severity.as_deref()), (1, "new", "high", Some("blocking")));
        assert!(take_notices().iter().any(|n| n.to_admins && n.title.contains("Bug report #1")));
        assert_eq!(badge(&admin), 1);
        // Only Dana and the admins see it.
        assert_eq!(list(&juan).len(), 0);
        assert!(comment(&juan, 1, "me too").is_err());
        assert!(update(&dana, 1, Some("done"), None, None).is_err(), "only an admin moves it along");
        // The admin moves it along; Dana hears about it.
        seen(&admin, 1).unwrap();
        let i = update(&admin, 1, Some("planned"), Some("urgent"), None).unwrap();
        assert_eq!((i.status.as_str(), i.priority.as_str()), ("planned", "urgent"));
        assert!(take_notices().iter().any(|n| n.user.as_deref() == Some("dana") && n.title.contains("Planned")));
        assert_eq!(badge(&dana), 1);
        seen(&dana, 1).unwrap();
        assert_eq!(badge(&dana), 0);
        comment(&dana, 1, "Still happens on my phone").unwrap();
        assert!(take_notices().iter().any(|n| n.to_admins));
        let i = update(&admin, 1, Some("done"), None, None).unwrap();
        assert_eq!(i.shipped_in.as_deref(), Some(crate::changelog::version()), "done ships in this version");
        assert!(submit(&dana, "praise", "x", "", None, vec![], "").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sender_sees_the_summary_and_admins_the_rest() {
        let now = Utc::now();
        let mut i = Item {
            id: 7, kind: "bug".into(), title: "Calendar".into(), details: String::new(), severity: None, status: "new".into(), priority: "normal".into(),
            user: "dana".into(), name: "Dana".into(), created: now, updated: now, version: "0".into(), page: String::new(), files: vec![], shipped_in: None,
            comments: vec![], news_for_sender: false, news_for_admins: false, analysis: None, analyzing: false,
        };
        i.analysis = Some(Analysis { summary: "Today lists yesterday.".into(), cause: Some("a stale day cache".into()), approach: vec!["refresh at midnight".into()], questions: vec!["which device?".into()], at: now, ..Default::default() });
        let dana = Who { user: "dana".into(), name: "Dana".into(), admin: false };
        let admin = Who { user: "owner".into(), name: "G".into(), admin: true };
        let mine = view(&i, &dana);
        assert_eq!(mine["analysis"]["summary"], "Today lists yesterday.");
        assert!(mine["analysis"].get("cause").is_none() && mine["analysis"].get("approach").is_none() && mine["analysis"].get("questions").is_none());
        assert_eq!(view(&i, &admin)["analysis"]["cause"], "a stale day cache");
        // The docs it's given are the parts about it.
        assert!(background("the calendar shows yesterday's meetings").to_lowercase().contains("calendar"));
    }
}
