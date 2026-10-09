//! Each person's Outlook calendar (Microsoft Graph), connected from the app
//! with lyra's Microsoft sign-in (`[web.entra]`): their refresh token is kept
//! in their own secrets (`[graph] token`). Reading is free; a personal event
//! (nobody else on it) is made, moved or removed at once; anything others see
//! (invites, answers, changes to meetings with attendees) waits for that
//! person's yes.


use chrono::{DateTime, Datelike, Duration as Span, Local, NaiveTime, TimeZone, Timelike, Utc};
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde_json::{Value, json};

use crate::caps::Ask;
use crate::graph::{connected_for, disconnect, graph, graph_time, utc};


fn get(path: &str) -> Result<Value, String> {
    graph(reqwest::Method::GET, path, None)
}

// ---- times

fn local(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%a %b %-d %H:%M").to_string()
}

/// A day (or the week) as a span: "today", "tomorrow", "week", or a date said in words.
fn span(when: &str) -> Result<(DateTime<Utc>, DateTime<Utc>), String> {
    let now = Local::now();
    let start_of = |d: chrono::NaiveDate| Local.from_local_datetime(&d.and_time(NaiveTime::MIN)).earliest().map(|t| t.with_timezone(&Utc));
    let w = when.trim().to_lowercase();
    let (from, days) = match w.as_str() {
        "" | "today" => (now.date_naive(), 1),
        "week" | "this week" | "next 7 days" => (now.date_naive(), 7),
        _ => (crate::when::parse(&w, now).ok_or_else(|| format!("can't read the day {when:?}: try today, tomorrow, friday, oct 14, week"))?.date, 1),
    };
    let start = start_of(from).ok_or("that day doesn't exist here")?;
    Ok((start, start + Span::days(days)))
}

/// An event in a few fields.
fn brief(e: &Value) -> Value {
    let attendees: Vec<String> = e["attendees"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| a["emailAddress"]["name"].as_str().filter(|n| !n.is_empty()).or(a["emailAddress"]["address"].as_str()).unwrap_or("?").to_string())
        .collect();
    let mut v = json!({
        "id": e["id"],
        "title": e["subject"],
        "start": utc(&e["start"]).map(local),
        "end": utc(&e["end"]).map(local),
    });
    if e["isAllDay"] == true {
        v["all_day"] = json!(true);
    }
    if let Some(l) = e["location"]["displayName"].as_str().filter(|l| !l.is_empty()) {
        v["where"] = json!(l);
    }
    if !attendees.is_empty() {
        v["with"] = json!(attendees);
    }
    if e["isOrganizer"] == false {
        v["organizer"] = e["organizer"]["emailAddress"]["name"].clone();
        v["your_answer"] = e["responseStatus"]["response"].clone();
    }
    if e["isCancelled"] == true {
        v["cancelled"] = json!(true);
    }
    if e["onlineMeeting"]["joinUrl"].is_string() {
        v["online"] = json!(true);
    }
    v
}

const SELECT: &str = "subject,start,end,location,attendees,organizer,isOrganizer,isAllDay,isCancelled,responseStatus,showAs,onlineMeeting,categories,bodyPreview";

/// The person's events between two times, soonest first.
pub fn events(from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Value>, String> {
    let path = format!(
        "/me/calendarView?startDateTime={}&endDateTime={}&$select={SELECT}&$orderby=start/dateTime&$top=100",
        lyra_web::oidc::encode(&from.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
        lyra_web::oidc::encode(&to.format("%Y-%m-%dT%H:%M:%SZ").to_string())
    );
    // Every page of them (Outlook sends 100 at a time): a busy calendar's
    // later meetings mustn't look like free time (I-9).
    let mut list = Vec::new();
    let mut next = Some(path);
    for _ in 0..20 {
        let Some(page) = next.take() else { break };
        let v = get(&page)?;
        list.extend(v["value"].as_array().cloned().unwrap_or_default());
        next = v["@odata.nextLink"].as_str().map(str::to_string);
    }
    // Outlook also returns events that end exactly when the span starts.
    Ok(list.into_iter().filter(|e| utc(&e["end"]).is_none_or(|end| end > from)).collect())
}

/// Free stretches of at least `minutes` between `from` and `to` (local hours), on a day.
fn free(events: &[Value], day: (DateTime<Utc>, DateTime<Utc>), hours: (NaiveTime, NaiveTime), minutes: i64) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    let date = day.0.with_timezone(&Local).date_naive();
    let at = |t: NaiveTime| Local.from_local_datetime(&date.and_time(t)).earliest().map(|x| x.with_timezone(&Utc));
    let (Some(mut cursor), Some(end)) = (at(hours.0), at(hours.1)) else { return vec![] };
    let mut busy: Vec<(DateTime<Utc>, DateTime<Utc>)> = events
        .iter()
        .filter(|e| e["isCancelled"] != true && e["showAs"] != "free" && e["isAllDay"] != true)
        .filter_map(|e| Some((utc(&e["start"])?, utc(&e["end"])?)))
        .collect();
    busy.sort();
    let mut out = Vec::new();
    for (s, e) in busy {
        if s > cursor && s.min(end) - cursor >= Span::minutes(minutes) {
            out.push((cursor, s.min(end)));
        }
        cursor = cursor.max(e);
        if cursor >= end {
            break;
        }
    }
    if end - cursor >= Span::minutes(minutes) {
        out.push((cursor, end));
    }
    out
}

/// The days to look in: the next 5 working days, or what `within` says.
fn days_within(within: &str) -> Vec<chrono::NaiveDate> {
    let s = crate::planner::settings();
    let today = Local::now().date_naive();
    let w = within.trim().to_lowercase();
    let work = |from: chrono::NaiveDate, n: usize| -> Vec<chrono::NaiveDate> { from.iter_days().take(21).filter(|d| s.workday(*d)).take(n).collect() };
    match w.as_str() {
        "" | "soon" | "this week or next" => work(today, 5),
        "this week" => {
            let monday = today - Span::days(today.weekday().num_days_from_monday() as i64);
            (0..7).map(|i| monday + Span::days(i)).filter(|d| *d >= today && s.workday(*d)).collect()
        }
        "next week" => {
            let monday = today - Span::days(today.weekday().num_days_from_monday() as i64) + Span::days(7);
            (0..7).map(|i| monday + Span::days(i)).filter(|d| s.workday(*d)).collect()
        }
        _ => match span(&w) {
            // At most three weeks, whatever was asked.
            Ok((from, to)) => from.with_timezone(&Local).date_naive().iter_days().take(21).take_while(|d| *d < to.with_timezone(&Local).date_naive().max(from.with_timezone(&Local).date_naive() + Span::days(1))).filter(|d| s.workday(*d)).collect(),
            Err(_) => work(today, 5),
        },
    }
}

/// Times everyone is free: the user's calendar and the others' free/busy.
fn find_time(args: &Value) -> Result<Value, String> {
    let minutes = args["minutes"].as_i64().unwrap_or(30).clamp(10, 8 * 60);
    let count = args["count"].as_u64().unwrap_or(5).clamp(1, 10) as usize;
    let mut people = Vec::new();
    let mut unknown = Vec::new();
    for who in args["attendees"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        match crate::people::email_of(who) {
            Some(p) => people.push(p),
            None => unknown.push(who.to_string()),
        }
    }
    if people.is_empty() {
        return Err(format!("couldn't find an email address for {}: give it (name@fbcad.org)", unknown.join(", ")));
    }
    let days = days_within(args["within"].as_str().unwrap_or(""));
    let (Some(first), Some(last)) = (days.first(), days.last()) else { return Err("no working days in that span".into()) };
    let s = crate::planner::settings();
    let (Some(((from, _), _)), Some(((_, to), _))) = (s.hours(*first), s.hours(*last)) else { return Err("couldn't work out the working day".into()) };
    // Their busy times, as events with start and end.
    let body = json!({
        "schedules": people.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>(),
        "startTime": graph_time(from),
        "endTime": graph_time(to),
        "availabilityViewInterval": 30,
    });
    let v = graph(reqwest::Method::POST, "/me/calendar/getSchedule", Some(&body))?;
    let mut busy: Vec<Value> = events(from, to)?;
    let mut hidden = Vec::new();
    for sched in v["value"].as_array().into_iter().flatten() {
        if sched["error"].is_object() {
            hidden.push(sched["scheduleId"].as_str().unwrap_or("?").to_string());
            continue;
        }
        for item in sched["scheduleItems"].as_array().into_iter().flatten().filter(|i| i["status"] != "free") {
            busy.push(json!({ "start": item["start"], "end": item["end"], "showAs": "busy" }));
        }
    }
    // Free stretches each day, minus lunch; at most two suggestions a day, spread out.
    let mut slots = Vec::new();
    let now = Utc::now() + Span::minutes(30);
    for d in &days {
        let Some(((ds, de), (ls, le))) = s.hours(*d) else { continue };
        let mut day_busy = busy.clone();
        day_busy.push(json!({ "start": graph_time(ls), "end": graph_time(le), "showAs": "busy" }));
        let (open, close) = (ds.with_timezone(&Local).time(), de.with_timezone(&Local).time());
        let mut today_count = 0;
        for (gs, ge) in free(&day_busy, (ds, de), (open, close), minutes) {
            // On the hour or half hour, and not in the past.
            let mut start = gs.max(now);
            let m = start.with_timezone(&Local).minute();
            if m % 30 != 0 {
                start += Span::minutes((30 - (m % 30)) as i64);
                start -= Span::seconds(start.timestamp() % 60);
            }
            if start + Span::minutes(minutes) <= ge && today_count < 2 {
                slots.push(start);
                today_count += 1;
            }
        }
    }
    let shown: Vec<Value> = slots
        .iter()
        .take(count)
        .map(|t| {
            let l = t.with_timezone(&Local);
            json!({ "when": l.format("%a %b %-d %H:%M").to_string(), "for_cal_create": l.format("%b %-d %H:%M").to_string().to_lowercase(), "until": (l + Span::minutes(minutes)).format("%H:%M").to_string() })
        })
        .collect();
    Ok(json!({
        "with": people.iter().map(|(n, e)| json!({ "name": n, "email": e })).collect::<Vec<_>>(),
        "not_found": unknown,
        "calendar_hidden": hidden,
        "minutes": minutes,
        "slots": shown,
        "next": "offer these; once the user picks one, cal_create with the attendees' emails and `when` = for_cal_create (they approve the invite)",
    }))
}

// ---- tools

pub fn capabilities() -> Vec<Capability> {
    let tool = |name: &str, description: &str, risk: RiskLevel, properties: Value, required: &[&str]| {
        let mut c = Capability::new(name, CapabilityKind::NativeTool, description, risk);
        c.input_schema = json!({ "type": "object", "properties": properties, "required": required });
        c.source = "calendar".into();
        c.tags = ["calendar", "meeting", "meetings", "event", "schedule", "outlook", "agenda", "free", "busy", "invite"].iter().map(|t| t.to_string()).collect();
        c
    };
    let id = json!({ "type": "string", "description": "The event's id (from cal_agenda)." });
    vec![
        tool(
            "cal_agenda",
            "The user's Outlook calendar for a day or the week: meetings with times, places, who's on them, and invites not answered yet.",
            RiskLevel::ReadOnly,
            json!({ "when": { "type": "string", "description": "today (default), tomorrow, a weekday, a date (oct 14), or week." } }),
            &[],
        ),
        tool(
            "cal_free",
            "When the user is free on a day: free stretches of at least `minutes` within working hours.",
            RiskLevel::ReadOnly,
            json!({
                "when": { "type": "string", "description": "The day: today, tomorrow, thursday, oct 14." },
                "minutes": { "type": "integer", "description": "How long (default 30)." },
                "from": { "type": "string", "description": "Day starts (default 08:00)." },
                "to": { "type": "string", "description": "Day ends (default 17:00)." },
            }),
            &[],
        ),
        tool(
            "cal_find_time",
            "Find times that suit the user and other people: checks everyone's Outlook free/busy within working hours (lunch kept free) and suggests a few slots. Then cal_create sends the invite (the user approves first).",
            RiskLevel::ReadOnly,
            json!({
                "attendees": { "type": "array", "items": { "type": "string" }, "description": "Names or email addresses." },
                "minutes": { "type": "integer", "description": "How long (default 30)." },
                "within": { "type": "string", "description": "When: \"this week\", \"next week\", \"tomorrow\", a day (\"thursday\"); default the next 5 working days." },
                "count": { "type": "integer", "description": "How many slots (default 5)." },
            }),
            &["attendees"],
        ),
        tool("cal_invites", "Meeting invites the user hasn't answered yet (the next two weeks).", RiskLevel::ReadOnly, json!({}), &[]),
        tool(
            "cal_create",
            "Put an event on the user's calendar. With attendees it sends invites (the user approves first); without, it's just theirs.",
            RiskLevel::LowWrite,
            json!({
                "title": { "type": "string" },
                "when": { "type": "string", "description": "When it starts, as said: \"friday 3pm\", \"tomorrow at 10\", \"oct 14 9:30\"." },
                "minutes": { "type": "integer", "description": "How long (default 30)." },
                "attendees": { "type": "array", "items": { "type": "string" }, "description": "Email addresses to invite." },
                "location": { "type": "string" },
                "notes": { "type": "string" },
                "online": { "type": "boolean", "description": "Add a Teams meeting link." },
            }),
            &["title", "when"],
        ),
        tool(
            "cal_update",
            "Change an event: its title, start time, length or place (meetings with others: the user approves first).",
            RiskLevel::LowWrite,
            json!({ "id": id, "title": { "type": "string" }, "when": { "type": "string" }, "minutes": { "type": "integer" }, "location": { "type": "string" } }),
            &["id"],
        ),
        tool(
            "cal_cancel",
            "Remove an event; one the user organized with others is cancelled for everyone with a note (the user approves first).",
            RiskLevel::LowWrite,
            json!({ "id": id, "comment": { "type": "string" } }),
            &["id"],
        ),
        tool(
            "plan_my_day",
            "Plan the user's day now: private focus blocks on their calendar for PMI tasks due soon (overdue first), around meetings, within working hours, lunch kept free; blocks a meeting landed on move, ones whose task is done go. Returns what changed and the day.",
            RiskLevel::LowWrite,
            json!({}),
            &[],
        ),
        tool(
            "cal_respond",
            "Answer a meeting invite: accept, tentative or decline, with an optional note to the organizer (the user approves first).",
            RiskLevel::LowWrite,
            json!({ "id": id, "answer": { "type": "string", "enum": ["accept", "tentative", "decline"] }, "comment": { "type": "string" } }),
            &["id", "answer"],
        ),
    ]
}

fn attendees_of(id: &str) -> Result<(Vec<String>, bool), String> {
    let e = get(&format!("/me/events/{}?$select=attendees,isOrganizer,subject", lyra_web::oidc::encode(id)))?;
    let people = e["attendees"].as_array().into_iter().flatten().filter_map(|a| a["emailAddress"]["address"].as_str().map(str::to_string)).collect();
    Ok((people, e["isOrganizer"] != false))
}

/// What needs the person's yes: anything other people will see.
pub fn approval(name: &str, args: &Value) -> Option<Ask> {
    let title = args["title"].as_str().unwrap_or("");
    let id = args["id"].as_str().unwrap_or("");
    let ask = |what: String, detail: String| Some(Ask { what, detail, why: "other people will see it".into(), dangerous: false });
    match name {
        "cal_create" => {
            let people: Vec<String> = args["attendees"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(str::to_string)).collect();
            (!people.is_empty()).then(|| ask(format!("send a meeting invite: {title}"), format!("{} · with {}", args["when"].as_str().unwrap_or("?"), people.join(", "))))?
        }
        "cal_update" | "cal_cancel" => match attendees_of(id) {
            Ok((people, _)) if !people.is_empty() => ask(
                format!("{} a meeting with {}", if name == "cal_cancel" { "cancel" } else { "change" }, people.join(", ")),
                args.to_string().chars().take(300).collect(),
            ),
            _ => None,
        },
        "cal_respond" => ask(format!("{} a meeting invite", args["answer"].as_str().unwrap_or("answer")), args["comment"].as_str().unwrap_or("").to_string()),
        _ => None,
    }
}

/// Run a calendar tool as the person this thread works for (approval asked first by the caller).
pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "plan_my_day" => {
            let did = crate::planner::run()?;
            Ok(json!({ "changed": did, "day": crate::planner::today()? }))
        }
        "cal_agenda" => {
            let when = args["when"].as_str().unwrap_or("today");
            let (from, to) = span(when)?;
            let list = events(from, to)?;
            Ok(json!({ "from": local(from), "events": list.iter().map(brief).collect::<Vec<_>>() }))
        }
        "cal_free" => {
            let day = span(args["when"].as_str().unwrap_or("today"))?;
            let minutes = args["minutes"].as_i64().unwrap_or(30).clamp(5, 600);
            let hour = |k: &str, d: &str| crate::routines::time_of_day(args[k].as_str().unwrap_or(d)).unwrap_or_else(|| crate::routines::time_of_day(d).unwrap_or_default());
            let list = events(day.0, day.1)?;
            let gaps = free(&list, day, (hour("from", "08:00"), hour("to", "17:00")), minutes);
            Ok(json!({ "free": gaps.iter().map(|(s, e)| format!("{} – {}", local(*s), e.with_timezone(&Local).format("%H:%M"))).collect::<Vec<_>>() }))
        }
        "cal_find_time" => find_time(args),
        "cal_invites" => {
            let now = Utc::now();
            let list = events(now, now + Span::days(14))?;
            Ok(json!({ "invites": list.iter().filter(|e| e["isOrganizer"] == false && e["isCancelled"] != true && e["responseStatus"]["response"] == "notResponded").map(brief).collect::<Vec<_>>() }))
        }
        "cal_create" => {
            let start = crate::when::parse(args["when"].as_str().unwrap_or(""), Local::now()).ok_or("can't read when it starts: try \"friday 3pm\"")?;
            let start = start.at().ok_or("that time doesn't exist here")?.with_timezone(&Utc);
            let minutes = args["minutes"].as_i64().unwrap_or(30).clamp(5, 24 * 60);
            let people: Vec<&str> = args["attendees"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            if let Some(bad) = people.iter().find(|p| !p.contains('@')) {
                return Err(format!("{bad:?} isn't an email address: invites need one"));
            }
            let mut body = json!({
                "subject": args["title"].as_str().unwrap_or("").trim(),
                "start": graph_time(start),
                "end": graph_time(start + Span::minutes(minutes)),
                "attendees": people.iter().map(|p| json!({ "emailAddress": { "address": p }, "type": "required" })).collect::<Vec<_>>(),
            });
            if let Some(l) = args["location"].as_str().filter(|l| !l.is_empty()) {
                body["location"] = json!({ "displayName": l });
            }
            if let Some(n) = args["notes"].as_str().filter(|n| !n.is_empty()) {
                body["body"] = json!({ "contentType": "text", "content": n });
            }
            if args["online"] == true {
                body["isOnlineMeeting"] = json!(true);
                body["onlineMeetingProvider"] = json!("teamsForBusiness");
            }
            let e = graph(reqwest::Method::POST, "/me/events", Some(&body))?;
            Ok(brief(&e))
        }
        "cal_update" => {
            let id = args["id"].as_str().unwrap_or("");
            let mut body = json!({});
            if let Some(t) = args["title"].as_str().filter(|t| !t.is_empty()) {
                body["subject"] = json!(t);
            }
            if let Some(l) = args["location"].as_str().filter(|l| !l.is_empty()) {
                body["location"] = json!({ "displayName": l });
            }
            if let Some(w) = args["when"].as_str().filter(|w| !w.is_empty()) {
                let start = crate::when::parse(w, Local::now()).and_then(|w| w.at()).ok_or("can't read the new time")?.with_timezone(&Utc);
                let minutes = match args["minutes"].as_i64() {
                    Some(m) => m,
                    None => {
                        let e = get(&format!("/me/events/{}?$select=start,end", lyra_web::oidc::encode(id)))?;
                        utc(&e["start"]).zip(utc(&e["end"])).map_or(30, |(s, e)| (e - s).num_minutes())
                    }
                };
                body["start"] = graph_time(start);
                body["end"] = graph_time(start + Span::minutes(minutes.clamp(5, 24 * 60)));
            }
            if body.as_object().is_some_and(|m| m.is_empty()) {
                return Err("nothing to change".into());
            }
            Ok(brief(&graph(reqwest::Method::PATCH, &format!("/me/events/{}", lyra_web::oidc::encode(id)), Some(&body))?))
        }
        "cal_cancel" => {
            let id = args["id"].as_str().unwrap_or("");
            let (people, organizer) = attendees_of(id)?;
            let path = format!("/me/events/{}", lyra_web::oidc::encode(id));
            if organizer && !people.is_empty() {
                graph(reqwest::Method::POST, &format!("{path}/cancel"), Some(&json!({ "comment": args["comment"].as_str().unwrap_or("") })))?;
                Ok(json!({ "cancelled": id, "told": people }))
            } else {
                graph(reqwest::Method::DELETE, &path, None)?;
                Ok(json!({ "removed": id }))
            }
        }
        "cal_respond" => {
            let id = args["id"].as_str().unwrap_or("");
            let verb = match args["answer"].as_str().unwrap_or("") {
                "accept" => "accept",
                "tentative" => "tentativelyAccept",
                "decline" => "decline",
                other => return Err(format!("answer accept, tentative or decline, not {other:?}")),
            };
            graph(reqwest::Method::POST, &format!("/me/events/{}/{verb}", lyra_web::oidc::encode(id)), Some(&json!({ "comment": args["comment"].as_str().unwrap_or(""), "sendResponse": true })))?;
            Ok(json!({ "answered": args["answer"], "id": id }))
        }
        other => Err(format!("{other} isn't a calendar tool")),
    }
}

// ---- the briefing, the app and the terminal

/// Today at a glance: events, clashes and invites waiting (for the briefing and the app).
pub fn today() -> Result<Value, String> {
    let now = Utc::now();
    let (from, to) = span("today")?;
    let list = events(from, to)?;
    let mut sorted: Vec<(DateTime<Utc>, DateTime<Utc>, &Value)> =
        list.iter().filter(|e| e["isCancelled"] != true && e["isAllDay"] != true && e["showAs"] != "free").filter_map(|e| Some((utc(&e["start"])?, utc(&e["end"])?, e))).collect();
    sorted.sort_by_key(|x| x.0);
    let clashes: Vec<String> = sorted.windows(2).filter(|w| w[1].0 < w[0].1).map(|w| format!("{} overlaps {}", w[0].2["subject"].as_str().unwrap_or("?"), w[1].2["subject"].as_str().unwrap_or("?"))).collect();
    let later = events(now, now + Span::days(14))?;
    let invites: Vec<Value> = later.iter().filter(|e| e["isOrganizer"] == false && e["isCancelled"] != true && e["responseStatus"]["response"] == "notResponded").map(brief).collect();
    Ok(json!({ "events": list.iter().filter(|e| e["isCancelled"] != true).map(brief).collect::<Vec<_>>(), "clashes": clashes, "invites": invites }))
}

/// `/calendar [today|tomorrow|week|<day>|disconnect]`.
pub fn command(arg: &str, user: &str) -> Result<String, String> {
    let a = arg.trim();
    if a == "disconnect" {
        disconnect(user)?;
        return Ok("your Outlook calendar is disconnected (connect it again from More in the app)".into());
    }
    if !connected_for(user) {
        return Err("your Outlook calendar isn't connected: in the app, More → Connect Outlook calendar".into());
    }
    let v = crate::acting::run(user, || call("cal_agenda", &json!({ "when": if a.is_empty() { "today" } else { a } })))?;
    let list = v["events"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return Ok(format!("nothing on the calendar ({})", if a.is_empty() { "today" } else { a }));
    }
    let lines: Vec<String> = list
        .iter()
        .map(|e| {
            let mut l = format!("{} – {}  {}", e["start"].as_str().unwrap_or("?"), e["end"].as_str().map(|t| t.rsplit(' ').next().unwrap_or(t)).unwrap_or("?"), e["title"].as_str().unwrap_or(""));
            if let Some(w) = e["where"].as_str() {
                l += &format!(" · {w}");
            }
            if e["your_answer"] == "notResponded" {
                l += " · not answered";
            }
            l
        })
        .collect();
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(title: &str, from: (u32, u32), to: (u32, u32)) -> Value {
        let day = Local::now().date_naive();
        let at = |(h, m): (u32, u32)| Local.from_local_datetime(&day.and_hms_opt(h, m, 0).unwrap()).earliest().unwrap().with_timezone(&Utc);
        json!({ "subject": title, "start": graph_time(at(from)), "end": graph_time(at(to)), "showAs": "busy" })
    }

    #[test]
    fn free_time_is_the_gaps_between_meetings() {
        let day = span("today").unwrap();
        let list = vec![ev("standup", (9, 0), (9, 15)), ev("review", (10, 0), (11, 30)), ev("lunch", (12, 0), (12, 45))];
        let hours = (NaiveTime::from_hms_opt(8, 0, 0).unwrap(), NaiveTime::from_hms_opt(17, 0, 0).unwrap());
        let gaps: Vec<String> = free(&list, day, hours, 30).iter().map(|(s, e)| format!("{}-{}", s.with_timezone(&Local).format("%H:%M"), e.with_timezone(&Local).format("%H:%M"))).collect();
        assert_eq!(gaps, ["08:00-09:00", "09:15-10:00", "11:30-12:00", "12:45-17:00"], "a gap of exactly 30 minutes counts");
        assert_eq!(free(&list, day, hours, 45).len(), 3, "a shorter one doesn't");
    }

    #[test]
    fn meeting_times_are_looked_for_on_working_days() {
        let today = Local::now().date_naive();
        let next = days_within("next week");
        assert_eq!(next.len(), 5, "{next:?}");
        assert!(next.iter().all(|d| *d > today && !matches!(d.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun)));
        let soon = days_within("");
        assert_eq!(soon.len(), 5);
        assert!(soon[0] >= today);
        assert!(days_within("this week").iter().all(|d| *d >= today));
        let tomorrow = days_within("tomorrow");
        assert!(tomorrow.len() <= 1 && tomorrow.iter().all(|d| *d == today + Span::days(1)), "{tomorrow:?}");
    }

    #[test]
    fn graph_times_read_back() {
        let t = Utc.with_ymd_and_hms(2026, 10, 9, 15, 0, 0).unwrap();
        assert_eq!(utc(&graph_time(t)), Some(t));
        assert_eq!(utc(&json!({ "dateTime": "2026-10-09T15:00:00.0000000", "timeZone": "UTC" })), Some(t), "Graph's seven decimals");
    }

    #[test]
    fn only_what_others_see_asks() {
        assert!(approval("cal_create", &json!({ "title": "dentist", "when": "fri 3pm" })).is_none(), "just theirs");
        let a = approval("cal_create", &json!({ "title": "sync", "when": "fri 3pm", "attendees": ["dana@fbcad.org"] })).unwrap();
        assert!(a.what.contains("invite") && a.detail.contains("dana@fbcad.org"));
        assert!(approval("cal_respond", &json!({ "id": "x", "answer": "decline" })).is_some());
        assert!(approval("cal_agenda", &json!({})).is_none());
    }
}
