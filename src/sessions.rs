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
    /// Just talk: no tools in this conversation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub chat_only: bool,
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
            chat_only: false,
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
    let text = serde_json::to_string(s).map_err(|e| e.to_string())?;
    crate::store::write_text(&dir.join(format!("{}.json", s.id)), &text)
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
        .filter(|p| p.extension().is_some_and(|e| e == "json") && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.') || n == "meta.json"))
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
    let key = key.trim();
    let mine = |s: &Session| s.owner == owner && s.user_turns() > 0;
    // A whole id: its own file, read on its own (not every saved session).
    if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && let Ok(s) = load_file(&dir.join(format!("{key}.json")))
        && mine(&s)
    {
        return Ok(s);
    }
    // Part of one: only the files whose names fit are read.
    let matches: Vec<Session> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let id = name.strip_suffix(".json")?;
            (!key.is_empty() && !id.starts_with('.') && id != "meta" && (id.starts_with(key) || id.ends_with(key))).then(|| e.path())
        })
        .filter_map(|p| load_file(&p).ok())
        .filter(|s| mine(s) && (s.id.starts_with(key) || s.id.ends_with(key)))
        .collect();
    match matches.len() {
        0 => Err(format!("no saved session {key:?} (lyra -r lists them)")),
        1 => Ok(matches.into_iter().next().expect("one")),
        n => Err(format!("{n} sessions match {key:?}; use more of the id")),
    }
}

/// The session to continue: the latest started in this folder, else the latest.
pub fn latest(dir: &Path) -> Option<Session> {
    let here = cwd();
    let mut all = list_for(dir, lyra_web::users::OWNER);
    let i = all.iter().position(|s| s.cwd == here).unwrap_or(0);
    (!all.is_empty()).then(|| all.swap_remove(i))
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
        Session { id: id.into(), started: Utc::now(), updated: Utc::now(), cwd: String::new(), title: lines[0].1.into(), owner: owner(), chat_only: false, messages }
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
        let back = find_for(&dir, "20260101", "owner").unwrap();
        assert_eq!(back.title, "first question");
        let messages = back.into_messages();
        assert_eq!((messages.len(), messages[1].agents.clone(), messages[2].tool_calls.len()), (3, vec!["Writer".to_string()], 1));
        assert!(find_for(&dir, "2026010", "owner").err().unwrap().contains("match"));
        assert!(find_for(&dir, "nope", "owner").is_err());
        assert!(describe(&list(&dir), 10).contains("second chat"));
    }
}

/// How a person keeps a conversation in their list: pinned, archived, in a
/// folder, and the folder lyra suggests. Kept apart from the conversation
/// (`sessions/meta.json`), so saving one never loses it.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// lyra's suggestion, until it's taken or dismissed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested: Option<String>,
    /// lyra looked for a folder already (asked once per conversation).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub looked: bool,
}

static META: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn meta_path(dir: &Path) -> PathBuf {
    dir.join("meta.json")
}

/// Every conversation's keeping, by id.
pub fn metas(dir: &Path) -> std::collections::HashMap<String, Meta> {
    crate::store::read_json(&meta_path(dir))
}

/// Change one conversation's keeping (only one of `owner`'s own).
pub fn set_meta(dir: &Path, id: &str, owner: &str, change: impl FnOnce(&mut Meta)) -> Result<Meta, String> {
    let s = find_for(dir, id, owner)?;
    let _guard = META.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = metas(dir);
    let m = all.entry(s.id.clone()).or_default();
    change(m);
    let out = m.clone();
    if *m == Meta::default() {
        all.remove(&s.id);
    }
    crate::store::write_json(&meta_path(dir), &all)?;
    Ok(out)
}

/// A person's folders, in the order they're first used (A–Z).
pub fn folders(dir: &Path, owner: &str) -> Vec<String> {
    let mine: std::collections::HashSet<String> = list_for(dir, owner).into_iter().map(|s| s.id).collect();
    let mut names: Vec<String> = metas(dir).into_iter().filter(|(id, _)| mine.contains(id)).filter_map(|(_, m)| m.folder).collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
}

/// `/sessions pin|unpin|archive|unarchive|folder|dismiss <id> [folder]`.
pub fn keep_command(dir: &Path, owner: &str, arg: &str) -> Result<String, String> {
    let mut words = arg.split_whitespace();
    let (Some(what), Some(id)) = (words.next(), words.next()) else {
        return Err("usage: /sessions pin|unpin|archive|unarchive <id> · /sessions folder <id> <name> (- takes it out) · /sessions dismiss <id>".into());
    };
    let rest: String = words.collect::<Vec<_>>().join(" ");
    let name = rest.trim().trim_matches('"').trim();
    let (m, done) = match what {
        "pin" => (set_meta(dir, id, owner, |m| m.pinned = true)?, "pinned".to_string()),
        "unpin" => (set_meta(dir, id, owner, |m| m.pinned = false)?, "unpinned".to_string()),
        "archive" => (set_meta(dir, id, owner, |m| {
            m.archived = true;
            m.pinned = false;
        })?, "archived".to_string()),
        "unarchive" => (set_meta(dir, id, owner, |m| m.archived = false)?, "back in the list".to_string()),
        "dismiss" => (set_meta(dir, id, owner, |m| m.suggested = None)?, "suggestion dismissed".to_string()),
        "folder" if name.is_empty() => return Err("which folder? (/sessions folder <id> <name>, or - to take it out)".into()),
        "folder" if name == "-" => (set_meta(dir, id, owner, |m| m.folder = None)?, "out of its folder".to_string()),
        "folder" => {
            if name.chars().count() > 40 {
                return Err("a folder name up to 40 characters".into());
            }
            // The same folder however it's typed.
            let name = folders(dir, owner).into_iter().find(|f| f.eq_ignore_ascii_case(name)).unwrap_or_else(|| name.to_string());
            (set_meta(dir, id, owner, |m| {
                m.folder = Some(name.clone());
                m.suggested = None;
            })?, format!("moved to {name}"))
        }
        other => return Err(format!("/sessions {other}? pin, unpin, archive, unarchive, folder or dismiss")),
    };
    let _ = m;
    Ok(format!("{id}: {done}"))
}

/// Which of the person's folders a conversation belongs in, if any: the
/// decision model's pick from its title and first messages (asked once).
pub fn suggest_folder(s: &Session, folders: &[String]) -> Option<String> {
    if folders.is_empty() {
        return None;
    }
    let text: String = s.messages.iter().filter(|m| m.role == "user").take(3).map(|m| m.content.chars().take(400).collect::<String>()).collect::<Vec<_>>().join("\n");
    let mut options: Vec<(String, String)> = folders.iter().map(|f| (f.clone(), format!("Conversations about {f}."))).collect();
    options.push(("none".into(), "None of these: a different subject.".into()));
    let q = crate::decide::Question::Choice("Which folder does this conversation belong in?".into(), options);
    let answers = crate::decide::ask("folder", &format!("{}\n{text}", s.title), &[("folder".into(), q)])?;
    let a = crate::decide::confident(&answers, "folder")?;
    folders.iter().find(|f| **f == a.choice).cloned()
}

#[cfg(test)]
mod keep_tests {
    use super::*;

    #[test]
    fn pins_folders_and_archive_are_kept_apart_and_only_for_the_owner() {
        let dir = std::env::temp_dir().join(format!("lyra-sessions-meta-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = Session { id: "20261008-090000-abc123".into(), started: Utc::now(), updated: Utc::now(), cwd: String::new(), title: "Firewall rules".into(), owner: "dana".into(), chat_only: false, messages: vec![] };
        s.messages.push(SavedMessage { role: "user".into(), content: "Firewall rules".into(), tool_calls: vec![], tool_call_id: None, reasoning: String::new(), memories: vec![], skills: vec![], agents: vec![] });
        save(&dir, &s).unwrap();
        assert!(keep_command(&dir, "dana", "folder abc123 Network").unwrap().contains("moved to Network"));
        keep_command(&dir, "dana", "pin abc123").unwrap();
        assert!(keep_command(&dir, "owner", "pin abc123").is_err(), "not someone else's");
        // Saving the conversation again keeps them.
        save(&dir, &s).unwrap();
        let m = &metas(&dir)["20261008-090000-abc123"];
        assert!(m.pinned && m.folder.as_deref() == Some("Network"));
        assert_eq!(folders(&dir, "dana"), vec!["Network"]);
        assert!(folders(&dir, "owner").is_empty());
        // The same folder however it's typed; archiving unpins.
        keep_command(&dir, "dana", "folder abc123 network").unwrap();
        assert_eq!(metas(&dir)["20261008-090000-abc123"].folder.as_deref(), Some("Network"));
        keep_command(&dir, "dana", "archive abc123").unwrap();
        let m = &metas(&dir)["20261008-090000-abc123"];
        assert!(m.archived && !m.pinned);
        keep_command(&dir, "dana", "folder abc123 -").unwrap();
        keep_command(&dir, "dana", "unarchive abc123").unwrap();
        assert!(!metas(&dir).contains_key("20261008-090000-abc123"), "nothing left to keep");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
