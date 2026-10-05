//! Saved conversations (`~/.lyra/sessions/<id>.json`), so `lyra -c` picks up
//! where the last one left off and `lyra -r <id>` resumes any of them.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

use crate::{Message, ToolCall};

/// One chat line as saved: what the model saw plus what the UI showed.
#[derive(Serialize, Deserialize)]
pub struct SavedMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memories: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub started: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    /// Where lyra was started (`-c` prefers this folder's latest).
    #[serde(default)]
    pub cwd: String,
    /// The first thing the user said, for lists.
    #[serde(default)]
    pub title: String,
    pub messages: Vec<SavedMessage>,
}

pub fn dir() -> Option<PathBuf> {
    Some(crate::config::home()?.join("sessions"))
}

pub fn new_id() -> String {
    format!("{}-{}", Local::now().format("%Y%m%d-%H%M%S"), uuid_short())
}

fn uuid_short() -> String {
    lyra_learning::Uuid::new_v4().simple().to_string()[..6].to_string()
}

fn cwd() -> String {
    std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default()
}

impl Session {
    pub fn from_messages(id: &str, started: DateTime<Utc>, messages: &[Message]) -> Self {
        let title = messages.iter().find(|m| m.role == "user").map(|m| m.content.lines().next().unwrap_or("").chars().take(80).collect()).unwrap_or_default();
        Self {
            id: id.to_string(),
            started,
            updated: Utc::now(),
            cwd: cwd(),
            title,
            messages: messages
                .iter()
                .map(|m| SavedMessage {
                    role: m.role.clone(),
                    content: m.content.clone(),
                    tool_calls: m.tool_calls.clone(),
                    tool_call_id: m.tool_call_id.clone(),
                    reasoning: m.reasoning.clone(),
                    memories: m.memories.clone(),
                    skills: m.skills.clone(),
                    agents: m.agents.clone(),
                })
                .collect(),
        }
    }

    pub fn into_messages(self) -> Vec<Message> {
        self.messages
            .into_iter()
            .map(|s| {
                let mut m = Message::new(&s.role, s.content);
                m.tool_calls = s.tool_calls;
                m.tool_call_id = s.tool_call_id;
                m.reasoning = s.reasoning;
                m.memories = s.memories;
                m.skills = s.skills;
                m.agents = s.agents;
                m
            })
            .collect()
    }

    pub fn user_turns(&self) -> usize {
        self.messages.iter().filter(|m| m.role == "user").count()
    }
}

/// Write a session (atomically: a half-written file never replaces a good one).
pub fn save(dir: &Path, s: &Session) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.json", s.id));
    let tmp = dir.join(format!(".{}.json.tmp", s.id));
    let text = serde_json::to_string(s).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

fn load_file(path: &Path) -> Result<Session, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Every saved session, newest first.
pub fn list(dir: &Path) -> Vec<Session> {
    let mut all: Vec<Session> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json") && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .filter_map(|p| load_file(&p).ok())
        .filter(|s| s.user_turns() > 0)
        .collect();
    all.sort_by_key(|s| std::cmp::Reverse(s.updated));
    all
}

/// The session to continue: the latest started in this folder, else the latest.
pub fn latest(dir: &Path) -> Option<Session> {
    let here = cwd();
    let mut all = list(dir);
    let i = all.iter().position(|s| s.cwd == here).unwrap_or(0);
    (!all.is_empty()).then(|| all.swap_remove(i))
}

/// A session by id or the start of one.
pub fn find(dir: &Path, key: &str) -> Result<Session, String> {
    let key = key.trim();
    let matches: Vec<Session> = list(dir).into_iter().filter(|s| s.id.starts_with(key) || s.id.ends_with(key)).collect();
    match matches.len() {
        0 => Err(format!("no saved session {key:?} (lyra -r lists them)")),
        1 => Ok(matches.into_iter().next().expect("one")),
        n => Err(format!("{n} sessions match {key:?}; use more of the id")),
    }
}

/// A list for `lyra -r` and `/sessions`.
pub fn describe(sessions: &[Session], limit: usize) -> String {
    if sessions.is_empty() {
        return "no saved sessions yet".into();
    }
    let mut out: Vec<String> = sessions
        .iter()
        .take(limit)
        .map(|s| {
            format!(
                "{}  {}  {:>3} turns  {}",
                s.id,
                s.updated.with_timezone(&Local).format("%m-%d %H:%M"),
                s.user_turns(),
                if s.title.is_empty() { "(untitled)" } else { &s.title }
            )
        })
        .collect();
    if sessions.len() > limit {
        out.push(format!("… {} older", sessions.len() - limit));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_round_trip_and_the_latest_wins() {
        let dir = std::env::temp_dir().join(format!("lyra-sessions-{}", uuid_short()));
        let mut a = vec![Message::new("user", "first question".into()), Message::new("assistant", "an answer".into())];
        a[1].agents = vec!["Writer".into()];
        let mut call = Message::new("assistant", String::new());
        call.tool_calls = vec![ToolCall::default()];
        a.push(call);
        save(&dir, &Session::from_messages("20260101-000000-aaaaaa", Utc::now(), &a)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = vec![Message::new("user", "second chat".into())];
        save(&dir, &Session::from_messages("20260102-000000-bbbbbb", Utc::now(), &b)).unwrap();
        save(&dir, &Session::from_messages("20260103-000000-cccccc", Utc::now(), &[Message::new("info", "no user turn".into())])).unwrap();

        assert_eq!(list(&dir).len(), 2, "sessions without a user turn aren't listed");
        assert_eq!(latest(&dir).unwrap().id, "20260102-000000-bbbbbb");
        let back = find(&dir, "20260101").unwrap();
        assert_eq!(back.title, "first question");
        let messages = back.into_messages();
        assert_eq!((messages.len(), messages[1].agents.clone(), messages[2].tool_calls.len()), (3, vec!["Writer".to_string()], 1));
        assert!(find(&dir, "2026010").err().unwrap().contains("match"));
        assert!(find(&dir, "nope").is_err());
        assert!(describe(&list(&dir), 10).contains("second chat"));
    }
}
