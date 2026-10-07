//! How each person writes (`STYLE.md`: the owner's in `~/.lyra/context`,
//! anyone else's in `~/.lyra/users/<id>`), learned from their own sent mail
//! and corrected by them. Used whenever lyra writes as them (drafts, replies).
//! The file has two parts: "Learned" (refreshed from their mail) and "Your
//! notes" (their corrections, never overwritten).

use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde_json::{Value, json};

const LEARNED: &str = "## Learned from your sent mail";
const NOTES: &str = "## Your notes";

fn path(user: &str) -> Option<PathBuf> {
    if crate::acting::is_owner(user) { crate::config::context_dir().map(|d| d.join("STYLE.md")) } else { crate::context::user_dir(user).map(|d| d.join("STYLE.md")) }
}

/// The person's style, if lyra has one.
pub fn read(user: &str) -> Option<String> {
    std::fs::read_to_string(path(user)?).ok().filter(|t| !t.trim().is_empty())
}

/// When it was last learned (the file's own record).
fn learned_at(text: &str) -> Option<DateTime<Utc>> {
    text.lines().find_map(|l| l.strip_prefix("<!-- learned ")?.strip_suffix(" -->")).and_then(|t| DateTime::parse_from_rfc3339(t.trim()).ok()).map(|t| t.with_timezone(&Utc))
}

fn notes_of(text: &str) -> String {
    text.split_once(NOTES).map(|(_, n)| n.trim().to_string()).unwrap_or_default()
}

fn write(user: &str, learned: &str, notes: &str, at: Option<DateTime<Utc>>) -> Result<(), String> {
    let p = path(user).ok_or("no lyra home")?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let stamp = at.map(|t| format!("<!-- learned {} -->\n", t.to_rfc3339())).unwrap_or_default();
    let text = format!("# How I write\n\n{stamp}{LEARNED}\n\n{}\n\n{NOTES}\n\n{}\n", learned.trim(), notes.trim());
    std::fs::write(p, text).map_err(|e| e.to_string())
}

/// For a prompt: how to write as them.
pub fn section(user: &str) -> Option<String> {
    let t = read(user)?;
    let body = t.lines().filter(|l| !l.starts_with("<!--") && !l.starts_with("# How I write")).collect::<Vec<_>>().join("\n");
    Some(format!("When you write as the user (an email, a reply, a message others read), write the way they do. Their own notes override what was learned.\n{}", body.trim()))
}

const PROMPT: &str = "You study someone's sent emails to describe how they write, so an assistant can write as them. \
Describe, in short Markdown bullets (at most 15): greetings and sign-offs they use (exact words), typical length, \
tone and formality (with whom, if it differs), sentence style, words and phrases they often use, things they never do, \
punctuation and formatting habits (lists, bold, emoji), and how they ask for things or say no. \
Then give 2 short example sentences in their voice. Describe only what the emails show; no names of other people, no private details.";

/// Learn (again) from the person's sent mail; their notes stay. Returns a line saying what happened.
pub fn learn(url: &str, model: &str) -> Result<String, String> {
    let user = crate::acting::current();
    let samples = crate::mail::sent_texts(40)?;
    let samples: Vec<String> = samples.into_iter().filter(|t| t.chars().count() >= 40).take(25).collect();
    if samples.len() < 3 {
        return Err("not enough sent mail to learn from yet".into());
    }
    let corpus: String = samples.iter().enumerate().map(|(i, t)| format!("--- email {} ---\n{}", i + 1, t.chars().take(1200).collect::<String>())).collect::<Vec<_>>().join("\n\n");
    let (reply, _) = crate::learn::complete(url, model, PROMPT, &corpus.chars().take(24_000).collect::<String>())?;
    let learned = reply.rsplit_once("</think>").map_or(reply.as_str(), |(_, a)| a).trim().to_string();
    if learned.is_empty() {
        return Err("the model gave no description".into());
    }
    let notes = read(&user).map(|t| notes_of(&t)).unwrap_or_default();
    write(&user, &learned, &notes, Some(Utc::now()))?;
    Ok(format!("learned your writing style from {} sent emails", samples.len()))
}

/// Due for (re)learning: none yet, or older than a week.
pub fn stale(user: &str) -> bool {
    read(user).and_then(|t| learned_at(&t)).is_none_or(|t| Utc::now() - t > Duration::days(7))
}

/// A correction from the person ("don't sign off with Thanks").
pub fn note(text: &str) -> Result<String, String> {
    let user = crate::acting::current();
    let t = text.trim();
    if t.is_empty() {
        return Err("what should change?".into());
    }
    let current = read(&user).unwrap_or_default();
    let learned = current.split_once(LEARNED).map(|(_, rest)| rest.split(NOTES).next().unwrap_or("").trim().to_string()).unwrap_or_default();
    let mut notes = notes_of(&current);
    notes.push_str(&format!("\n- {t}"));
    write(&user, &learned, &notes, learned_at(&current))?;
    Ok(format!("noted for how you write: {t}"))
}

pub fn capabilities() -> Vec<Capability> {
    let mut c = Capability::new(
        "style_note",
        CapabilityKind::NativeTool,
        "Keep a correction to how the user writes (\"don't sign off with Thanks\", \"keep replies to three lines\", \"I say Hi, not Hello\"), used whenever lyra writes as them.",
        RiskLevel::LowWrite,
    );
    c.input_schema = json!({ "type": "object", "properties": { "note": { "type": "string" } }, "required": ["note"] });
    c.source = "style".into();
    c.tags = ["style", "tone", "writing", "voice", "sign-off", "email"].iter().map(|t| t.to_string()).collect();
    vec![c]
}

pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "style_note" => Ok(json!({ "noted": note(args["note"].as_str().unwrap_or(""))? })),
        other => Err(format!("{other} isn't a style tool")),
    }
}

/// `/style [learn|note <text>|clear notes]`.
pub fn command(arg: &str, url: &str, model: &str) -> Result<String, String> {
    let user = crate::acting::current();
    let a = arg.trim();
    match a.split_once(' ').map_or((a, ""), |(x, y)| (x, y.trim())) {
        ("", _) => Ok(read(&user).unwrap_or_else(|| "lyra hasn't learned how you write yet: /style learn (it reads your sent mail)".into())),
        ("learn", _) => learn(url, model),
        ("note", t) => note(t),
        ("clear", "notes") => {
            let current = read(&user).unwrap_or_default();
            let learned = current.split_once(LEARNED).map(|(_, r)| r.split(NOTES).next().unwrap_or("").trim().to_string()).unwrap_or_default();
            write(&user, &learned, "", learned_at(&current))?;
            Ok("your style notes are cleared".into())
        }
        _ => Err("usage: /style [learn | note <how you write> | clear notes]".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_survive_relearning_and_dates_read_back() {
        let at = Utc::now();
        let text = format!("# How I write\n\n<!-- learned {} -->\n{LEARNED}\n\n- Hi first name\n\n{NOTES}\n\n- never \"Best regards\"\n", at.to_rfc3339());
        assert_eq!(notes_of(&text), "- never \"Best regards\"");
        assert_eq!(learned_at(&text).map(|t| t.timestamp()), Some(at.timestamp()));
        assert!(learned_at("no stamp").is_none());
    }
}
