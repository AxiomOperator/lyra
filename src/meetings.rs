//! Meeting follow-up: after a Teams meeting, its transcript (when the meeting
//! was transcribed and the person may read it: OnlineMeetings.Read and
//! OnlineMeetingTranscript.Read.All, `[web.entra] meetings = true`), who was
//! there, and what to do with it. The chat model writes the summary and action
//! items; making the person's own PMI tasks and drafting the follow-up mail
//! go through the usual tools (sending still waits for their yes).

use chrono::{DateTime, Duration, Local, Utc};
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde_json::{Value, json};

use crate::calendar::{graph, utc};

/// The person can read their meetings' transcripts.
pub fn ready(user: &str) -> bool {
    crate::calendar::connected_for(user) && crate::calendar::has(user, "OnlineMeetingTranscript.Read.All")
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
    let url = format!("https://graph.microsoft.com/v1.0/me/onlineMeetings/{}/transcripts/{}/content?$format=text/vtt", lyra_web::oidc::encode(&id), lyra_web::oidc::encode(&tid));
    let bytes = crate::calendar::graph_bytes(&url)?;
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
    if !crate::calendar::connected_for(&user) {
        return Err("your Outlook isn't connected: in the app, More → Outlook → Connect".into());
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
    fn transcripts_read_as_speaker_lines() {
        let vtt = "WEBVTT\n\n0f1c2d3e-1\n00:00:01.000 --> 00:00:04.000\n<v Dana Doe>Let's look at the firewall.</v>\n\n00:00:04.000 --> 00:00:06.000\n<v Dana Doe>Phase two next week.</v>\n\n00:00:06.000 --> 00:00:09.000\n<v Garrett Post>I'll order the switches.</v>\n";
        assert_eq!(vtt_text(vtt), "Dana Doe: Let's look at the firewall. Phase two next week.\nGarrett Post: I'll order the switches.");
    }

    #[test]
    fn follow_ups_are_offered_shortly_after_a_teams_meeting() {
        let now = Utc::now();
        let ev = |ago: i64, online: bool| {
            let end = now - Duration::minutes(ago);
            let mut e = json!({ "end": crate::calendar::graph_time(end), "subject": "Sync" });
            if online {
                e["onlineMeeting"] = json!({ "joinUrl": "https://teams.microsoft.com/l/meetup-join/x" });
            }
            e
        };
        let list = vec![ev(5, true), ev(30, true), ev(30, false), ev(200, true)];
        assert_eq!(just_ended(&list, now).len(), 1, "only the Teams one that ended half an hour ago");
    }
}
