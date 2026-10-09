//! Saved prompts: one-tap starters ("Weekly status for my manager"). Each
//! person keeps their own (`templates.json` in their files); admins publish
//! shared ones everyone sees (`templates/shared.json`). The app fills the
//! message box with one, to change before sending; nothing runs by itself.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::store::JsonStore;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Template {
    pub id: String,
    pub title: String,
    /// What goes in the message box.
    pub prompt: String,
    #[serde(default)]
    pub updated: String,
}

/// The shared ones a fresh lyra starts with (until an admin changes them).
fn starters() -> Vec<Template> {
    let t = |id: &str, title: &str, prompt: &str| Template { id: id.into(), title: title.into(), prompt: prompt.into(), updated: String::new() };
    vec![
        t("weekly-status", "Weekly status for my manager", "Write my weekly status for my manager: what I finished this week, what's in progress, what's blocked, and what's next. Use my PMI tasks and calendar, keep it short, in my writing style."),
        t("summarize-thread", "Summarize this thread", "Summarize this email thread: who said what, what was decided, and what's still open (with who owns it). Thread: "),
        t("reply-in-my-style", "Draft a reply in my style", "Draft a reply in my style to the latest email from "),
        t("meeting-prep", "Prepare me for my next meeting", "Prepare me for my next meeting: who's coming, our last emails, open tasks with them, and what I should bring up."),
    ]
}

fn mine_path(user: &str) -> Option<PathBuf> {
    if user == lyra_web::users::OWNER { Some(crate::config::home()?.join("templates").join("mine.json")) } else { Some(crate::context::user_dir(user)?.join("templates.json")) }
}

fn shared_path() -> Option<PathBuf> {
    Some(crate::config::home()?.join("templates").join("shared.json"))
}

/// Shared ones: the starters until anyone has saved the list.
fn shared() -> Vec<Template> {
    match shared_path() {
        Some(p) if p.exists() => JsonStore::<Vec<Template>>::new(p).load(),
        _ => starters(),
    }
}

fn mine(user: &str) -> Vec<Template> {
    mine_path(user).map(|p| JsonStore::<Vec<Template>>::new(p).load()).unwrap_or_default()
}

/// What the app shows: theirs, then the shared ones.
pub fn page(user: &str, admin: bool) -> Value {
    json!({ "mine": mine(user), "shared": shared(), "can_share": admin })
}

fn slug(title: &str) -> String {
    let s: String = title.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    format!("{}-{}", s.chars().take(40).collect::<String>(), &lyra_learning::Uuid::new_v4().simple().to_string()[..4])
}

/// Add or change one (`id` empty: a new one). `shared`: everyone's (admins only).
pub fn put(user: &str, admin: bool, shared: bool, id: &str, title: &str, prompt: &str) -> Result<Template, String> {
    let (title, prompt) = (title.trim(), prompt.trim_end());
    if title.is_empty() || prompt.trim().is_empty() {
        return Err("a saved prompt needs a name and its text".into());
    }
    if title.chars().count() > 80 || prompt.chars().count() > 4000 {
        return Err("that's too long (80 characters for the name, 4,000 for the text)".into());
    }
    if shared && !admin {
        return Err("only an admin can share a prompt with everyone".into());
    }
    let path = if shared { shared_path() } else { mine_path(user) }.ok_or("no lyra home")?;
    let store = JsonStore::<Vec<Template>>::new(path.clone());
    let fresh = shared && !path.exists();
    store.update(|all| {
        // The first change to the shared list starts from the starters.
        if fresh {
            *all = starters();
        }
        let t = Template { id: if id.is_empty() { slug(title) } else { id.to_string() }, title: title.into(), prompt: prompt.into(), updated: chrono::Utc::now().to_rfc3339() };
        match all.iter_mut().find(|x| x.id == t.id) {
            Some(x) => *x = t.clone(),
            None => all.push(t.clone()),
        }
        t
    })
}

/// Remove one of theirs (or, an admin, a shared one).
pub fn remove(user: &str, admin: bool, shared: bool, id: &str) -> Result<(), String> {
    if shared && !admin {
        return Err("only an admin can remove a shared prompt".into());
    }
    let path = if shared { shared_path() } else { mine_path(user) }.ok_or("no lyra home")?;
    let fresh = shared && !path.exists();
    JsonStore::<Vec<Template>>::new(path).try_update(|all| {
        if fresh {
            *all = starters();
        }
        let before = all.len();
        all.retain(|t| t.id != id);
        if all.len() == before { Err(format!("no saved prompt {id:?}")) } else { Ok(()) }
    })
}

/// `/templates`: theirs and the shared ones, by name.
pub fn command(user: &str) -> String {
    let line = |t: &Template| format!("  {} — {}", t.title, t.prompt.chars().take(70).collect::<String>());
    let mut out = vec!["Your saved prompts:".to_string()];
    let m = mine(user);
    if m.is_empty() {
        out.push("  (none yet: save one in the app, the bookmark button by the message box)".into());
    }
    out.extend(m.iter().map(line));
    out.push("Shared:".into());
    out.extend(shared().iter().map(line));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starters_have_ids_and_text() {
        let s = starters();
        assert!(s.len() >= 3 && s.iter().all(|t| !t.id.is_empty() && !t.prompt.is_empty()));
        assert!(slug("Weekly status: for my manager!").starts_with("weekly-status-for-my-manager-"));
    }
}
