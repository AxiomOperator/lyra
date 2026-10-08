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
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
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
}
