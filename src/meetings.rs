//! Meeting follow-up: after a Teams meeting, its transcript (when the meeting
//! was transcribed and the person may read it: OnlineMeetings.Read and
//! OnlineMeetingTranscript.Read.All, `[web.entra] meetings = true`), who was
//! there, and what to do with it. The chat model writes the summary and action
//! items; making the person's own PMI tasks and drafting the follow-up mail
//! go through the usual tools (sending still waits for their yes).

use chrono::{DateTime, Duration, Local, Utc};
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde_json::{Value, json};

use crate::graph::{graph, utc};

/// The person can read their meetings' transcripts.
pub fn ready(user: &str) -> bool {
    crate::graph::connected_for(user) && crate::graph::has(user, "OnlineMeetingTranscript.Read.All")
}

/// Teams meetings that ended in the last `days`, newest first.
fn recent(days: i64) -> Result<Vec<Value>, String> {
    let now = Utc::now();
    let mut list: Vec<Value> = crate::calendar::events(now - Duration::days(days), now)?
        .into_iter()
        .filter(|e| e["isCancelled"] != true && e["onlineMeeting"]["joinUrl"].is_string() && utc(&e["end"]).is_some_and(|t| t <= now))
        .collect();
    list.reverse();
    Ok(list)
}

/// The meeting asked about: its id, words of its title, or the latest one.
fn find(key: &str) -> Result<Value, String> {
    let list = recent(7)?;
    let k = key.trim().to_lowercase();
    let pick = if k.is_empty() || k == "last" || k == "latest" {
        list.first()
    } else {
        list.iter().find(|e| e["id"].as_str() == Some(key.trim())).or_else(|| {
            let words: Vec<&str> = k.split_whitespace().filter(|w| w.len() > 2).collect();
            list.iter().find(|e| e["subject"].as_str().is_some_and(|s| {
                let s = s.to_lowercase();
                !words.is_empty() && words.iter().all(|w| s.contains(w))
            }))
        })
    };
    pick.cloned().ok_or_else(|| if key.trim().is_empty() { "no Teams meeting ended in the last week".to_string() } else { format!("no Teams meeting matching {key:?} in the last week") })
}

/// A WebVTT transcript as "Speaker: words" lines (one per turn, repeats folded).
pub fn vtt_text(vtt: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in vtt.lines() {
        let l = line.trim();
        if l.is_empty() || l == "WEBVTT" || l.contains("-->") || l.chars().all(|c| c.is_ascii_hexdigit() || c == '-' || c == '/') {
            continue;
        }
        // <v Dana Doe>words</v>
        let (who, words) = match l.strip_prefix("<v ").and_then(|r| r.split_once('>')) {
            Some((who, rest)) => (who.trim().to_string(), rest.trim_end_matches("</v>").trim().to_string()),
            None => (String::new(), l.to_string()),
        };
        if words.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if !who.is_empty() && last.starts_with(&format!("{who}: ")) => {
                last.push(' ');
                last.push_str(&words);
            }
            _ => out.push(if who.is_empty() { words } else { format!("{who}: {words}") }),
        }
    }
    out.join("\n")
}

/// The meeting's transcript text, or why there isn't one.
fn transcript(e: &Value) -> Result<String, String> {
    let join = e["onlineMeeting"]["joinUrl"].as_str().ok_or("not a Teams meeting")?;
    let filter = format!("JoinWebUrl eq '{}'", join.replace('\'', "''"));
    let m = graph(reqwest::Method::GET, &format!("/me/onlineMeetings?$filter={}", lyra_web::oidc::encode(&filter)), None)?;
    let id = m["value"][0]["id"].as_str().ok_or("Teams doesn't show this meeting to you (only meetings you organized or joined in your organization)")?.to_string();
    let t = graph(reqwest::Method::GET, &format!("/me/onlineMeetings/{}/transcripts", lyra_web::oidc::encode(&id)), None)?;
    let tid = t["value"].as_array().and_then(|v| v.last()).and_then(|t| t["id"].as_str()).ok_or("this meeting wasn't transcribed (start transcription in Teams next time)")?.to_string();
    let bytes = crate::graph::graph_bytes(&format!("/me/onlineMeetings/{}/transcripts/{}/content?$format=text/vtt", lyra_web::oidc::encode(&id), lyra_web::oidc::encode(&tid)))?;
    Ok(vtt_text(&String::from_utf8_lossy(&bytes)))
}

fn when(t: Option<DateTime<Utc>>) -> String {
    t.map(|t| t.with_timezone(&Local).format("%a %b %-d %H:%M").to_string()).unwrap_or_default()
}

pub fn capabilities() -> Vec<Capability> {
    let mut c = Capability::new(
        "meeting_followup",
        CapabilityKind::NativeTool,
        "After a Teams meeting: its transcript, who was there and the organizer, to write the follow-up: a short summary, decisions, and action items with owners. Then offer the user's own items as PMI tasks (pmi_add_task) and a follow-up mail to the attendees (mail_draft; sending waits for their yes).",
        RiskLevel::ReadOnly,
    );
    c.input_schema = json!({ "type": "object", "properties": { "meeting": { "type": "string", "description": "Words of the meeting's title, or empty for the latest one that ended (the last week)." } }, "required": [] });
    c.source = "meetings".into();
    c.tags = ["meeting", "meetings", "transcript", "follow", "followup", "minutes", "notes", "action", "items", "teams", "recap"].iter().map(|t| t.to_string()).collect();
    vec![c]
}

pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    if name != "meeting_followup" {
        return Err(format!("{name} isn't a meetings tool"));
    }
    let user = crate::acting::current();
    if !crate::graph::connected_for(&user) {
        return Err("your Outlook isn't connected: in the app, Profile → Connections → Outlook → Connect".into());
    }
    let e = find(args["meeting"].as_str().unwrap_or(""))?;
    let people: Vec<Value> = e["attendees"].as_array().into_iter().flatten().map(|a| json!({ "name": a["emailAddress"]["name"], "email": a["emailAddress"]["address"], "response": a["status"]["response"] })).collect();
    let mut out = json!({
        "meeting": e["subject"],
        "id": e["id"],
        "when": when(utc(&e["start"])),
        "ended": when(utc(&e["end"])),
        "organizer": e["organizer"]["emailAddress"]["name"],
        "attendees": people,
        "agenda": e["bodyPreview"],
    });
    if !ready(&user) {
        out["transcript"] = Value::Null;
        out["note"] = json!("lyra can't read meeting transcripts yet (an admin turns on [web.entra] meetings, then connect Outlook again): follow up from the agenda and what the user says");
        return Ok(out);
    }
    match transcript(&e) {
        Ok(t) => {
            let cut = t.chars().count() > 40_000;
            out["transcript"] = json!(t.chars().take(40_000).collect::<String>());
            out["cut"] = json!(cut);
        }
        Err(why) => {
            out["transcript"] = Value::Null;
            out["note"] = json!(why);
        }
    }
    Ok(out)
}

// ---- the meeting workspace (the app's Meetings page)
//
// One page per meeting: before, the prep (who's coming, the last mail with
// them, open tasks); during, the person's notes (kept in their files); after,
// the follow-up (summary, decisions, their action items, a mail to the
// attendees) written from the transcript when there is one, else the notes.

/// What's kept for one meeting: the notes and the follow-up.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Kept {
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub followup: Option<Value>,
    #[serde(default)]
    pub updated: String,
}

fn kept_path(user: &str, id: &str) -> Option<std::path::PathBuf> {
    // Event ids are long and full of + / =: a hash names the file (FNV-1a,
    // the same on every build).
    let hash = id.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
    let name = format!("{hash:016x}");
    let dir = if user == lyra_web::users::OWNER { crate::config::home()?.join("meetings") } else { crate::context::user_dir(user)?.join("meetings") };
    Some(dir.join(format!("{name}.json")))
}

fn kept(user: &str, id: &str) -> Kept {
    kept_path(user, id).map(|p| crate::store::read_json(&p)).unwrap_or_default()
}

fn my_address(user: &str) -> String {
    lyra_web::Users::open(&crate::config::home().unwrap_or_default().join("web")).get(user).map(|u| u.email).unwrap_or_default()
}

fn attendees(e: &Value) -> Vec<Value> {
    e["attendees"].as_array().into_iter().flatten().map(|a| json!({ "name": a["emailAddress"]["name"], "email": a["emailAddress"]["address"], "response": a["status"]["response"] })).collect()
}

/// Their meetings from yesterday to three days ahead (not lyra's own focus blocks).
pub fn list() -> Result<Value, String> {
    let user = crate::acting::current();
    if !crate::graph::connected_for(&user) {
        return Ok(json!({ "connected": false }));
    }
    let now = Utc::now();
    let events = crate::calendar::events(now - Duration::days(1), now + Duration::days(4))?;
    let rows: Vec<Value> = events
        .iter()
        .filter(|e| e["isCancelled"] != true && !e["categories"].as_array().is_some_and(|c| c.iter().any(|x| x == "lyra")) && e["isAllDay"] != true)
        .map(|e| {
            let (start, end) = (utc(&e["start"]), utc(&e["end"]));
            let has = e["id"].as_str().map(|id| kept(&user, id)).unwrap_or_default();
            json!({
                "id": e["id"], "subject": e["subject"], "start": start, "end": end,
                "when": when(start), "people": e["attendees"].as_array().map_or(0, |a| a.len()),
                "teams": e["onlineMeeting"]["joinUrl"].is_string(),
                "state": match (start, end) { (_, Some(t)) if t <= now => "after", (Some(s), _) if s <= now => "during", _ => "before" },
                "notes": !has.notes.trim().is_empty(), "followup": has.followup.is_some(),
            })
        })
        .collect();
    Ok(json!({ "connected": true, "meetings": rows }))
}

fn event(id: &str) -> Result<Value, String> {
    if id.trim().is_empty() {
        return Err("which meeting?".into());
    }
    graph(reqwest::Method::GET, &format!("/me/events/{}?$select=subject,start,end,location,attendees,organizer,isOrganizer,onlineMeeting,bodyPreview,categories", lyra_web::oidc::encode(id)), None)
}

/// One meeting's page: the details, the prep, the notes and any follow-up.
pub fn workspace(id: &str) -> Result<Value, String> {
    let user = crate::acting::current();
    let e = event(id)?;
    let (start, end) = (utc(&e["start"]), utc(&e["end"]));
    let now = Utc::now();
    let (_, prep) = crate::proactive::prep(&e, &my_address(&user));
    let k = kept(&user, id);
    Ok(json!({
        "id": id, "subject": e["subject"], "when": when(start), "until": when(end),
        "state": match (start, end) { (_, Some(t)) if t <= now => "after", (Some(s), _) if s <= now => "during", _ => "before" },
        "location": e["location"]["displayName"], "join": e["onlineMeeting"]["joinUrl"],
        "organizer": e["organizer"]["emailAddress"]["name"], "agenda": e["bodyPreview"],
        "attendees": attendees(&e),
        "prep": prep.lines().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>(),
        "notes": k.notes, "followup": k.followup,
        "transcripts": ready(&user) && e["onlineMeeting"]["joinUrl"].is_string(),
    }))
}

/// Keep their notes for a meeting.
pub fn save_notes(id: &str, notes: &str) -> Result<(), String> {
    let user = crate::acting::current();
    if notes.chars().count() > 50_000 {
        return Err("those notes are too long (50,000 characters at most)".into());
    }
    let p = kept_path(&user, id).ok_or("no lyra home")?;
    crate::store::JsonStore::<Kept>::new(p).update(|k| {
        k.notes = notes.to_string();
        k.updated = Utc::now().to_rfc3339();
    })
}

const FOLLOWUP: &str = "You write the follow-up to a meeting for the person you work for. From what's given (the transcript when there is one, else their notes, the agenda and who was there), answer with JSON only: {\"summary\": \"3-5 sentences\", \"decisions\": [\"…\"], \"mine\": [{\"task\": \"what they must do, short\", \"due\": \"a day in words, or empty\"}], \"others\": [{\"who\": \"name\", \"task\": \"…\"}], \"email\": {\"subject\": \"…\", \"body\": \"a short follow-up to the attendees in their voice: thanks, the decisions, who does what (Markdown)\"}}. Only what was actually said or written; empty lists when there's nothing.";

/// Write the follow-up (the chat model), keep it, and return it.
pub fn follow_up(id: &str, url: &str, model: &str) -> Result<Value, String> {
    let user = crate::acting::current();
    let e = event(id)?;
    let k = kept(&user, id);
    let transcript = if ready(&user) && e["onlineMeeting"]["joinUrl"].is_string() { transcript(&e).ok() } else { None };
    if transcript.is_none() && k.notes.trim().is_empty() {
        return Err("there's nothing to follow up from yet: write a few notes (or turn on transcription in Teams next time)".into());
    }
    let people: Vec<String> = attendees(&e).iter().filter_map(|a| a["name"].as_str().map(str::to_string)).collect();
    let mut text = format!("Meeting: {}\nWhen: {}\nOrganizer: {}\nAttendees: {}\nAgenda: {}\n", e["subject"].as_str().unwrap_or(""), when(utc(&e["start"])), e["organizer"]["emailAddress"]["name"].as_str().unwrap_or(""), people.join(", "), e["bodyPreview"].as_str().unwrap_or(""));
    if !k.notes.trim().is_empty() {
        text += &format!("\nTheir notes:\n{}\n", k.notes.chars().take(20_000).collect::<String>());
    }
    if let Some(t) = &transcript {
        text += &format!("\nTranscript:\n{}\n", t.chars().take(40_000).collect::<String>());
    }
    let system = match crate::style::section(&user) {
        Some(st) => format!("{FOLLOWUP}\n\n{st}"),
        None => FOLLOWUP.to_string(),
    };
    let (reply, _) = crate::learn::complete(url, model, &system, &text)?;
    let mut f = parse_json(&reply).ok_or("the model's follow-up didn't come back as JSON: try again")?;
    f["from"] = json!(if transcript.is_some() { "transcript" } else { "notes" });
    f["made"] = json!(Utc::now().to_rfc3339());
    let p = kept_path(&user, id).ok_or("no lyra home")?;
    let out = f.clone();
    crate::store::JsonStore::<Kept>::new(p).update(move |k| {
        k.followup = Some(f);
        k.updated = Utc::now().to_rfc3339();
    })?;
    Ok(out)
}

/// The first JSON object in a model's answer (it may think aloud or fence it).
fn parse_json(reply: &str) -> Option<Value> {
    let r = reply.rsplit("</think>").next().unwrap_or(reply);
    let (a, b) = (r.find('{')?, r.rfind('}')?);
    serde_json::from_str(&r[a..=b]).ok()
}

/// The follow-up mail as a draft in their Outlook (to the attendees; nothing is sent).
pub fn draft_mail(id: &str) -> Result<Value, String> {
    let user = crate::acting::current();
    let e = event(id)?;
    let f = kept(&user, id).followup.ok_or("write the follow-up first")?;
    let me = my_address(&user).to_lowercase();
    let to: Vec<String> = attendees(&e).iter().filter_map(|a| a["email"].as_str().map(str::to_string)).filter(|a| a.to_lowercase() != me).collect();
    if to.is_empty() {
        return Err("nobody else was invited to this meeting".into());
    }
    let subject = f["email"]["subject"].as_str().map_or_else(|| format!("Follow-up: {}", e["subject"].as_str().unwrap_or("our meeting")), str::to_string);
    let body = f["email"]["body"].as_str().filter(|b| !b.trim().is_empty()).ok_or("the follow-up has no mail in it")?;
    crate::mail::call("mail_draft", &json!({ "to": to, "subject": subject, "body": body }))
}

/// Teams meetings that ended 10–90 minutes ago, for offering a follow-up once.
pub fn just_ended(events: &[Value], now: DateTime<Utc>) -> Vec<&Value> {
    events
        .iter()
        .filter(|e| e["isCancelled"] != true && e["onlineMeeting"]["joinUrl"].is_string() && e["isAllDay"] != true)
        .filter(|e| e["responseStatus"]["response"] != "declined")
        .filter(|e| utc(&e["end"]).is_some_and(|t| now - t >= Duration::minutes(10) && now - t <= Duration::minutes(90)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn followups_are_read_even_wrapped_and_files_are_named_stably() {
        let f = parse_json("<think>let me see</think> Sure: ```json\n{\"summary\": \"We agreed.\", \"mine\": [{\"task\": \"Send the PO\", \"due\": \"friday\"}]}\n```").unwrap();
        assert_eq!((f["summary"].as_str(), f["mine"][0]["due"].as_str()), (Some("We agreed."), Some("friday")));
        assert!(parse_json("no json here").is_none());
        let a = kept_path("owner", "AAMkAGI2+/x=").unwrap();
        assert_eq!(a, kept_path("owner", "AAMkAGI2+/x=").unwrap(), "the same id, the same file");
        assert_ne!(a, kept_path("owner", "AAMkAGI3+/x=").unwrap());
        assert!(a.file_name().unwrap().to_str().unwrap().chars().all(|c| c.is_ascii_hexdigit() || c == '.' || c.is_ascii_alphabetic()));
    }

    #[test]
    fn transcripts_read_as_speaker_lines() {
        let vtt = "WEBVTT\n\n0f1c2d3e-1\n00:00:01.000 --> 00:00:04.000\n<v Dana Doe>Let's look at the firewall.</v>\n\n00:00:04.000 --> 00:00:06.000\n<v Dana Doe>Phase two next week.</v>\n\n00:00:06.000 --> 00:00:09.000\n<v Garrett Post>I'll order the switches.</v>\n";
        assert_eq!(vtt_text(vtt), "Dana Doe: Let's look at the firewall. Phase two next week.\nGarrett Post: I'll order the switches.");
    }

    #[test]
    fn follow_ups_are_offered_shortly_after_a_teams_meeting() {
        let now = Utc::now();
        let ev = |ago: i64, online: bool| {
            let end = now - Duration::minutes(ago);
            let mut e = json!({ "end": crate::graph::graph_time(end), "subject": "Sync" });
            if online {
                e["onlineMeeting"] = json!({ "joinUrl": "https://teams.microsoft.com/l/meetup-join/x" });
            }
            e
        };
        let list = vec![ev(5, true), ev(30, true), ev(30, false), ev(200, true)];
        assert_eq!(just_ended(&list, now).len(), 1, "only the Teams one that ended half an hour ago");
    }
}
