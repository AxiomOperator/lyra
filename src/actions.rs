//! What lyra did for each person, and why: every change a tool made (a task
//! created, a mail flagged or drafted, a meeting moved, a file written, a
//! memory kept), with what prompted it (their message, mail triage, a
//! routine…), the skills that were in play, and whether they approved it.
//! Kept per person (`actions/<YYYY-MM>.jsonl` in their files) for the "What
//! lyra knows about me" page and its Why?.

use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// What prompted the work going on in this thread.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Why {
    /// "chat", "mail triage", "follow-up", "routine", "plan", "meeting follow-up"…
    pub source: String,
    /// Their message, the email's subject, the routine's name.
    #[serde(default)]
    pub detail: String,
    /// The skills lyra was using.
    #[serde(default)]
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Action {
    pub at: DateTime<Utc>,
    /// The tool (`pmi_add_task`), or what lyra did by itself.
    pub tool: String,
    /// What it did, in a line.
    pub what: String,
    pub why: Why,
    /// They said yes to it (it asked); false: it didn't need asking.
    #[serde(default)]
    pub approved: bool,
    /// The agent that did it, when it wasn't lyra itself.
    #[serde(default)]
    pub agent: Option<String>,
}

thread_local! {
    static WHY: RefCell<Option<Why>> = const { RefCell::new(None) };
}

/// Say what prompted the work this thread does next (until it's said again).
pub fn because(why: Why) {
    WHY.with(|w| *w.borrow_mut() = Some(why));
}

/// The same, unless this thread already said (a plan step inside a chat turn keeps the chat's).
pub fn because_if_unset(why: Why) {
    WHY.with(|w| {
        if w.borrow().is_none() {
            *w.borrow_mut() = Some(why);
        }
    });
}

fn current_why() -> Why {
    WHY.with(|w| w.borrow().clone()).unwrap_or_else(|| Why { source: "lyra".into(), ..Default::default() })
}

fn dir(user: &str) -> Option<PathBuf> {
    if user == lyra_web::users::OWNER { Some(crate::config::home()?.join("actions")) } else { Some(crate::context::user_dir(user)?.join("actions")) }
}

static WRITE: Mutex<()> = Mutex::new(());

fn append(user: &str, a: &Action) {
    if cfg!(test) {
        return;
    }
    let Some(d) = dir(user) else { return };
    let _held = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::create_dir_all(&d);
    if let (Ok(mut f), Ok(line)) = (std::fs::OpenOptions::new().create(true).append(true).open(d.join(format!("{}.jsonl", a.at.format("%Y-%m")))), serde_json::to_string(a)) {
        let _ = writeln!(f, "{line}");
    }
}

/// A line for what a tool did, from its arguments.
pub fn describe(tool: &str, args: &Value) -> String {
    let label = tool.replace('_', " ");
    let pick = ["title", "subject", "name", "path", "task", "text", "content", "folder", "query", "id"].iter().find_map(|k| args[*k].as_str().filter(|s| !s.trim().is_empty()));
    match pick {
        Some(s) => format!("{label}: {}", s.chars().take(100).collect::<String>()),
        None => label,
    }
}

/// A tool changed something, as the person this thread works for.
pub fn record_tool(tool: &str, args: &Value, approved: bool, agent: Option<&str>) {
    let user = crate::acting::current();
    append(&user, &Action { at: Utc::now(), tool: tool.into(), what: describe(tool, args), why: current_why(), approved, agent: agent.map(str::to_string) });
}

/// lyra did something by itself (mail triage made a task, flagged a mail…).
pub fn record(tool: &str, what: &str, why: Why) {
    let user = crate::acting::current();
    append(&user, &Action { at: Utc::now(), tool: tool.into(), what: what.chars().take(200).collect(), why, approved: false, agent: None });
}

/// Their latest actions, newest first (this month and last).
pub fn recent(user: &str, limit: usize) -> Vec<Action> {
    let Some(d) = dir(user) else { return vec![] };
    let now = Utc::now();
    let last = now.with_day(1).map_or(now, |m| m - chrono::Duration::days(1));
    let mut all: Vec<Action> = [last, now]
        .iter()
        .filter_map(|m| std::fs::read_to_string(d.join(format!("{}.jsonl", m.format("%Y-%m")))).ok())
        .flat_map(|t| t.lines().filter_map(|l| serde_json::from_str::<Action>(l).ok()).collect::<Vec<_>>())
        .collect();
    all.sort_by_key(|a| std::cmp::Reverse(a.at));
    all.dedup_by(|a, b| a.at == b.at && a.what == b.what);
    all.truncate(limit);
    all
}

/// Everything kept about them, for Export.
pub fn all(user: &str) -> Vec<Value> {
    let Some(d) = dir(user) else { return vec![] };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&d).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")).collect();
    files.sort();
    files.iter().filter_map(|p| std::fs::read_to_string(p).ok()).flat_map(|t| t.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect::<Vec<_>>()).collect()
}

/// Forget what lyra did for them (the record, not the changes themselves).
pub fn clear(user: &str) -> Result<(), String> {
    match dir(user) {
        Some(d) if d.exists() => std::fs::remove_dir_all(&d).map_err(|e| e.to_string()),
        _ => Ok(()),
    }
}

/// The page's view of one.
pub fn view(a: &Action) -> Value {
    json!({ "at": a.at, "tool": a.tool, "what": a.what, "why": a.why, "approved": a.approved, "agent": a.agent })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn why_is_per_thread_and_tools_read_as_a_line() {
        because(Why { source: "chat".into(), detail: "remind me to call Dana".into(), skills: vec!["reminders".into()] });
        assert_eq!(current_why().detail, "remind me to call Dana");
        std::thread::spawn(|| assert_eq!(current_why().source, "lyra", "another thread has its own")).join().unwrap();
        assert_eq!(describe("pmi_add_task", &json!({ "title": "Call Dana", "due": "friday" })), "pmi add task: Call Dana");
        assert_eq!(describe("mail_tidy", &json!({})), "mail tidy");
    }
}
