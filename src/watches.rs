//! "Tell me when …": things a person asked lyra to watch for: mail from
//! someone, a reply on a thread they sent, a PMI task changing, a Teams
//! message from someone, a web page changing or a search turning up
//! something new. Checked every couple of minutes in their own accounts (the
//! web ones on their own schedule, every few hours); when one happens they
//! get a push, and the watch ends. Kept per person (`watches.json`).
//!
//! A web watch can wait for something particular (`when`): a price below a
//! number ("below 500": the lowest price on the page, in $, €, £), or words
//! to appear ("in stock"). Without one, any change counts.

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
    /// A web watch's condition: "below 500", or words to look for (empty: any change).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub when: String,
    /// How often a web watch looks (hours).
    #[serde(default = "six")]
    pub every_hours: u32,
}

fn six() -> u32 {
    6
}

fn path(user: &str) -> Option<PathBuf> {
    if user == lyra_web::users::OWNER { Some(crate::config::home()?.join("watches.json")) } else { Some(crate::context::user_dir(user)?.join("watches.json")) }
}

fn store(user: &str) -> Result<crate::store::JsonStore<Vec<Watch>>, String> {
    Ok(crate::store::JsonStore::new(path(user).ok_or("no home directory")?))
}

pub fn list_for(user: &str) -> Vec<Watch> {
    store(user).map(|s| s.load()).unwrap_or_default()
}

/// Anyone has a watch (the loop only looks for people who do).
pub fn any(user: &str) -> bool {
    !list_for(user).is_empty()
}

fn new_id() -> String {
    lyra_learning::Uuid::new_v4().simple().to_string()[..6].to_string()
}

/// Start watching, for the person this thread works for.
/// A web watch also takes what to wait for (`when`) and how often it looks.
pub fn add(kind: &str, target: &str, days: i64, when: &str, every_hours: u32) -> Result<Watch, String> {
    let user = crate::acting::current();
    let target = target.trim();
    if target.is_empty() {
        return Err("watch for what? a person, a subject or a task id".into());
    }
    let now = Utc::now();
    let (label, state) = match kind {
        "mail_from" => {
            if !crate::mail::connected_for(&user) {
                return Err("your Outlook isn't connected: in the app, Profile → Connections → Outlook → Connect".into());
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
                return Err("lyra can't see your Teams chats yet: connect again (Profile → Connections → Outlook → Add Teams & files)".into());
            }
            (format!("a Teams message from {target}"), Value::Null)
        }
        // The page as it is now, to compare with.
        "web_page" => {
            if !crate::websearch::configured().enabled {
                return Err("web pages are off ([search] enabled)".into());
            }
            let (title, text) = crate::websearch::page(target)?;
            let label = match web_condition(when) {
                Some(Condition::Below(n)) => format!("the price on {} to drop below {n}", short(&title, target)),
                Some(Condition::Words(w)) => format!("\"{w}\" on {}", short(&title, target)),
                None => format!("{} to change", short(&title, target)),
            };
            (label, json!({ "text": baseline(&text), "next": next_look(now, every_hours) }))
        }
        // What the search finds now: only new results count.
        "web_search" => {
            let results = crate::websearch::found(target)?;
            let label = match web_condition(when) {
                Some(Condition::Words(w)) => format!("a new result for \"{target}\" mentioning \"{w}\""),
                _ => format!("a new result for \"{target}\""),
            };
            (label, json!({ "seen": results.iter().map(|(_, u)| u.clone()).collect::<Vec<_>>(), "next": next_look(now, every_hours) }))
        }
        other => return Err(format!("can't watch for {other:?}: mail_from, mail_reply, task, teams_from, web_page or web_search")),
    };
    let w = Watch {
        id: new_id(),
        kind: kind.into(),
        target: target.into(),
        label,
        created: now,
        state,
        until: now + Duration::days(days.clamp(1, 60)),
        when: when.trim().into(),
        every_hours: every_hours.clamp(1, 168),
    };
    store(&user)?.update(|all| all.push(w.clone()))?;
    Ok(w)
}

pub fn cancel(user: &str, id: &str) -> Result<Watch, String> {
    let key = id.trim().to_lowercase();
    if key.is_empty() {
        return Err("which watch? (its id from /watches)".into());
    }
    store(user)?.try_update(|all| {
        let i = all.iter().position(|w| w.id == key || w.label.to_lowercase().contains(&key)).ok_or_else(|| format!("no watch {id:?}"))?;
        Ok(all.remove(i))
    })
}

/// What a web watch waits for.
#[derive(Debug, Clone, PartialEq)]
enum Condition {
    /// The lowest price on the page under this.
    Below(f64),
    /// These words on the page (or in a new result).
    Words(String),
}

fn web_condition(when: &str) -> Option<Condition> {
    let w = when.trim().to_lowercase();
    if w.is_empty() {
        return None;
    }
    for lead in ["below ", "under ", "less than ", "cheaper than ", "< "] {
        if let Some(rest) = w.strip_prefix(lead)
            && let Some(n) = number(rest)
        {
            return Some(Condition::Below(n));
        }
    }
    Some(Condition::Words(w.trim_start_matches("contains ").trim().to_string()))
}

/// The first number in `s` ("$1,299.00" → 1299).
fn number(s: &str) -> Option<f64> {
    let digits: String = s.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.').filter(|c| *c != ',').collect();
    digits.trim_end_matches('.').parse().ok()
}

/// Every price on a page ($, €, £ before the number).
fn prices(text: &str) -> Vec<f64> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if matches!(c, '$' | '€' | '£') {
            let rest: String = chars[i + 1..].iter().take(16).collect();
            if rest.trim_start().starts_with(|d: char| d.is_ascii_digit())
                && let Some(n) = number(&rest)
            {
                out.push(n);
            }
        }
    }
    out
}

/// A page's text as kept to compare: lines, spaces tidied.
fn baseline(text: &str) -> String {
    text.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n").chars().take(20_000).collect()
}

fn next_look(now: DateTime<Utc>, hours: u32) -> String {
    (now + Duration::hours(i64::from(hours.clamp(1, 168)))).to_rfc3339()
}

fn short(title: &str, url: &str) -> String {
    let t = title.trim();
    if t.is_empty() { url.to_string() } else { t.chars().take(60).collect() }
}

/// A web watch's look, when it's due: whether it happened (what to say),
/// and the watch's state from now on (its next look).
fn check_web(w: &Watch, now: DateTime<Utc>, page: impl Fn(&str) -> Result<(String, String), String>, found: impl Fn(&str) -> Result<Vec<(String, String)>, String>) -> Result<(Option<String>, Value), String> {
    let due = w.state["next"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).is_none_or(|t| now >= t.with_timezone(&Utc));
    if !due {
        return Ok((None, w.state.clone()));
    }
    let mut state = w.state.clone();
    state["next"] = json!(next_look(now, w.every_hours));
    let condition = web_condition(&w.when);
    let fired = match w.kind.as_str() {
        "web_page" => {
            let (title, text) = page(&w.target)?;
            let now_text = baseline(&text);
            let lower = now_text.to_lowercase();
            match condition {
                Some(Condition::Below(n)) => prices(&now_text).into_iter().filter(|p| *p > 0.0).reduce(f64::min).filter(|p| *p < n).map(|p| format!("{} is now {p} (below {n}): {}", short(&title, &w.target), w.target)),
                Some(Condition::Words(words)) => lower.contains(&words).then(|| format!("\"{words}\" is on {} now: {}", short(&title, &w.target), w.target)),
                None => {
                    let before: std::collections::HashSet<&str> = w.state["text"].as_str().unwrap_or("").lines().collect();
                    let new: Vec<&str> = now_text.lines().filter(|l| !before.contains(l)).collect();
                    (now_text != w.state["text"].as_str().unwrap_or("")).then(|| {
                        let first = new.first().map_or(String::new(), |l| format!(": \"{}\"", l.chars().take(140).collect::<String>()));
                        format!("{} changed{first} — {}", short(&title, &w.target), w.target)
                    })
                }
            }
        }
        "web_search" => {
            let seen: Vec<String> = w.state["seen"].as_array().into_iter().flatten().filter_map(|u| u.as_str().map(str::to_string)).collect();
            let results = found(&w.target)?;
            let fresh: Vec<&(String, String)> = results
                .iter()
                .filter(|(t, u)| !seen.contains(u) && match &condition {
                    Some(Condition::Words(words)) => t.to_lowercase().contains(words),
                    _ => true,
                })
                .collect();
            let mut all = seen.clone();
            all.extend(results.iter().map(|(_, u)| u.clone()));
            state["seen"] = json!(all.into_iter().rev().take(60).collect::<Vec<_>>());
            fresh.first().map(|(t, u)| format!("new for \"{}\": {t} — {u}", w.target))
        }
        _ => None,
    };
    Ok((fired, state))
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
    // The checks take a while (mail, PMI, Teams): made without holding the file,
    // then only the ones that fired or ran out are taken out, so a watch added
    // or cancelled meanwhile stays as it is.
    let mut done = Vec::new();
    let mut fired = Vec::new();
    // Web watches carry on with a new state (their next look, what's been seen).
    let mut states: Vec<(String, Value)> = Vec::new();
    for w in &all {
        if now > w.until {
            fired.push(("Stopped watching".to_string(), format!("{} (nothing in {} days)", w.label, (w.until - w.created).num_days())));
            done.push(w.id.clone());
            continue;
        }
        let happened = if w.kind.starts_with("web_") {
            match check_web(w, now, crate::websearch::page, crate::websearch::found) {
                Ok((what, state)) => {
                    if state != w.state {
                        states.push((w.id.clone(), state));
                    }
                    what
                }
                Err(_) => None,
            }
        } else {
            check(w).ok().flatten()
        };
        if let Some(what) = happened {
            fired.push((format!("You asked: {}", w.label), what));
            done.push(w.id.clone());
        }
    }
    if (!done.is_empty() || !states.is_empty())
        && let Ok(s) = store(&user)
    {
        let _ = s.update(|all| {
            all.retain(|w| !done.contains(&w.id));
            for w in all.iter_mut() {
                if let Some((_, state)) = states.iter().find(|(id, _)| *id == w.id) {
                    w.state = state.clone();
                }
            }
        });
    }
    fired
}

pub fn capabilities() -> Vec<Capability> {
    let tool = |name: &str, description: &str, risk: RiskLevel, properties: Value, required: &[&str]| {
        let mut c = Capability::new(name, CapabilityKind::NativeTool, description, risk);
        c.input_schema = json!({ "type": "object", "properties": properties, "required": required });
        c.source = "watches".into();
        c.tags = ["watch", "notify", "tell", "when", "alert", "remind", "reply", "replies", "waiting", "price", "drops", "page", "website", "changes", "stock"].iter().map(|t| t.to_string()).collect();
        c
    };
    vec![
        tool(
            "watch_for",
            "\"Tell me when …\": watch for mail from someone (mail_from), a reply on a thread the user sent (mail_reply, by words in its subject), a PMI task changing status (task, by id), a Teams message from someone (teams_from), a web page changing (web_page, by URL: \"tell me when the price drops below 500\", \"when it says in stock\") or a search turning up something new (web_search). The user gets a push when it happens, once.",
            RiskLevel::LowWrite,
            json!({
                "kind": { "type": "string", "enum": ["mail_from", "mail_reply", "task", "teams_from", "web_page", "web_search"] },
                "target": { "type": "string", "description": "The person (name or address), words of the thread's subject, the task id, the page's URL, or what to search for." },
                "when": { "type": "string", "description": "Web watches: what to wait for, \"below 500\" (a price) or words to appear (\"in stock\"); empty: any change." },
                "every_hours": { "type": "integer", "description": "Web watches: how often to look (default 6)." },
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
        "watch_for" => add(
            args["kind"].as_str().unwrap_or(""),
            args["target"].as_str().unwrap_or(""),
            args["days"].as_i64().unwrap_or(14),
            args["when"].as_str().unwrap_or(""),
            args["every_hours"].as_u64().map_or(6, |h| h as u32),
        )
        .map(|w| json!({ "watching": view(&w), "note": "the user gets a push when it happens" })),
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

    fn watch(kind: &str, when: &str, state: Value) -> Watch {
        let now = Utc::now();
        Watch { id: "w1".into(), kind: kind.into(), target: "https://shop.example/tv".into(), label: "x".into(), created: now, state, until: now + Duration::days(14), when: when.into(), every_hours: 6 }
    }

    fn no_search(_: &str) -> Result<Vec<(String, String)>, String> {
        Err("no search here".into())
    }

    #[test]
    fn web_watches_tell_on_a_change_a_price_or_words_and_wait_their_turn() {
        let now = Utc::now();
        let page = |text: &'static str| move |_: &str| Ok::<_, String>(("The TV".to_string(), text.to_string()));
        // Any change: told, with the new line.
        let w = watch("web_page", "", json!({ "text": "Price $600\nIn stock: no" }));
        let (said, state) = check_web(&w, now, page("Price  $600\nIn stock: yes"), no_search).unwrap();
        assert!(said.unwrap().contains("The TV changed: \"In stock: yes\""));
        assert!(state["next"].as_str().is_some(), "the next look is set");
        // Nothing new: not told.
        assert_eq!(check_web(&w, now, page("Price $600\nIn stock: no"), no_search).unwrap().0, None);
        // Not due yet: not even looked at.
        let later = watch("web_page", "", json!({ "text": "a", "next": (now + Duration::hours(2)).to_rfc3339() }));
        let (said, state) = check_web(&later, now, |_: &str| -> Result<(String, String), String> { panic!("looked too soon") }, no_search).unwrap();
        assert_eq!((said, state), (None, later.state.clone()));
        // A price: only once it's below.
        let w = watch("web_page", "below $500", json!({ "text": "" }));
        assert_eq!(check_web(&w, now, page("Now $549.99, was $799"), no_search).unwrap().0, None);
        assert!(check_web(&w, now, page("Now $479.00 (was $1,299.00)"), no_search).unwrap().0.unwrap().contains("is now 479 (below 500)"));
        // Words: once they're there.
        let w = watch("web_page", "in stock", json!({ "text": "" }));
        assert_eq!(check_web(&w, now, page("Sold out"), no_search).unwrap().0, None);
        assert!(check_web(&w, now, page("Back IN STOCK today"), no_search).unwrap().0.unwrap().contains("\"in stock\" is on The TV"));
        // A search: only results it hadn't seen.
        let mut w = watch("web_search", "", json!({ "seen": ["https://a.example"] }));
        w.target = "fedora 45 release".into();
        let found = |_: &str| Ok::<_, String>(vec![("Old".to_string(), "https://a.example".to_string()), ("Fedora 45 is out".to_string(), "https://b.example".to_string())]);
        let (said, state) = check_web(&w, now, |_: &str| Err("no pages".to_string()), found).unwrap();
        assert!(said.unwrap().contains("Fedora 45 is out — https://b.example"));
        assert!(state["seen"].as_array().unwrap().iter().any(|u| u == "https://b.example"), "remembered");
    }

    #[test]
    fn prices_and_conditions_read_as_people_write_them() {
        assert_eq!(number("$1,299.00 today"), Some(1299.0));
        assert_eq!(prices("£20, then €15.50 and $ 9 — 30 USD"), vec![20.0, 15.5, 9.0]);
        assert_eq!(web_condition("under 500"), Some(Condition::Below(500.0)));
        assert_eq!(web_condition("Back in stock"), Some(Condition::Words("back in stock".into())));
        assert_eq!(web_condition(""), None);
    }
}
