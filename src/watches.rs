//! "Tell me when …": things a person asked lyra to watch for: mail from
//! someone, a reply on a thread they sent, a PMI task changing, a Teams
//! message from someone. Checked every couple of minutes in their own
//! accounts; when one happens they get a push, and the watch ends. Kept per
//! person (`watches.json`).

use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One thing to watch for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Watch {
    pub id: String,
    /// "mail_from", "mail_reply", "task", "teams_from".
    pub kind: String,
    /// Who (a name or address), the thread's subject, or the task's id.
    pub target: String,
    /// What it says, for the list and the push ("mail from Jeremy").
    pub label: String,
    pub created: DateTime<Utc>,
    /// For a reply: the thread, once found. For a task: its status when asked.
    #[serde(default)]
    pub state: Value,
    /// Gives up after this (default two weeks).
    pub until: DateTime<Utc>,
}

fn path(user: &str) -> Option<PathBuf> {
    if user == lyra_web::users::OWNER { Some(crate::config::home()?.join("watches.json")) } else { Some(crate::context::user_dir(user)?.join("watches.json")) }
}

pub fn list_for(user: &str) -> Vec<Watch> {
    path(user).and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_for(user: &str, all: &[Watch]) -> Result<(), String> {
    let p = path(user).ok_or("no home directory")?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(p, serde_json::to_string_pretty(all).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Anyone has a watch (the loop only looks for people who do).
pub fn any(user: &str) -> bool {
    !list_for(user).is_empty()
}

fn new_id() -> String {
    lyra_learning::Uuid::new_v4().simple().to_string()[..6].to_string()
}

/// Start watching, for the person this thread works for.
pub fn add(kind: &str, target: &str, days: i64) -> Result<Watch, String> {
    let user = crate::acting::current();
    let target = target.trim();
    if target.is_empty() {
        return Err("watch for what? a person, a subject or a task id".into());
    }
    let now = Utc::now();
    let (label, state) = match kind {
        "mail_from" => {
            if !crate::mail::connected_for(&user) {
                return Err("your Outlook isn't connected: in the app, More → Outlook → Connect".into());
            }
            (format!("mail from {target}"), Value::Null)
        }
        "mail_reply" => {
            // The thread: their latest sent mail with these words in the subject.
            let sent = crate::mail::sent_between(now - Duration::days(30), now)?;
            let w = target.to_lowercase();
            let m = sent.iter().find(|m| m["subject"].as_str().is_some_and(|s| s.to_lowercase().contains(&w))).ok_or_else(|| format!("no mail you sent in the last month has {target:?} in its subject"))?;
            (format!("a reply to \"{}\"", m["subject"].as_str().unwrap_or(target)), json!({ "conversation": m["conversationId"], "sent": m["sentDateTime"], "me": m["from"]["emailAddress"]["address"] }))
        }
        "task" => {
            let t = crate::pmi::call("pmi_task", &json!({ "id": target }))?;
            (format!("task \"{}\" to change (now {})", t["title"].as_str().unwrap_or(target), t["status"].as_str().unwrap_or("?")), json!({ "status": t["status"], "title": t["title"] }))
        }
        "teams_from" => {
            if !crate::teams::connected_for(&user) {
                return Err("lyra can't see your Teams chats yet: connect again (More → Outlook → Add Teams & files)".into());
            }
            (format!("a Teams message from {target}"), Value::Null)
        }
        other => return Err(format!("can't watch for {other:?}: mail_from, mail_reply, task or teams_from")),
    };
    let w = Watch { id: new_id(), kind: kind.into(), target: target.into(), label, created: now, state, until: now + Duration::days(days.clamp(1, 60)) };
    let mut all = list_for(&user);
    all.push(w.clone());
    save_for(&user, &all)?;
    Ok(w)
}

pub fn cancel(user: &str, id: &str) -> Result<Watch, String> {
    let mut all = list_for(user);
    let i = all.iter().position(|w| w.id == id.trim() || w.label.to_lowercase().contains(&id.trim().to_lowercase())).ok_or_else(|| format!("no watch {id:?}"))?;
    let w = all.remove(i);
    save_for(user, &all)?;
    Ok(w)
}

/// A name or address in a "Name <address>"-style sender.
fn from_matches(from: &Value, who: &str) -> bool {
    crate::people::matches_address(from, who)
}

/// Whether a watch happened, and what to say.
fn check(w: &Watch) -> Result<Option<String>, String> {
    match w.kind.as_str() {
        "mail_from" => {
            let found = crate::mail::received_since(w.created)?;
            Ok(found.iter().find(|m| m["isDraft"] != true && from_matches(&m["from"], &w.target)).map(|m| {
                format!("{} wrote: {}", m["from"]["emailAddress"]["name"].as_str().unwrap_or(&w.target), m["subject"].as_str().unwrap_or("(no subject)"))
            }))
        }
        "mail_reply" => {
            let conv = w.state["conversation"].as_str().unwrap_or("");
            let me = w.state["me"].as_str().unwrap_or("").to_lowercase();
            let all = crate::mail::conversation(conv)?;
            let since = w.created.max(w.state["sent"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map_or(w.created, |t| t.with_timezone(&Utc)));
            Ok(all
                .iter()
                .find(|m| {
                    m["isDraft"] != true
                        && m["from"]["emailAddress"]["address"].as_str().is_some_and(|a| a.to_lowercase() != me)
                        && m["receivedDateTime"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).is_some_and(|t| t > since)
                })
                .map(|m| format!("{} replied: {}", m["from"]["emailAddress"]["name"].as_str().unwrap_or("someone"), w.label.trim_start_matches("a reply to "))))
        }
        "task" => {
            let t = crate::pmi::call("pmi_task", &json!({ "id": w.target }))?;
            Ok((t["status"] != w.state["status"]).then(|| format!("\"{}\" is now {}", t["title"].as_str().unwrap_or("the task"), t["status"].as_str().unwrap_or("changed"))))
        }
        "teams_from" => {
            let chats = crate::teams::chats(30)?;
            Ok(chats.iter().find(|c| c["unread"] == true && c["last_from"].as_str().is_some_and(|f| crate::people::name_matches(f, &w.target))).map(|c| format!("{}: {}", c["last_from"].as_str().unwrap_or(&w.target), c["last"].as_str().unwrap_or(""))))
        }
        _ => Ok(None),
    }
}

/// Look at the person's watches: what happened (title, body), and the rest kept.
pub fn pass() -> Vec<(String, String)> {
    let user = crate::acting::current();
    let all = list_for(&user);
    if all.is_empty() {
        return vec![];
    }
    let now = Utc::now();
    let mut keep = Vec::new();
    let mut fired = Vec::new();
    for w in all {
        if now > w.until {
            fired.push(("Stopped watching".to_string(), format!("{} (nothing in {} days)", w.label, (w.until - w.created).num_days())));
            continue;
        }
        match check(&w) {
            Ok(Some(what)) => fired.push((format!("You asked: {}", w.label), what)),
            _ => keep.push(w),
        }
    }
    let _ = save_for(&user, &keep);
    fired
}

pub fn capabilities() -> Vec<Capability> {
    let tool = |name: &str, description: &str, risk: RiskLevel, properties: Value, required: &[&str]| {
        let mut c = Capability::new(name, CapabilityKind::NativeTool, description, risk);
        c.input_schema = json!({ "type": "object", "properties": properties, "required": required });
        c.source = "watches".into();
        c.tags = ["watch", "notify", "tell", "when", "alert", "remind", "reply", "replies", "waiting"].iter().map(|t| t.to_string()).collect();
        c
    };
    vec![
        tool(
            "watch_for",
            "\"Tell me when …\": watch for mail from someone (mail_from), a reply on a thread the user sent (mail_reply, by words in its subject), a PMI task changing status (task, by id), or a Teams message from someone (teams_from). The user gets a push when it happens.",
            RiskLevel::LowWrite,
            json!({
                "kind": { "type": "string", "enum": ["mail_from", "mail_reply", "task", "teams_from"] },
                "target": { "type": "string", "description": "The person (name or address), words of the thread's subject, or the task id." },
                "days": { "type": "integer", "description": "Give up after this many days (default 14)." },
            }),
            &["kind", "target"],
        ),
        tool("watch_list", "What lyra is watching for the user.", RiskLevel::ReadOnly, json!({}), &[]),
        tool("watch_cancel", "Stop watching for something (by its id or words of it).", RiskLevel::LowWrite, json!({ "id": { "type": "string" } }), &["id"]),
    ]
}

fn view(w: &Watch) -> Value {
    json!({ "id": w.id, "watching_for": w.label, "since": w.created.with_timezone(&chrono::Local).format("%a %b %-d %H:%M").to_string(), "until": w.until.with_timezone(&chrono::Local).format("%b %-d").to_string() })
}

pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    let user = crate::acting::current();
    match name {
        "watch_for" => add(args["kind"].as_str().unwrap_or(""), args["target"].as_str().unwrap_or(""), args["days"].as_i64().unwrap_or(14)).map(|w| json!({ "watching": view(&w), "note": "the user gets a push when it happens" })),
        "watch_list" => Ok(json!({ "watches": list_for(&user).iter().map(view).collect::<Vec<_>>() })),
        "watch_cancel" => cancel(&user, args["id"].as_str().unwrap_or("")).map(|w| json!({ "stopped": w.label })),
        other => Err(format!("{other} isn't a watch tool")),
    }
}

/// `/watches [cancel <id>]`.
pub fn command(user: &str, arg: &str) -> Result<String, String> {
    if let Some(id) = arg.trim().strip_prefix("cancel ") {
        return cancel(user, id).map(|w| format!("stopped watching for {}", w.label));
    }
    let all = list_for(user);
    if all.is_empty() {
        return Ok("Not watching for anything. Ask: \"tell me when Jeremy replies\".".into());
    }
    Ok(all.iter().map(|w| format!("[{}] {} · since {} · until {}", w.id, w.label, w.created.with_timezone(&chrono::Local).format("%b %-d %H:%M"), w.until.with_timezone(&chrono::Local).format("%b %-d"))).collect::<Vec<_>>().join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn senders_match_by_name_or_address() {
        let from = json!({ "emailAddress": { "name": "Jeremy Sorensen", "address": "jeremy.sorensen@us.ovhcloud.com" } });
        assert!(from_matches(&from, "Jeremy") && from_matches(&from, "jeremy.sorensen@us.ovhcloud.com") && from_matches(&from, "Sorensen"));
        assert!(!from_matches(&from, "Dana"));
        assert!(crate::people::name_matches("Dana Doe", "dana") && !crate::people::name_matches("Dana Doe", "Dan"));
    }
}
