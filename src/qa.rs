//! Q&A: questions people asked and their answers, for everyone to read.
//! An admin promotes a question sent through Feedback (lyra drafts the
//! answer; the admin edits and approves it), or writes one directly, and can
//! change or remove entries. lyra looks here too when it answers in chat.
//! Kept in `~/.lyra/feedback/qa.json`.

use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub id: u64,
    pub question: String,
    pub answer: String,
    /// The Feedback question it came from.
    #[serde(default)]
    pub source: Option<u64>,
    /// Who asked it (their name), when it came from someone.
    #[serde(default)]
    pub asked_by: Option<String>,
    /// The admin who approved it.
    pub approved_by: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
}

static LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
thread_local! {
    static TEST_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

fn path() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(d) = TEST_DIR.with(|d| d.borrow().clone()) {
        return Some(d.join("qa.json"));
    }
    Some(crate::config::home()?.join("feedback").join("qa.json"))
}

pub fn all() -> Vec<Entry> {
    let mut v: Vec<Entry> = path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    v.sort_by_key(|e| std::cmp::Reverse(e.updated));
    v
}

fn save(all: &[Entry]) -> Result<(), String> {
    let p = path().ok_or("no home directory")?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(all).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

/// Add or change an entry (admins). `id` 0 adds one.
pub fn put(who: &crate::feedback::Who, id: u64, question: &str, answer: &str, source: Option<u64>, asked_by: Option<String>) -> Result<Entry, String> {
    if !who.admin {
        return Err("only an admin changes Q&A".into());
    }
    let (q, a) = (question.trim(), answer.trim());
    if q.len() < 3 || a.len() < 3 {
        return Err("a question and an answer, please".into());
    }
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = all();
    let now = Utc::now();
    let entry = if id == 0 {
        let e = Entry { id: list.iter().map(|e| e.id).max().unwrap_or(0) + 1, question: q.into(), answer: a.into(), source, asked_by, approved_by: who.name.clone(), created: now, updated: now };
        list.push(e.clone());
        e
    } else {
        let e = list.iter_mut().find(|e| e.id == id).ok_or_else(|| format!("no Q&A #{id}"))?;
        e.question = q.into();
        e.answer = a.into();
        e.approved_by = who.name.clone();
        e.updated = now;
        e.clone()
    };
    save(&list)?;
    Ok(entry)
}

pub fn remove(who: &crate::feedback::Who, id: u64) -> Result<(), String> {
    if !who.admin {
        return Err("only an admin changes Q&A".into());
    }
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = all();
    let before = list.len();
    list.retain(|e| e.id != id);
    if list.len() == before {
        return Err(format!("no Q&A #{id}"));
    }
    save(&list)
}

/// Entries about `query` (shared words), best first.
pub fn search(query: &str, n: usize) -> Vec<Entry> {
    let words: Vec<String> = query.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 2).map(str::to_string).collect();
    let mut scored: Vec<(usize, Entry)> = all()
        .into_iter()
        .map(|e| {
            let text = format!("{} {}", e.question, e.answer).to_lowercase();
            (words.iter().filter(|w| text.contains(w.as_str())).count(), e)
        })
        .filter(|(n, _)| *n > 0)
        .collect();
    scored.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    scored.into_iter().take(n).map(|(_, e)| e).collect()
}

pub fn capabilities() -> Vec<Capability> {
    let mut c = Capability::new("qa_search", CapabilityKind::NativeTool, "Look in lyra's Q&A: questions people asked about lyra and the answers the admins approved. Use it for \"how do I …\" questions about lyra itself.", RiskLevel::ReadOnly);
    c.input_schema = json!({ "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] });
    c.source = "qa".into();
    c.tags = ["how", "question", "faq", "help", "answer", "lyra", "use"].iter().map(|t| t.to_string()).collect();
    vec![c]
}

pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "qa_search" => Ok(json!({ "found": search(args["query"].as_str().unwrap_or(""), 5).iter().map(|e| json!({ "question": e.question, "answer": e.answer })).collect::<Vec<_>>() })),
        other => Err(format!("{other} isn't a Q&A tool")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admins_keep_q_and_a_and_everyone_finds_it() {
        let dir = std::env::temp_dir().join(format!("lyra-qa-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        TEST_DIR.with(|d| *d.borrow_mut() = Some(dir.clone()));
        let dana = crate::feedback::Who { user: "dana".into(), name: "Dana".into(), admin: false };
        let admin = crate::feedback::Who { user: "owner".into(), name: "Garrett".into(), admin: true };
        assert!(put(&dana, 0, "How do I connect Outlook?", "More → Outlook → Connect.", None, None).is_err(), "members read it, admins write it");
        let e = put(&admin, 0, "How do I connect Outlook?", "More → Outlook → Connect.", Some(4), Some("Dana".into())).unwrap();
        assert_eq!((e.id, e.source, e.approved_by.as_str()), (1, Some(4), "Garrett"));
        put(&admin, 1, "How do I connect my Outlook calendar?", "In the app: More → Outlook → Connect.", None, None).unwrap();
        assert_eq!(search("connect outlook calendar", 3)[0].answer, "In the app: More → Outlook → Connect.");
        assert!(search("firewall", 3).is_empty());
        assert!(remove(&dana, 1).is_err());
        remove(&admin, 1).unwrap();
        assert!(all().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
