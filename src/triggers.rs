//! Automations, "when X, do Y": a routine whose schedule is an event
//! instead of a time ("when mail from Accounts Payable arrives", "when a
//! Teams message from Dana about the budget", "when a task is assigned to
//! me"). `lyra serve` looks every couple of minutes in the person's own
//! accounts (as watches do); what's new since the last look starts one run of
//! the routine, told what set it off. The run has the routine's own rights:
//! it only looks, or (`changes`) each change asks the person, as any routine.
//! What each automation has seen is kept beside the routines (`triggers.json`).

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What sets an automation off.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Mail,
    Teams,
    Task,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Trigger {
    pub kind: Kind,
    /// Who it's from (a name or address); empty: anyone.
    pub from: String,
    /// Words it's about (in the subject, the message or the task); empty: anything.
    pub about: String,
    /// Tasks: what happened ("assigned", "comment", "done", "created", "edited"); empty: any change.
    pub what: String,
}

/// Whether a schedule is an event ("when …", "on …"): its trigger, or why it can't be one.
pub fn parse(schedule: &str) -> Option<Result<Trigger, String>> {
    let s = schedule.trim();
    let lower = s.to_ascii_lowercase();
    let rest = ["when ", "whenever ", "on "].iter().find_map(|lead| lower.starts_with(lead).then(|| &s[lead.len()..]))?;
    Some(parse_event(rest))
}

fn parse_event(text: &str) -> Result<Trigger, String> {
    let lower = text.to_ascii_lowercase();
    let has = |w: &str| lower.split(|c: char| !c.is_ascii_alphanumeric()).any(|x| x == w);
    let kind = if has("teams") {
        Kind::Teams
    } else if has("mail") || has("email") || has("mails") || has("emails") || lower.contains("e-mail") {
        Kind::Mail
    } else if has("task") || has("tasks") {
        Kind::Task
    } else {
        return Err("lyra reacts to mail, Teams messages and tasks: e.g. \"when mail from Accounts Payable arrives\", \"when a Teams message from Dana\", \"when a task is assigned to me\"".into());
    };
    // Where a part ends: the next part, or words that only say it happened.
    const ENDS: [&str; 14] = [" about ", " mentioning ", " with ", " that ", " arrives", " comes in", " is received", " gets here", " on teams", " in teams", " is ", " was ", ",", " and "];
    let after = |lead: &str| -> String {
        let at = if lower.starts_with(lead.trim_start()) { Some(lead.trim_start().len()) } else { lower.find(lead).map(|i| i + lead.len()) };
        let Some(start) = at else { return String::new() };
        let tail = &lower[start..];
        let end = ENDS.iter().filter_map(|e| tail.find(e)).min().unwrap_or(tail.len());
        text[start..start + end].trim().trim_matches(|c| c == '"' || c == '\'' || c == '“' || c == '”').trim().to_string()
    };
    let mut from = after(" from ");
    if ["anyone", "anybody", "someone", "somebody"].contains(&from.to_lowercase().as_str()) {
        from.clear();
    }
    let mut about = String::new();
    for lead in [" about ", " mentioning ", " with "] {
        about = after(lead);
        if !about.is_empty() {
            break;
        }
    }
    let about = about.trim_start_matches("subject ").trim_start_matches("the subject ").trim().to_string();
    let what = if kind == Kind::Task {
        let word = |ws: &[&str]| ws.iter().any(|w| lower.contains(w));
        if word(&["assigned", "given to me", "gives me"]) {
            "assigned"
        } else if word(&["comment"]) {
            "comment"
        } else if word(&["done", "completed", "finished", "closed"]) {
            "done"
        } else if word(&["created", "added", "new task"]) {
            "created"
        } else {
            ""
        }
    } else {
        ""
    };
    if from.contains('…') || about.contains('…') || from.contains("...") {
        return Err("say who or what in place of the …".into());
    }
    if kind != Kind::Task && from.is_empty() && about.is_empty() {
        return Err(format!("say from whom or about what, or it would run on every {}: e.g. \"when mail from Accounts Payable arrives\" or \"when mail about invoices\"", if kind == Kind::Mail { "mail" } else { "Teams message" }));
    }
    Ok(Trigger { kind, from, about, what: what.into() })
}

/// "each new mail from Accounts Payable about invoices".
pub fn describe(t: &Trigger) -> String {
    let mut out = match t.kind {
        Kind::Mail => "each new mail".to_string(),
        Kind::Teams => "each new Teams message".to_string(),
        Kind::Task => match t.what.as_str() {
            "assigned" => "each task given to you".to_string(),
            "comment" => "each comment on your tasks".to_string(),
            "done" => "each of your tasks someone finishes".to_string(),
            "created" => "each task made for you".to_string(),
            _ => "each change someone makes to your tasks".to_string(),
        },
    };
    if !t.from.is_empty() {
        out += &format!(" from {}", t.from);
    }
    if !t.about.is_empty() {
        out += &format!(" about \"{}\"", t.about);
    }
    out
}

fn has_words(text: &str, words: &str) -> bool {
    words.is_empty() || text.to_lowercase().contains(&words.to_lowercase())
}

/// A mail that sets it off: from them, about it, not a draft.
pub fn mail_matches(t: &Trigger, m: &Value) -> bool {
    m["isDraft"] != true
        && (t.from.is_empty() || crate::people::matches_address(&m["from"], &t.from))
        && has_words(&format!("{} {}", m["subject"].as_str().unwrap_or(""), m["bodyPreview"].as_str().unwrap_or("")), &t.about)
}

/// A Teams chat whose latest message, unread, sets it off.
pub fn teams_matches(t: &Trigger, c: &Value) -> bool {
    c["unread"] == true && (t.from.is_empty() || c["last_from"].as_str().is_some_and(|f| crate::people::name_matches(f, &t.from))) && has_words(c["last"].as_str().unwrap_or(""), &t.about)
}

/// Something someone did on one of their tasks that sets it off.
pub fn task_matches(t: &Trigger, e: &Value) -> bool {
    let kind = e["what"].as_str().unwrap_or("");
    (t.what.is_empty() || kind == t.what || (t.what == "assigned" && kind == "created"))
        && (t.from.is_empty() || e["by"].as_str().is_some_and(|b| crate::people::name_matches(b, &t.from)))
        && has_words(&format!("{} {}", e["task"]["title"].as_str().unwrap_or(""), e["text"].as_str().unwrap_or("")), &t.about)
}

fn time(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|t| t.with_timezone(&Utc))
}

/// What's happened since `since` that sets it off: (a key, what to tell the run), oldest first.
fn events(t: &Trigger, since: DateTime<Utc>) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(DateTime<Utc>, String, String)> = match t.kind {
        Kind::Mail => crate::mail::received_since(since)?
            .iter()
            .filter(|m| mail_matches(t, m))
            .map(|m| {
                let from = &m["from"]["emailAddress"];
                let text = format!(
                    "Mail from {} <{}>, received {}: \"{}\" (id {})\n{}",
                    from["name"].as_str().unwrap_or(""),
                    from["address"].as_str().unwrap_or(""),
                    m["receivedDateTime"].as_str().unwrap_or(""),
                    m["subject"].as_str().unwrap_or("(no subject)"),
                    m["id"].as_str().unwrap_or(""),
                    m["bodyPreview"].as_str().unwrap_or("").chars().take(400).collect::<String>()
                );
                (time(&m["receivedDateTime"]).unwrap_or(since), m["id"].as_str().unwrap_or("").to_string(), text)
            })
            .collect(),
        Kind::Teams => crate::teams::chats(30)?
            .iter()
            .filter(|c| teams_matches(t, c) && time(&c["sent"]).is_none_or(|at| at >= since))
            .map(|c| {
                let text = format!("Teams message from {} in \"{}\" (chat id {}): {}", c["last_from"].as_str().unwrap_or(""), c["chat"].as_str().unwrap_or(""), c["id"].as_str().unwrap_or(""), c["last"].as_str().unwrap_or(""));
                (time(&c["sent"]).unwrap_or(since), format!("{}@{}", c["id"].as_str().unwrap_or(""), c["sent"].as_str().unwrap_or("")), text)
            })
            .collect(),
        Kind::Task => crate::tasks::inbox(false)?
            .0
            .iter()
            .filter(|e| task_matches(t, e) && time(&e["at"]).is_some_and(|at| at >= since))
            .map(|e| {
                let did = match e["what"].as_str().unwrap_or("") {
                    "comment" => "commented on",
                    "assigned" => "gave you",
                    "created" => "made",
                    "done" => "finished",
                    "edited" => "changed",
                    "reopened" => "reopened",
                    "deleted" => "deleted",
                    other => other,
                };
                let text = format!("{} {did} the task \"{}\" (id {}){}", e["by"].as_str().unwrap_or("someone"), e["task"]["title"].as_str().unwrap_or(""), e["task"]["id"].as_str().unwrap_or(""), e["text"].as_str().filter(|x| !x.is_empty()).map_or(String::new(), |x| format!(": {x}")));
                (time(&e["at"]).unwrap_or(since), e["id"].as_str().unwrap_or("").to_string(), text)
            })
            .collect(),
    };
    out.sort_by_key(|(at, _, _)| *at);
    Ok(out.into_iter().filter(|(_, k, _)| !k.is_empty()).map(|(_, k, x)| (k, x)).collect())
}

/// What an automation has seen.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Seen {
    since: Option<DateTime<Utc>>,
    keys: Vec<String>,
}

fn store() -> Option<crate::store::JsonStore<HashMap<String, Seen>>> {
    Some(crate::store::JsonStore::new(crate::routines::dir()?.join("triggers.json")))
}

/// This person's automations (enabled, with a trigger that reads).
fn automations() -> Vec<(crate::routines::Routine, Trigger)> {
    crate::routines::list().into_iter().filter(|r| r.enabled).filter_map(|r| parse(&r.schedule)?.ok().map(|t| (r, t))).collect()
}

/// Someone has an automation (the loop only looks for people who do).
pub fn any(user: &str) -> bool {
    crate::acting::run(user, || !automations().is_empty())
}

/// Most a run is told about at once.
const AT_ONCE: usize = 5;

/// Which of these haven't been seen, and what's kept as seen after.
fn fresh(seen: &Seen, found: Vec<(String, String)>) -> (Vec<String>, Vec<String>) {
    let new: Vec<(String, String)> = found.into_iter().filter(|(k, _)| !seen.keys.contains(k)).collect();
    let mut keys = seen.keys.clone();
    keys.extend(new.iter().map(|(k, _)| k.clone()));
    let skip = keys.len().saturating_sub(300);
    (new.into_iter().map(|(_, x)| x).collect(), keys.split_off(skip))
}

/// Look for what's new for the person's automations: each with something new
/// (and not running already) is asked to run, told what. Returns what started.
pub fn pass() -> Vec<String> {
    let user = crate::acting::current();
    let all = automations();
    let Some(store) = store() else { return Vec::new() };
    if all.is_empty() {
        return Vec::new();
    }
    let running: Vec<String> = crate::routines::running_for(&user).into_iter().map(|(n, _)| n).collect();
    let now = Utc::now();
    let mut seen = store.load();
    let mut started = Vec::new();
    for (r, t) in all {
        // Running: what's new waits for the next look (it isn't marked seen).
        if running.contains(&r.name) {
            continue;
        }
        let mine = seen.entry(r.name.clone()).or_default();
        // A little overlap: mail can show up a minute after it arrived.
        let since = mine.since.map_or(r.created, |s| s - Duration::minutes(10)).max(now - Duration::days(2));
        let Ok(found) = events(&t, since) else { continue };
        let (new, keys) = fresh(mine, found);
        mine.keys = keys;
        mine.since = Some(now);
        if new.is_empty() {
            continue;
        }
        let shown: Vec<String> = new.iter().take(AT_ONCE).cloned().collect();
        let more = new.len().saturating_sub(AT_ONCE);
        let what = shown.join("\n\n") + &if more > 0 { format!("\n\n(and {more} more like it: look for them yourself)") } else { String::new() };
        crate::routines::request_event(&user, &r.name, &what);
        started.push(format!("automation {}: {} new ({})", r.name, new.len(), describe(&t)));
    }
    let names: Vec<String> = crate::routines::list().into_iter().map(|r| r.name).collect();
    let _ = store.update(|all| {
        for (k, v) in &seen {
            all.insert(k.clone(), v.clone());
        }
        all.retain(|k, _| names.contains(k));
    });
    started
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t(s: &str) -> Trigger {
        parse(s).expect("a trigger").expect("it reads")
    }

    #[test]
    fn events_read_as_people_write_them() {
        assert!(parse("every day at 07:00").is_none(), "a time isn't an event");
        let ap = t("when a mail from Accounts Payable arrives");
        assert_eq!((ap.kind, ap.from.as_str(), ap.about.as_str()), (Kind::Mail, "Accounts Payable", ""));
        let inv = t("When email from ap@vendor.example about \"Invoice\" comes in");
        assert_eq!((inv.from.as_str(), inv.about.as_str()), ("ap@vendor.example", "Invoice"));
        assert_eq!(t("whenever mail about the budget").about, "the budget");
        let dana = t("when a Teams message from Dana Whitfield mentioning release");
        assert_eq!((dana.kind, dana.from.as_str(), dana.about.as_str()), (Kind::Teams, "Dana Whitfield", "release"));
        assert_eq!(t("when a task is assigned to me").what, "assigned");
        assert_eq!(t("when someone comments on my tasks").what, "comment");
        assert_eq!(t("when a task changes").what, "");
        assert!(parse("when mail arrives").unwrap().is_err(), "every mail is too much");
        assert!(parse("when it rains").unwrap().is_err());
        assert!(parse("when mail from … arrives").unwrap().is_err(), "the example's blank, not filled in");
        assert_eq!(describe(&inv), "each new mail from ap@vendor.example about \"Invoice\"");
        assert_eq!(describe(&t("on a task assigned to me")), "each task given to you");
    }

    #[test]
    fn only_what_matches_and_wasnt_seen_sets_it_off() {
        let ap = t("when mail from Accounts Payable about invoice");
        let mail = |name: &str, subject: &str| json!({ "from": { "emailAddress": { "name": name, "address": "ap@vendor.example" } }, "subject": subject, "bodyPreview": "" });
        assert!(mail_matches(&ap, &mail("Accounts Payable", "Invoice 4410 due")));
        assert!(!mail_matches(&ap, &mail("Accounts Payable", "Lunch")), "about something else");
        assert!(!mail_matches(&ap, &mail("Dana", "Invoice")), "from someone else");
        let mut draft = mail("Accounts Payable", "Invoice");
        draft["isDraft"] = json!(true);
        assert!(!mail_matches(&ap, &draft));
        let chat = json!({ "unread": true, "last_from": "Dana Whitfield", "last": "the release is Friday" });
        assert!(teams_matches(&t("when a Teams message from Dana about release"), &chat));
        assert!(!teams_matches(&t("when a Teams message from Sam"), &chat));
        let given = json!({ "what": "assigned", "by": "Dana Whitfield", "text": "", "task": { "title": "Pay invoice" } });
        assert!(task_matches(&t("when a task is assigned to me"), &given) && task_matches(&t("when a task changes"), &given));
        assert!(!task_matches(&t("when someone comments on a task"), &given));
        // Seen once: not again.
        let seen = Seen { since: None, keys: vec!["a".into()] };
        let (new, keys) = fresh(&seen, vec![("a".into(), "old".into()), ("b".into(), "new".into())]);
        assert_eq!((new, keys), (vec!["new".to_string()], vec!["a".to_string(), "b".to_string()]));
    }
}
