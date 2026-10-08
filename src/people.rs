//! "Who is Dana?": everything lyra has on a person, for the user asking: what
//! their memory says, recent mail from them, meetings with them (the last
//! and next few weeks) and PMI tasks they share. Read-only, the asker's own
//! accounts only.

use chrono::{Duration, Utc};
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde_json::{Value, json};

pub fn capabilities() -> Vec<Capability> {
    let mut c = Capability::new(
        "who_is",
        CapabilityKind::NativeTool,
        "Everything known about a person for the user: what they remember about them, recent mail from them, meetings with them (past and coming), PMI tasks they share. Use for \"who is …\", \"what do I have with …\", \"prep me for …\".",
        RiskLevel::ReadOnly,
    );
    c.input_schema = json!({ "type": "object", "properties": { "who": { "type": "string", "description": "A name or email address." } }, "required": ["who"] });
    c.source = "people".into();
    c.tags = ["who", "person", "people", "contact", "colleague", "about"].iter().map(|t| t.to_string()).collect();
    vec![c]
}

/// The words of a name worth matching ("Juan from Dell" → juan, dell).
fn words(who: &str) -> Vec<String> {
    const SMALL: &[&str] = &["from", "at", "the", "of", "with", "and", "mr", "ms", "mrs", "dr"];
    who.split(|c: char| !c.is_alphanumeric() && c != '@' && c != '.')
        .map(str::to_lowercase)
        .filter(|w| w.len() >= 3 && !SMALL.contains(&w.as_str()))
        .collect()
}

/// The attendee in an event matching `who`: their address, or any word of
/// the name ("Juan Dell" finds Juan Pérez).
fn matches(a: &Value, who: &str) -> bool {
    let w = who.trim().to_lowercase();
    let name = a["emailAddress"]["name"].as_str().unwrap_or("").to_lowercase();
    let addr = a["emailAddress"]["address"].as_str().unwrap_or("").to_lowercase();
    if w.is_empty() {
        return false;
    }
    if addr == w {
        return true;
    }
    let name_words: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|x| !x.is_empty()).collect();
    let local = addr.split('@').next().unwrap_or("");
    words(who).iter().any(|x| name_words.contains(&x.as_str()) || (x.len() >= 4 && local.contains(x.as_str())))
}

/// A sender or attendee (`{"emailAddress": {name, address}}`) is this person.
pub fn matches_address(a: &Value, who: &str) -> bool {
    matches(a, who)
}

/// A display name is this person (any word of what was said).
pub fn name_matches(name: &str, who: &str) -> bool {
    matches(&json!({ "emailAddress": { "name": name, "address": "" } }), who)
}

/// A meeting is with them: they're on it, or its title names them.
fn with_them(e: &Value, who: &str) -> bool {
    let title = e["subject"].as_str().unwrap_or("").to_lowercase();
    let title_words: Vec<&str> = title.split(|c: char| !c.is_alphanumeric()).collect();
    e["attendees"].as_array().is_some_and(|a| a.iter().any(|x| matches(x, who))) || words(who).iter().any(|x| title_words.contains(&x.as_str()))
}

/// A person's name and email address, from the user's meetings (the last and
/// next few weeks) or their mail: what an invite needs.
pub fn email_of(who: &str) -> Option<(String, String)> {
    let w = who.trim();
    if w.contains('@') {
        return Some((w.to_string(), w.to_string()));
    }
    if crate::graph::connected_for(&crate::acting::current()) {
        let now = Utc::now();
        if let Ok(events) = crate::calendar::events(now - Duration::days(45), now + Duration::days(21))
            && let Some(a) = events.iter().flat_map(|e| e["attendees"].as_array().into_iter().flatten()).find(|a| matches(a, w))
        {
            return Some((a["emailAddress"]["name"].as_str().unwrap_or(w).to_string(), a["emailAddress"]["address"].as_str()?.to_string()));
        }
    }
    let first = words(w).into_iter().next()?;
    let v = crate::mail::call("mail_search", &json!({ "query": w, "count": 10 })).or_else(|_| crate::mail::call("mail_search", &json!({ "query": first, "count": 10 }))).ok()?;
    v["messages"].as_array()?.iter().find_map(|m| {
        let from = m["from"].as_str()?;
        // "Name <address>"
        let (name, addr) = from.rsplit_once('<').map(|(n, a)| (n.trim().to_string(), a.trim_end_matches('>').trim().to_string()))?;
        let fake = json!({ "emailAddress": { "name": name, "address": addr } });
        matches(&fake, w).then_some((name, addr))
    })
}

pub fn call(name: &str, args: &Value, mem: Option<&crate::mem::Mem>) -> Result<Value, String> {
    if name != "who_is" {
        return Err(format!("{name} isn't a people tool"));
    }
    let who = args["who"].as_str().unwrap_or("").trim().to_string();
    if who.len() < 2 {
        return Err("who? a name or an email address".into());
    }
    let user = crate::acting::current();
    let mut out = json!({ "who": who });
    // Their own memories about the person (only theirs: the owner's or their own scope).
    if let Some(m) = mem {
        let scope = (!crate::acting::is_owner(&user)).then(|| format!("user:{user}"));
        if let Ok(found) = m.recall(scope.as_deref(), &who, 6, false) {
            let w = who.to_lowercase();
            let about: Vec<String> = found.into_iter().map(|r| r.memory.content).filter(|c| c.to_lowercase().contains(&w) || w.split_whitespace().any(|p| p.len() > 2 && c.to_lowercase().contains(p))).take(5).collect();
            if !about.is_empty() {
                out["remembered"] = json!(about);
            }
        }
    }
    if crate::mail::connected_for(&user) {
        // The whole name, else its first word ("Juan Dell" → Juan).
        let first = words(&who).into_iter().next().unwrap_or_else(|| who.clone());
        let queries = if who.contains('@') { vec![format!("from:{who}")] } else { vec![who.clone(), first] };
        for q in queries {
            if let Ok(v) = crate::mail::call("mail_search", &json!({ "query": q, "count": 5 }))
                && let Some(found) = v["messages"].as_array().filter(|m| !m.is_empty())
            {
                out["recent_mail"] = json!(found.iter().map(|m| json!({ "from": m["from"], "subject": m["subject"], "received": m["received"] })).collect::<Vec<_>>());
                break;
            }
        }
    }
    if crate::graph::connected_for(&user) {
        let now = Utc::now();
        if let Ok(events) = crate::calendar::events(now - Duration::days(21), now + Duration::days(21)) {
            let with: Vec<&Value> = events.iter().filter(|e| with_them(e, &who)).collect();
            let (past, next): (Vec<&Value>, Vec<&Value>) = with.into_iter().partition(|e| crate::graph::utc(&e["start"]).is_some_and(|s| s < now));
            let show = |e: &&Value| json!({ "title": e["subject"], "when": crate::graph::utc(&e["start"]).map(|t| t.with_timezone(&chrono::Local).format("%a %b %-d %H:%M").to_string()) });
            out["last_meetings"] = json!(past.iter().rev().take(3).map(show).collect::<Vec<_>>());
            out["next_meetings"] = json!(next.iter().take(3).map(show).collect::<Vec<_>>());
            if let Some(a) = events.iter().flat_map(|e| e["attendees"].as_array().into_iter().flatten()).find(|a| matches(a, &who)) {
                out["email"] = a["emailAddress"]["address"].clone();
                out["name"] = a["emailAddress"]["name"].clone();
            }
        }
    }
    if crate::pmi::configured_for(&user) {
        if let Ok(v) = crate::pmi::call("pmi_search", &json!({ "query": who }))
            && let Some(p) = v["people"].as_array().and_then(|p| p.first())
        {
            out["in_pmi"] = json!({ "name": p["name"], "username": p["username"] });
        }
        if let Ok(v) = crate::pmi::call("pmi_tasks", &json!({})) {
            let w = who.to_lowercase();
            let shared: Vec<Value> = v["tasks"].as_array().into_iter().flatten().filter(|t| t["assignees"].as_array().is_some_and(|a| a.iter().any(|n| n.as_str().is_some_and(|n| n.to_lowercase().contains(&w))))).map(|t| json!({ "title": t["title"], "due": t["due"], "where": t["where"] })).take(6).collect();
            if !shared.is_empty() {
                out["shared_tasks"] = json!(shared);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_match_by_name_or_address() {
        let a = json!({ "emailAddress": { "name": "Dana Doe", "address": "dana@fbcad.org" } });
        assert!(matches(&a, "dana") && matches(&a, "Dana Doe") && matches(&a, "DANA@fbcad.org") && matches(&a, "doe dana"));
        assert!(matches(&a, "Dana from Fort Bend"), "any word of the name");
        assert!(!matches(&a, "jeremy") && !matches(&a, "") && !matches(&a, "Dan"), "whole words only");
        let e = json!({ "subject": "Dell & FBC Quarterly Sync: Juan x Garrett", "attendees": [] });
        assert!(with_them(&e, "Juan Dell") && !with_them(&e, "Jeremy"), "a meeting named for them");
    }
}
