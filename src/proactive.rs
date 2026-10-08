//! Proactive help (`[proactive]`), per person with Outlook connected:
//! - meeting prep: a push shortly before a meeting with other people (who,
//!   the latest mail with them, open PMI tasks about it);
//! - mail triage: new mail from people that asks something of them becomes a
//!   personal PMI task (with its due date), is flagged, and gets a reply
//!   drafted into Drafts (never sent) — all theirs alone, done and then told;
//! - follow-ups: mail they sent that asked something and got no answer.
//!
//! What was done is kept per person (`proactive.json`) so nothing happens twice.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

use chrono::{DateTime, Duration, Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// `[proactive]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// Meeting prep this many minutes before.
    pub prep_minutes: i64,
    /// New mail that asks something becomes a personal task (PMI), is flagged, gets a draft reply.
    pub mail_tasks: bool,
    pub flag: bool,
    pub drafts: bool,
    /// Mail sent without an answer for this many days gets a nudge.
    pub followup_days: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: true, prep_minutes: 15, mail_tasks: true, flag: true, drafts: true, followup_days: 3 }
    }
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

pub fn configure(s: Settings) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

pub fn settings() -> Settings {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

/// What's been done for a person already.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Seen {
    /// Meetings prepped (event id → when).
    #[serde(default)]
    pub prepped: HashMap<String, DateTime<Utc>>,
    /// Mail looked at (message id → when).
    #[serde(default)]
    pub mail: HashMap<String, DateTime<Utc>>,
    /// Sent mail nudged about.
    #[serde(default)]
    pub nudged: HashMap<String, DateTime<Utc>>,
    /// The last mail looked at was received then.
    #[serde(default)]
    pub mail_since: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_followups: Option<DateTime<Utc>>,
    /// Teams meetings a follow-up was offered for (event id → when).
    #[serde(default)]
    pub meetings_offered: HashMap<String, DateTime<Utc>>,
    /// The last try at learning how they write.
    #[serde(default)]
    pub style_tried: Option<DateTime<Utc>>,
}

fn path() -> Option<PathBuf> {
    let user = crate::acting::current();
    if crate::acting::is_owner(&user) { Some(crate::config::home()?.join("proactive.json")) } else { Some(crate::context::user_dir(&user)?.join("proactive.json")) }
}

impl Seen {
    pub fn load() -> Self {
        path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&mut self) {
        // A few weeks is plenty to remember.
        let cutoff = Utc::now() - Duration::days(30);
        for m in [&mut self.prepped, &mut self.mail, &mut self.nudged] {
            m.retain(|_, t| *t > cutoff);
        }
        if let Some(p) = path() {
            let _ = crate::store::write_json(&p, self);
        }
    }
}

// ---- meeting prep

/// Meetings with other people starting within `minutes` (and not yet prepped).
pub fn due_for_prep<'a>(events: &'a [Value], now: DateTime<Utc>, minutes: i64, seen: &Seen) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|e| e["isCancelled"] != true && e["isAllDay"] != true)
        .filter(|e| !e["categories"].as_array().is_some_and(|c| c.iter().any(|x| x == "lyra")))
        .filter(|e| e["attendees"].as_array().is_some_and(|a| !a.is_empty()))
        .filter(|e| e["id"].as_str().is_some_and(|id| !seen.prepped.contains_key(id)))
        .filter(|e| crate::calendar::utc(&e["start"]).is_some_and(|s| s > now && s - now <= Duration::minutes(minutes)))
        .collect()
}

fn names(e: &Value) -> Vec<(String, String)> {
    e["attendees"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let addr = a["emailAddress"]["address"].as_str()?.to_string();
            let name = a["emailAddress"]["name"].as_str().filter(|n| !n.is_empty()).map_or_else(|| addr.clone(), str::to_string);
            Some((name, addr))
        })
        .collect()
}

/// The prep note for a meeting: who, the latest mail with them, open tasks about it.
fn prep(e: &Value, me: &str) -> (String, String) {
    let subject = e["subject"].as_str().unwrap_or("a meeting").to_string();
    let at = crate::calendar::utc(&e["start"]).map(|t| t.with_timezone(&Local).format("%H:%M").to_string()).unwrap_or_default();
    let people: Vec<(String, String)> = names(e).into_iter().filter(|(_, a)| !a.eq_ignore_ascii_case(me)).take(6).collect();
    let mut lines = Vec::new();
    if !people.is_empty() {
        lines.push(format!("With {}", people.iter().map(|(n, _)| n.split_whitespace().next().unwrap_or(n).to_string()).collect::<Vec<_>>().join(", ")));
    }
    if let Some(l) = e["location"]["displayName"].as_str().filter(|l| !l.is_empty()) {
        lines.push(l.chars().take(80).collect());
    }
    // The latest mail with the first few of them.
    for (name, addr) in people.iter().take(3) {
        if let Ok(v) = crate::mail::call("mail_search", &json!({ "query": format!("from:{addr}"), "count": 1 }))
            && let Some(m) = v["messages"].as_array().and_then(|m| m.first())
        {
            lines.push(format!("Last from {}: {} ({})", name.split_whitespace().next().unwrap_or(name), m["subject"].as_str().unwrap_or(""), m["received"].as_str().unwrap_or("")));
        }
    }
    // Open PMI tasks that share a word with the meeting.
    if crate::pmi::configured_for(&crate::acting::current()) {
        let words: Vec<String> = subject.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 5).map(str::to_lowercase).collect();
        if let Ok(v) = crate::pmi::call("pmi_tasks", &json!({})) {
            let related: Vec<String> = v["tasks"].as_array().into_iter().flatten().filter(|t| t["title"].as_str().is_some_and(|tt| words.iter().any(|w| tt.to_lowercase().contains(w)))).filter_map(|t| t["title"].as_str().map(str::to_string)).take(3).collect();
            if !related.is_empty() {
                lines.push(format!("Open: {}", related.join(" · ")));
            }
        }
    }
    (format!("📋 {at} {subject}"), lines.join("\n"))
}

// ---- mail triage

/// What the model says about one email.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
pub struct Triage {
    /// It asks the user to do something (or answer).
    #[serde(default)]
    pub needs_user: bool,
    /// The task, short, when there's something to do.
    #[serde(default)]
    pub task: Option<String>,
    /// When it's due, as said ("friday", "oct 14"), if it says.
    #[serde(default)]
    pub due: Option<String>,
    /// A reply worth drafting, in the user's voice.
    #[serde(default)]
    pub draft: Option<String>,
}

pub fn parse_triage(reply: &str) -> Option<Triage> {
    let reply = reply.rsplit_once("</think>").map_or(reply, |(_, after)| after);
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    serde_json::from_str(&reply[start..=end]).ok()
}

const TRIAGE: &str = "You look at one email for its recipient, a busy IT professional. Decide if it asks them to do or answer something. \
Answer with JSON only: {\"needs_user\": bool, \"task\": \"a short to-do in the imperative, or null\", \"due\": \"when it's due in plain words (friday, oct 14, tomorrow) or null\", \
\"draft\": \"a short reply in the recipient's own voice (plain, friendly, no sign-off name), or null when none is needed\"}. \
Newsletters, notifications, receipts and FYIs don't need them. Never invent facts in the draft: acknowledge, ask, or say when they'll do it.";

/// Look at new mail from people: tasks, flags and drafts for what asks something.
/// Returns what was done, in words.
pub fn triage_mail(seen: &mut Seen, url: &str, model: &str) -> Vec<String> {
    let s = settings();
    let user = crate::acting::current();
    if !crate::mail::connected_for(&user) || !(s.mail_tasks || s.flag || s.drafts) {
        return vec![];
    }
    // On first sight, start from now: old mail isn't triaged.
    let since = *seen.mail_since.get_or_insert_with(Utc::now);
    let Ok(new) = crate::mail::inbox(true, true, 15) else { return vec![] };
    let mut did = Vec::new();
    let mut newest = since;
    for m in new.iter().rev() {
        let (Some(id), Some(at)) = (m["id"].as_str(), m["receivedDateTime"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|t| t.with_timezone(&Utc))) else { continue };
        if at <= since || seen.mail.contains_key(id) {
            continue;
        }
        newest = newest.max(at);
        seen.mail.insert(id.to_string(), Utc::now());
        let Ok(full) = crate::mail::call("mail_read", &json!({ "id": id })) else { continue };
        let text = format!("From: {}\nSubject: {}\n\n{}", full["from"].as_str().unwrap_or(""), full["subject"].as_str().unwrap_or(""), full["text"].as_str().unwrap_or("").chars().take(4000).collect::<String>());
        // Drafts in their own voice when lyra knows it.
        let system = match crate::style::section(&user) {
            Some(st) => format!("{TRIAGE}\n\n{st}"),
            None => TRIAGE.to_string(),
        };
        let Some(t) = crate::learn::complete(url, model, &system, &text).ok().and_then(|(r, _)| parse_triage(&r)) else { continue };
        if !t.needs_user {
            continue;
        }
        let who = full["from"].as_str().unwrap_or("someone").split(" <").next().unwrap_or("someone").to_string();
        let subject = full["subject"].as_str().unwrap_or("").to_string();
        if s.mail_tasks
            && crate::pmi::configured_for(&user)
            && let Some(task) = t.task.as_deref().filter(|x| !x.trim().is_empty())
        {
            let mut args = json!({ "title": task, "description": format!("From {who}'s email \"{subject}\"") });
            if let Some(d) = t.due.as_deref().filter(|d| !d.trim().is_empty() && crate::when::parse(d, Local::now()).is_some()) {
                args["due"] = json!(d);
            }
            if crate::pmi::call("pmi_add_task", &args).is_ok() {
                did.push(format!("task from {who}'s email: {task}{}", t.due.as_deref().map(|d| format!(" (due {d})")).unwrap_or_default()));
            }
        }
        if s.flag && crate::mail::call("mail_tidy", &json!({ "id": id, "action": "flag" })).is_ok() {
            did.push(format!("flagged {who}'s \"{subject}\""));
        }
        if s.drafts
            && let Some(draft) = t.draft.as_deref().filter(|d| !d.trim().is_empty())
            && crate::mail::call("mail_draft", &json!({ "reply_to": id, "body": draft })).is_ok()
        {
            did.push(format!("drafted a reply to {who} (in Drafts, not sent)"));
        }
    }
    seen.mail_since = Some(newest);
    did
}

// ---- follow-ups

/// Sent mail that asked something, `days` or more ago, with no answer since.
pub fn followups(seen: &mut Seen) -> Vec<String> {
    let s = settings();
    let user = crate::acting::current();
    if !crate::mail::connected_for(&user) {
        return vec![];
    }
    let now = Utc::now();
    if seen.last_followups.is_some_and(|t| now - t < Duration::hours(20)) {
        return vec![];
    }
    seen.last_followups = Some(now);
    let Ok(sent) = crate::mail::sent_between(now - Duration::days(s.followup_days + 7), now - Duration::days(s.followup_days)) else { return vec![] };
    let mut out = Vec::new();
    for m in sent.iter().filter(|m| m["bodyPreview"].as_str().is_some_and(|p| p.contains('?'))).take(15) {
        let (Some(id), Some(conv), Some(at)) = (m["id"].as_str(), m["conversationId"].as_str(), m["sentDateTime"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok())) else { continue };
        if seen.nudged.contains_key(id) {
            continue;
        }
        let answered = crate::mail::conversation(conv).is_ok_and(|all| {
            all.iter().any(|x| x["receivedDateTime"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).is_some_and(|t| t > at) && x["isDraft"] != true && x["id"] != m["id"] && x["from"]["emailAddress"]["address"] != m["from"]["emailAddress"]["address"])
        });
        if answered {
            continue;
        }
        seen.nudged.insert(id.to_string(), now);
        let to = m["toRecipients"].as_array().and_then(|t| t.first()).and_then(|t| t["emailAddress"]["name"].as_str()).unwrap_or("them");
        let days = (now - at.with_timezone(&Utc)).num_days();
        out.push(format!("{to} hasn't answered \"{}\" ({days} days)", m["subject"].as_str().unwrap_or("")));
    }
    out
}

/// One pass for the person this thread works for: meeting prep (pushed now)
/// and what was done or noticed (told when they're free). Meeting prep comes
/// back separately so it isn't held for quiet time within the working day.
pub struct Pass {
    pub prep: Vec<(String, String)>,
    pub done: Vec<String>,
}

pub fn pass(url: &str, model: &str, mail_due: bool) -> Pass {
    let s = settings();
    let user = crate::acting::current();
    let mut out = Pass { prep: vec![], done: vec![] };
    if !s.enabled || !crate::calendar::connected_for(&user) {
        return out;
    }
    let mut seen = Seen::load();
    let now = Utc::now();
    if let Ok(events) = crate::calendar::events(now, now + Duration::minutes(s.prep_minutes + 5)) {
        let me = lyra_web::Users::open(&crate::config::home().unwrap_or_default().join("web")).get(&user).map(|u| u.email).unwrap_or_default();
        for e in due_for_prep(&events, now, s.prep_minutes, &seen) {
            if let Some(id) = e["id"].as_str() {
                seen.prepped.insert(id.to_string(), now);
            }
            out.prep.push(prep(e, &me));
        }
    }
    // A Teams meeting just ended: offer the follow-up (once), when its transcript can be read.
    if crate::meetings::ready(&user)
        && let Ok(ended) = crate::calendar::events(now - Duration::minutes(100), now)
    {
        for e in crate::meetings::just_ended(&ended, now) {
            let Some(id) = e["id"].as_str() else { continue };
            if seen.meetings_offered.contains_key(id) {
                continue;
            }
            seen.meetings_offered.insert(id.to_string(), now);
            let title = e["subject"].as_str().unwrap_or("your meeting");
            out.prep.push((format!("Follow up on {title}?"), format!("Ask lyra \"follow up on {title}\": a summary, decisions, action items as tasks, and a follow-up mail to draft.")));
        }
        seen.meetings_offered.retain(|_, t| now - *t < Duration::days(14));
    }
    if mail_due {
        // How they write: learned once mail is connected, again each week (tried once a day).
        if crate::mail::connected_for(&user) && crate::style::stale(&user) && seen.style_tried.is_none_or(|t| now - t > Duration::hours(20)) {
            seen.style_tried = Some(now);
            if let Ok(line) = crate::style::learn(url, model) {
                out.done.push(line);
            }
        }
        out.done.extend(triage_mail(&mut seen, url, model));
        out.done.extend(followups(&mut seen));
    }
    seen.save();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: &str, mins: i64, attendees: usize) -> Value {
        let start = Utc::now() + Duration::minutes(mins);
        json!({
            "id": id, "subject": "Budget review",
            "start": crate::calendar::graph_time(start), "end": crate::calendar::graph_time(start + Duration::minutes(30)),
            "attendees": (0..attendees).map(|i| json!({ "emailAddress": { "name": format!("P{i}"), "address": format!("p{i}@x.org") } })).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn prep_comes_once_shortly_before_meetings_with_people() {
        let now = Utc::now();
        let mut seen = Seen::default();
        let events = vec![ev("soon", 10, 2), ev("alone", 10, 0), ev("later", 60, 2), ev("past", -5, 2)];
        let due: Vec<&str> = due_for_prep(&events, now, 15, &seen).iter().filter_map(|e| e["id"].as_str()).collect();
        assert_eq!(due, ["soon"], "with people, within 15 minutes, not started");
        seen.prepped.insert("soon".into(), now);
        assert!(due_for_prep(&events, now, 15, &seen).is_empty(), "once");
        let mut focus = ev("focus", 10, 2);
        focus["categories"] = json!(["lyra"]);
        assert!(due_for_prep(&[focus], now, 15, &Seen::default()).is_empty(), "not lyra's own blocks");
    }

    #[test]
    fn triage_answers_are_read_even_wrapped() {
        let t = parse_triage("<think>hmm</think> Here: {\"needs_user\": true, \"task\": \"Send Dana the PO\", \"due\": \"friday\", \"draft\": \"Will do by Friday.\"}").unwrap();
        assert_eq!(t, Triage { needs_user: true, task: Some("Send Dana the PO".into()), due: Some("friday".into()), draft: Some("Will do by Friday.".into()) });
        assert_eq!(parse_triage("{\"needs_user\": false}").unwrap(), Triage::default());
        assert!(parse_triage("no json").is_none());
    }
}
