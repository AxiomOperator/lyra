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
    /// Whose conversation it is (`users.json`); earlier ones are the owner's.
    #[serde(default = "owner")]
    pub owner: String,
    pub messages: Vec<SavedMessage>,
}

fn owner() -> String {
    lyra_web::users::OWNER.to_string()
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
            owner: owner(),
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

/// One user's saved sessions, newest first.
pub fn list_for(dir: &Path, owner: &str) -> Vec<Session> {
    list(dir).into_iter().filter(|s| s.owner == owner).collect()
}

/// One of the user's own sessions by id or the start of one.
pub fn find_for(dir: &Path, key: &str, owner: &str) -> Result<Session, String> {
    let s = find(dir, key)?;
    if s.owner == owner { Ok(s) } else { Err(format!("no saved session {:?} (lyra -r lists them)", key.trim())) }
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

/// A conversation that matched a search, with where.
pub struct Hit {
    pub id: String,
    pub title: String,
    pub updated: DateTime<Utc>,
    /// How many times the words appear (more is better).
    pub score: usize,
    /// The best-matching message, shortened around the first word found.
    pub role: String,
    pub snippet: String,
}

/// Full-text search over saved conversations: every word must appear in the
/// conversation (any case); best matches first, newer first on ties.
pub fn search(sessions: &[Session], query: &str, limit: usize) -> Vec<Hit> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<Hit> = sessions
        .iter()
        .filter_map(|s| {
            let texts: Vec<(&str, String)> = std::iter::once(("title", s.title.to_lowercase()))
                .chain(s.messages.iter().filter(|m| matches!(m.role.as_str(), "user" | "assistant" | "info")).map(|m| (m.role.as_str(), m.content.to_lowercase())))
                .collect();
            if !words.iter().all(|w| texts.iter().any(|(_, t)| t.contains(w.as_str()))) {
                return None;
            }
            let count = |t: &str| words.iter().map(|w| t.matches(w.as_str()).count()).sum::<usize>();
            let score = texts.iter().map(|(_, t)| count(t)).sum();
            // The message with the most of the words, user and assistant ones first.
            let best = s
                .messages
                .iter()
                .filter(|m| matches!(m.role.as_str(), "user" | "assistant" | "info"))
                .max_by_key(|m| (words.iter().filter(|w| m.content.to_lowercase().contains(w.as_str())).count(), count(&m.content.to_lowercase())));
            let (role, snippet) = best.map_or(("title".to_string(), s.title.clone()), |m| (m.role.clone(), snippet(&m.content, &words)));
            Some(Hit { id: s.id.clone(), title: s.title.clone(), updated: s.updated, score, role, snippet })
        })
        .collect();
    hits.sort_by(|a, b| b.score.cmp(&a.score).then(b.updated.cmp(&a.updated)));
    hits.truncate(limit);
    hits
}

/// About 160 characters of `text` around the first of `words` found.
fn snippet(text: &str, words: &[String]) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = flat.to_lowercase();
    let at = words.iter().filter_map(|w| lower.find(w.as_str())).min().unwrap_or(0);
    let chars: Vec<char> = flat.chars().collect();
    let at = lower[..at].chars().count();
    let start = at.saturating_sub(60).min(chars.len());
    let end = (start + 160).min(chars.len());
    let mut out: String = chars[start..end].iter().collect();
    if start > 0 {
        out = format!("…{out}");
    }
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// `/sessions search` as text.
pub fn describe_hits(hits: &[Hit], query: &str) -> String {
    if hits.is_empty() {
        return format!("no conversation mentions {query:?}");
    }
    hits.iter()
        .map(|h| {
            format!(
                "{}  {}  {}\n    {}: {}",
                h.id,
                h.updated.with_timezone(&Local).format("%m-%d %H:%M"),
                if h.title.is_empty() { "(untitled)" } else { &h.title },
                h.role,
                h.snippet
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n\n/resume <id> opens one"
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

    fn convo(id: &str, lines: &[(&str, &str)]) -> Session {
        let messages: Vec<SavedMessage> = lines
            .iter()
            .map(|(role, content)| SavedMessage {
                role: role.to_string(),
                content: content.to_string(),
                tool_calls: vec![],
                tool_call_id: None,
                reasoning: String::new(),
                memories: vec![],
                skills: vec![],
                agents: vec![],
            })
            .collect();
        Session { id: id.into(), started: Utc::now(), updated: Utc::now(), cwd: String::new(), title: lines[0].1.into(), owner: owner(), messages }
    }

    #[test]
    fn search_finds_conversations_by_what_was_said() {
        let all = vec![
            convo("a", &[("user", "nginx keeps restarting on web1"), ("assistant", "The NGINX config had a typo in the upstream block; fixed and reloaded.")]),
            convo("b", &[("user", "what's the weather"), ("assistant", "Sunny.")]),
            convo("c", &[("user", "set up nginx on web2"), ("assistant", "Installed.")]),
            convo("d", &[("user", "İstanbul notes"), ("assistant", "ok")]),
        ];
        let hits = search(&all, "nginx typo", 10);
        assert_eq!(hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), vec!["a"], "every word must appear");
        assert!(hits[0].snippet.contains("typo"), "{}", hits[0].snippet);
        let hits = search(&all, "NGINX", 10);
        assert_eq!(hits[0].id, "a", "more mentions first");
        assert_eq!(hits.len(), 2);
        assert!(search(&all, "  ", 10).is_empty());
        assert_eq!(search(&all, "notes", 10).len(), 1, "unicode text doesn't trip the snippet");
        assert!(describe_hits(&[], "zzz").contains("no conversation"));
    }

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
