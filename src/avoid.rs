//! What not to do (4.2): a correction becomes a short "avoid" note on the
//! tool or skill it was about ("mail_send: don't send to a whole list when
//! one person was named"), and the note goes into the prompt whenever that
//! tool or skill is likely: the skill is in it, or the tool was used just
//! before or is named by the request. `skills/avoid.json`; the owner's notes
//! are everyone's (like shared skills), anyone else's are theirs alone.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store::JsonStore;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Avoid {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    /// The note itself: "Don't …".
    pub note: String,
    /// Whose (`None`: the owner's, everyone's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Corrections that said the same.
    pub count: u32,
    pub added: DateTime<Utc>,
    pub last: DateTime<Utc>,
}

/// Notes kept at most (the least confirmed go first).
const KEEP: usize = 200;
/// Notes in one prompt at most.
const IN_PROMPT: usize = 4;

fn store() -> Option<JsonStore<Vec<Avoid>>> {
    Some(JsonStore::new(crate::config::home()?.join("skills").join("avoid.json")))
}

pub fn all() -> Vec<Avoid> {
    store().map(|s| s.load()).unwrap_or_default()
}

/// Keep a note (one saying the same about the same thing counts again).
/// Returns it and whether it's new.
pub fn add(tool: Option<&str>, skill: Option<&str>, note: &str, owner: Option<&str>) -> Result<(Avoid, bool), String> {
    let s = store().ok_or("no home directory")?;
    let note = note.trim().chars().take(240).collect::<String>();
    if note.len() < 8 || (tool.is_none() && skill.is_none()) {
        return Err("an avoid note needs its tool or skill, and some words".into());
    }
    s.update(|all| {
        let now = Utc::now();
        if let Some(a) = all.iter_mut().find(|a| a.tool.as_deref() == tool && a.skill.as_deref() == skill && a.owner.as_deref() == owner && crate::outcomes::similar(&a.note, &note)) {
            a.count += 1;
            a.last = now;
            return (a.clone(), false);
        }
        let a = Avoid { id: lyra_learning::Uuid::new_v4().simple().to_string()[..8].to_string(), tool: tool.map(str::to_string), skill: skill.map(str::to_string), note, owner: owner.map(str::to_string), count: 1, added: now, last: now };
        all.push(a.clone());
        if all.len() > KEEP {
            all.sort_by_key(|a| std::cmp::Reverse((a.count, a.last)));
            all.truncate(KEEP);
        }
        (a, true)
    })
}

pub fn remove(id: &str) -> Result<bool, String> {
    let s = store().ok_or("no home directory")?;
    s.update(|all| {
        let before = all.len();
        all.retain(|a| !(a.id.starts_with(id.trim()) && !id.trim().is_empty()));
        before != all.len()
    })
}

/// The notes for a turn: `viewer` (`None`: the owner) sees the owner's and
/// their own; a note counts when its skill is in the prompt, or its tool was
/// used just before or is named by the request ("mail" in "mail_send").
pub fn relevant(viewer: Option<&str>, request: &str, skills: &[String], recent_tools: &[String]) -> Vec<Avoid> {
    let words: Vec<String> = request.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 2).map(str::to_string).collect();
    let named = |tool: &str| tool.split(['_', '.', '-']).filter(|p| p.len() > 2).any(|p| words.iter().any(|w| w == p || w.trim_end_matches('s') == p));
    let mut found: Vec<Avoid> = all()
        .into_iter()
        .filter(|a| a.owner.is_none() || a.owner.as_deref() == viewer)
        .filter(|a| a.skill.as_ref().is_some_and(|s| skills.iter().any(|x| x.trim_end_matches(" (trial)") == s)) || a.tool.as_ref().is_some_and(|t| recent_tools.contains(t) || named(t)))
        .collect();
    found.sort_by_key(|a| std::cmp::Reverse((a.count, a.last)));
    found.truncate(IN_PROMPT);
    found
}

/// The prompt's section for these notes.
pub fn section(notes: &[Avoid]) -> Option<String> {
    if notes.is_empty() {
        return None;
    }
    let lines: Vec<String> = notes.iter().map(|a| format!("- {}: {}", a.tool.as_deref().or(a.skill.as_deref()).unwrap_or(""), a.note)).collect();
    Some(format!("# Avoid (learned from corrections)\n\nYou were corrected for these before; don't repeat them:\n{}", lines.join("\n")))
}

const PROMPT: &str = "\
A person corrected an AI assistant's reply. If the correction is about how a specific tool or learned skill was used, \
write one short note on what to avoid next time (\"Don't …\"), for that tool or skill. \
Use only a tool or skill name from the lists given. If it isn't about one of them, say so with nulls. \
Reply with JSON only: {\"tool\": \"name\" | null, \"skill\": \"name\" | null, \"avoid\": \"Don't …\" | null}.";

/// Read a correction for a note (the light model): `None` when it isn't
/// about one of the tools or skills it had.
pub fn learn(url: &str, model: &str, asked: &str, reply: &str, correction: &str, tools: &[String], skills: &[String]) -> Option<(Option<String>, Option<String>, String)> {
    if tools.is_empty() && skills.is_empty() {
        return None;
    }
    let clip = |t: &str, n: usize| t.chars().take(n).collect::<String>();
    let state = format!(
        "Request: {}\n\nReply: {}\n\nTools it used: {}\nSkills in its instructions: {}\n\nThe person's correction: {}",
        clip(asked, 600),
        clip(reply, 1500),
        if tools.is_empty() { "none".into() } else { tools.join(", ") },
        if skills.is_empty() { "none".into() } else { skills.join(", ") },
        clip(correction, 600)
    );
    let (out, _) = crate::learn::complete_light_short(url, model, PROMPT, &state, 200).ok()?;
    let v: Value = out.find('{').zip(out.rfind('}')).and_then(|(a, b)| serde_json::from_str(out.get(a..=b)?).ok())?;
    let note = v["avoid"].as_str()?.trim().to_string();
    let tool = v["tool"].as_str().filter(|t| tools.iter().any(|x| x == t)).map(str::to_string);
    let skill = v["skill"].as_str().filter(|s| skills.iter().any(|x| x == s)).map(str::to_string);
    (tool.is_some() || skill.is_some()).then_some((tool, skill, note))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_goes_in_when_its_tool_or_skill_is_likely_and_only_for_whose_it_is() {
        crate::config::with_test_home(|_| {
            let (a, new) = add(Some("mail_send"), None, "Don't send to the whole team when one person was named", None).unwrap();
            assert!(new);
            let (again, new) = add(Some("mail_send"), None, "Don't send to the whole team when only one person was named", None).unwrap();
            assert!(!new && again.count == 2 && again.id == a.id, "the same note counts again");
            add(None, Some("restart-printer"), "Don't power-cycle the printer while it's printing", None).unwrap();
            add(Some("calendar_create"), None, "Don't book over lunch", Some("dana")).unwrap();
            assert!(add(None, None, "Don't do that", None).is_err(), "it needs its tool or skill");
            // Named by the request, used just before, or the skill is in the prompt.
            assert_eq!(relevant(None, "send a mail to Sam", &[], &[]).len(), 1);
            assert_eq!(relevant(None, "what's next?", &[], &["mail_send".into()]).len(), 1);
            assert_eq!(relevant(None, "fix it", &["restart-printer".into()], &[]).len(), 1);
            assert!(relevant(None, "what's the weather", &[], &[]).is_empty());
            // Dana's own note is hers; the owner's are everyone's.
            assert!(relevant(None, "book a calendar slot", &[], &[]).is_empty());
            assert_eq!(relevant(Some("dana"), "book a calendar slot and send a mail", &[], &[]).len(), 2);
            let s = section(&relevant(None, "send mail", &[], &[])).unwrap();
            assert!(s.contains("- mail_send: Don't send to the whole team"));
            assert!(remove(&a.id).unwrap());
            assert!(relevant(None, "send a mail", &[], &[]).is_empty());
        });
    }
}
