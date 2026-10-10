//! The task board: one per conversation, shared by lyra and the agents
//! working in it (`boards/<session>.json`). Anyone working there can post a
//! task (for a named agent or for anyone), leave a note for another agent,
//! and mark a task done with its result. An agent handed work sees what's on
//! the board for it; a delegation that takes a board task (`board_task`)
//! marks it working, then done or failed with the result. The main agent
//! stays in charge: it reads the board and decides what to hand out next.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The most tasks one board keeps (the oldest finished ones go first).
const MOST: usize = 60;

/// Bumped on every change, so a view is read again only after one.
static REV: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
/// Each board's last view (none: empty), with the change it was made at.
type Views = std::collections::HashMap<String, (u64, Option<Value>)>;
static VIEWS: std::sync::Mutex<Option<Views>> = std::sync::Mutex::new(None);

fn changed() {
    REV.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Open,
    Working,
    Done,
    Failed,
    /// A message for an agent (nothing to do but read it).
    Note,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Working => "working",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Note => "note",
        }
    }

    fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "open" => Status::Open,
            "working" => Status::Working,
            "done" => Status::Done,
            "failed" => Status::Failed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: u32,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The agent it's for (none: anyone, or lyra itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Who posted it ("lyra", or an agent's name).
    pub by: String,
    pub status: Status,
    /// Who's on it, or finished it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_by: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub result: String,
    pub at: DateTime<Utc>,
    pub updated: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Board {
    pub tasks: Vec<Task>,
}

fn dir() -> Option<PathBuf> {
    Some(crate::config::home()?.join("boards"))
}

fn path(session: &str) -> Option<PathBuf> {
    let s = session.trim();
    (!s.is_empty() && s.len() <= 80 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then_some(())?;
    Some(dir()?.join(format!("{s}.json")))
}

fn store(session: &str) -> Result<crate::store::JsonStore<Board>, String> {
    Ok(crate::store::JsonStore::new(path(session).ok_or("this conversation has no board")?))
}

/// The board of a conversation (empty when nothing's been posted).
pub fn read(session: &str) -> Board {
    path(session).map(|p| crate::store::read_json(&p)).unwrap_or_default()
}

fn clip(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

/// Post a task (or, with `note`, a message) on the board.
pub fn post(session: &str, by: &str, title: &str, detail: &str, agent: Option<&str>, note: bool) -> Result<Task, String> {
    let title = clip(title, 200);
    if title.is_empty() {
        return Err("a task needs a title".into());
    }
    store(session)?.update(|b| {
        let now = Utc::now();
        let id = b.tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        let t = Task {
            id,
            title,
            detail: clip(detail, 2000),
            agent: agent.map(str::trim).filter(|a| !a.is_empty()).map(str::to_string),
            by: by.to_string(),
            status: if note { Status::Note } else { Status::Open },
            taken_by: None,
            result: String::new(),
            at: now,
            updated: now,
        };
        b.tasks.push(t.clone());
        changed();
        // Too many: the oldest finished ones (then notes) go.
        while b.tasks.len() > MOST {
            match b.tasks.iter().position(|t| matches!(t.status, Status::Done | Status::Failed | Status::Note)) {
                Some(i) => b.tasks.remove(i),
                None => b.tasks.remove(0),
            };
        }
        t
    })
}

/// Change a task's status (and its result): who's on it, or how it went.
pub fn update(session: &str, by: &str, id: u32, status: &str, result: &str) -> Result<Task, String> {
    let status = Status::parse(status.trim()).ok_or("status is open, working, done or failed")?;
    store(session)?
        .update(|b| {
            let t = b.tasks.iter_mut().find(|t| t.id == id && t.status != Status::Note)?;
            t.status = status;
            t.taken_by = Some(by.to_string());
            if !result.trim().is_empty() {
                t.result = clip(result, 2000);
            }
            t.updated = Utc::now();
            changed();
            Some(t.clone())
        })?
        .ok_or_else(|| format!("there's no task {id} on the board"))
}

/// What's on the board for `agent`: open tasks for it or for anyone, and notes to it.
pub fn for_agent(session: &str, agent: &str) -> Vec<Task> {
    let mine = |t: &Task| t.agent.as_deref().is_none_or(|a| a.eq_ignore_ascii_case(agent));
    read(session).tasks.into_iter().filter(|t| matches!(t.status, Status::Open | Status::Note) && mine(t) && !t.by.eq_ignore_ascii_case(agent)).collect()
}

fn brief(t: &Task) -> Value {
    json!({
        "id": t.id, "title": t.title, "detail": t.detail, "for": t.agent, "by": t.by, "status": t.status.as_str(),
        "taken_by": t.taken_by, "result": t.result,
    })
}

/// The board for an agent's context (none when nothing's there).
pub fn context(session: &str, agent: &str) -> Option<Value> {
    let tasks = for_agent(session, agent);
    (!tasks.is_empty()).then(|| json!(tasks.iter().map(brief).collect::<Vec<_>>()))
}

pub const TOOLS: [&str; 3] = ["board_read", "board_post", "board_update"];

/// The board's tools, for lyra and every agent working in the conversation.
pub fn definitions() -> Vec<Value> {
    let def = |name: &str, description: &str, properties: Value, required: &[&str]| {
        json!({ "type": "function", "function": { "name": name, "description": description, "parameters": { "type": "object", "properties": properties, "required": required } } })
    };
    vec![
        def("board_read", "This conversation's shared task board: tasks lyra and the agents posted, who's on them, and their results.", json!({}), &[]),
        def(
            "board_post",
            "Put a task on this conversation's task board, for a named agent or for anyone; or, with note, leave a message for another agent. The main agent hands open tasks out (delegate with board_task).",
            json!({
                "title": { "type": "string", "description": "What's to be done, in a line." },
                "detail": { "type": "string", "description": "Anything the one doing it needs." },
                "agent": { "type": "string", "description": "The agent it's for (empty: anyone)." },
                "note": { "type": "boolean", "description": "A message to read, not a task." },
            }),
            &["title"],
        ),
        def(
            "board_update",
            "Mark a task on the board as working, done or failed, with its result.",
            json!({
                "id": { "type": "integer" },
                "status": { "type": "string", "enum": ["open", "working", "done", "failed"] },
                "result": { "type": "string", "description": "What came of it." },
            }),
            &["id", "status"],
        ),
    ]
}

/// One of the board's tools, called by `by` ("lyra" or an agent's name).
pub fn call(session: &str, by: &str, name: &str, args: &str) -> String {
    let a: Value = serde_json::from_str(args).unwrap_or(json!({}));
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let out = match name {
        "board_read" => Ok(json!({ "tasks": read(session).tasks.iter().map(brief).collect::<Vec<_>>() })),
        "board_post" => post(session, by, &s("title"), &s("detail"), a["agent"].as_str(), a["note"] == true).map(|t| json!({ "posted": t.id, "for": t.agent })),
        "board_update" => update(session, by, a["id"].as_u64().unwrap_or(0) as u32, &s("status"), &s("result")).map(|t| json!({ "task": t.id, "status": t.status.as_str() })),
        other => Err(format!("{other} isn't a board tool")),
    };
    out.unwrap_or_else(|e| json!({ "error": e })).to_string()
}

/// A delegation took a board task: it's on it now.
pub fn taken(session: &str, id: u32, agent: &str) -> Result<Task, String> {
    update(session, agent, id, "working", "")
}

/// For the app and the panel: every task, newest last.
pub fn view(session: &str) -> Value {
    let b = read(session);
    let count = |s: Status| b.tasks.iter().filter(|t| t.status == s).count();
    json!({
        "tasks": b.tasks.iter().map(|t| json!({
            "id": t.id, "title": t.title, "detail": t.detail, "agent": t.agent, "by": t.by, "status": t.status.as_str(),
            "taken_by": t.taken_by, "result": t.result, "updated": t.updated,
        })).collect::<Vec<_>>(),
        "open": count(Status::Open), "working": count(Status::Working), "done": count(Status::Done), "failed": count(Status::Failed),
    })
}

/// The view for a conversation's status (none while its board is empty),
/// read again only after a change.
pub fn current(session: &str) -> Option<Value> {
    let rev = REV.load(std::sync::atomic::Ordering::Relaxed);
    let mut views = VIEWS.lock().unwrap_or_else(|e| e.into_inner());
    let map = views.get_or_insert_with(Default::default);
    if let Some((at, v)) = map.get(session)
        && *at == rev
    {
        return v.clone();
    }
    let v = (!read(session).tasks.is_empty()).then(|| view(session));
    if map.len() > 200 {
        map.clear();
    }
    map.insert(session.to_string(), (rev, v.clone()));
    v
}

/// For the TUI's Agent panel: "2 open · 1 working · 3 done".
pub fn line(session: &str) -> Option<String> {
    let v = current(session)?;
    let parts: Vec<String> = ["open", "working", "done", "failed"]
        .into_iter()
        .filter_map(|w| v[w].as_u64().filter(|n| *n > 0).map(|n| format!("{n} {w}")))
        .collect();
    Some(if parts.is_empty() { "notes only".into() } else { parts.join(" · ") })
}

/// Boards whose conversation is gone (removed or tidied away).
pub fn tidy(home: &std::path::Path) -> usize {
    let sessions = home.join("sessions");
    let mut n = 0;
    for e in std::fs::read_dir(home.join("boards")).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if let Some(id) = name.strip_suffix(".json")
            && !sessions.join(format!("{id}.json")).exists()
            && std::fs::remove_file(e.path()).is_ok()
        {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_post_take_and_finish_tasks_and_leave_each_other_notes() {
        crate::config::with_test_home(|_| {
            let s = "conv-1";
            let t = post(s, "lyra", "Find last year's tax rate", "", Some("researcher"), false).unwrap();
            post(s, "researcher", "Rewrite the summary when the numbers are in", "", Some("writer"), false).unwrap();
            post(s, "researcher", "The 2025 rate is on page 4", "", Some("writer"), true).unwrap();
            // Each sees what's for it, not what it posted itself.
            assert_eq!(for_agent(s, "researcher").iter().map(|t| t.id).collect::<Vec<_>>(), [1]);
            assert_eq!(for_agent(s, "writer").len(), 2, "its task and the note");
            taken(s, t.id, "researcher").unwrap();
            assert!(for_agent(s, "researcher").is_empty(), "taken: not open any more");
            let done = update(s, "researcher", t.id, "done", "2.1%").unwrap();
            assert_eq!((done.status, done.result.as_str(), done.taken_by.as_deref()), (Status::Done, "2.1%", Some("researcher")));
            assert!(update(s, "x", 3, "done", "").is_err(), "a note isn't a task");
            assert!(update(s, "x", 9, "done", "").is_err());
            assert_eq!(line(s).as_deref(), Some("1 open · 1 done"));
            // The tools say the same.
            let r: Value = serde_json::from_str(&call(s, "writer", "board_read", "{}")).unwrap();
            assert_eq!(r["tasks"].as_array().unwrap().len(), 3);
            assert!(call(s, "writer", "board_post", r#"{"title":""}"#).contains("error"));
            assert!(read("../etc").tasks.is_empty() && post("../etc", "x", "y", "", None, false).is_err(), "only a conversation's own id");
        });
    }

    #[test]
    fn a_full_board_lets_go_of_finished_tasks_first() {
        crate::config::with_test_home(|_| {
            let s = "conv-2";
            let first = post(s, "lyra", "keep me", "", None, false).unwrap();
            let done = post(s, "lyra", "finished", "", None, false).unwrap();
            update(s, "lyra", done.id, "done", "ok").unwrap();
            for i in 0..MOST - 1 {
                post(s, "lyra", &format!("task {i}"), "", None, false).unwrap();
            }
            let b = read(s);
            assert_eq!(b.tasks.len(), MOST);
            assert!(b.tasks.iter().any(|t| t.id == first.id) && !b.tasks.iter().any(|t| t.id == done.id), "the finished one went first");
        });
    }
}
