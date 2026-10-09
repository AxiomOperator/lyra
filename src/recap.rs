//! The end-of-day recap (`[recap]`): at the end of each person's working day
//! (`[planner] day_end`), what today held and what's waiting: today's
//! meetings, tasks that slipped (due today or overdue, still open), mail
//! waiting on them and on others, and how tomorrow starts. Facts only, from
//! their own accounts; pushed to them, shown on the Status page and by `/recap`.

use std::path::PathBuf;
use std::sync::RwLock;

use chrono::{DateTime, Duration, Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// `[recap]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// Push it to the person's devices.
    pub notify: bool,
    /// When: empty for the end of their working day (`[planner] day_end`), or "17:00".
    pub at: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: true, notify: true, at: String::new() }
    }
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

pub fn configure(s: Settings) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

pub fn settings() -> Settings {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

/// One part of the recap.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Part {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Recap {
    pub at: DateTime<Utc>,
    pub parts: Vec<Part>,
}

fn path(user: &str) -> Option<PathBuf> {
    if user == lyra_web::users::OWNER { Some(crate::config::home()?.join("recap").join("last.json")) } else { Some(crate::context::user_dir(user)?.join("recap").join("last.json")) }
}

pub fn last_for(user: &str) -> Option<Recap> {
    crate::store::read_json::<Option<Recap>>(&path(user)?)
}

pub fn save_for(user: &str, r: &Recap) {
    if let Some(p) = path(user) {
        // Atomic: the Status page may be reading it (a failure is logged).
        let _ = crate::store::write_json(&p, r);
    }
}

/// When today's recap is due for this person, if today is a working day.
pub fn due_at(now: DateTime<Local>) -> Option<DateTime<Local>> {
    let p = crate::planner::settings();
    let d = now.date_naive();
    if !p.workday(d) {
        return None;
    }
    let s = settings();
    let t = crate::routines::time_of_day(if s.at.trim().is_empty() { &p.day_end } else { &s.at })?;
    use chrono::TimeZone;
    Local.from_local_datetime(&d.and_time(t)).earliest()
}

/// It's time, and today's hasn't been made (made at most 3 hours late).
pub fn due(user: &str, now: DateTime<Local>) -> bool {
    let Some(at) = due_at(now) else { return false };
    let made_today = last_for(user).is_some_and(|r| r.at.with_timezone(&Local).date_naive() == now.date_naive());
    now >= at && now - at < Duration::hours(3) && !made_today
}

fn time(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// Today's recap for the person this thread works for.
pub fn gather(now: DateTime<Utc>) -> Recap {
    let user = crate::acting::current();
    let mut parts = Vec::new();
    if crate::graph::connected_for(&user) {
        // Today's meetings.
        if let Ok(v) = crate::calendar::call("cal_agenda", &json!({ "when": "today" })) {
            let lines: Vec<String> = v["events"].as_array().into_iter().flatten().filter(|e| e["all_day"] != true).map(|e| format!("{} {}", time(&e["start"]).rsplit(' ').next().unwrap_or(""), time(&e["title"]))).collect();
            parts.push(Part { title: format!("Today: {} meeting{}", lines.len(), if lines.len() == 1 { "" } else { "s" }), lines });
        }
        // How tomorrow starts.
        if let Ok(v) = crate::calendar::call("cal_agenda", &json!({ "when": "tomorrow" })) {
            let list: Vec<&Value> = v["events"].as_array().into_iter().flatten().filter(|e| e["all_day"] != true).collect();
            let mut lines = Vec::new();
            if let Some(first) = list.first() {
                lines.push(format!("first: {} {}", time(&first["start"]).rsplit(' ').next().unwrap_or(""), time(&first["title"])));
            }
            if list.len() > 1 {
                lines.push(format!("{} meetings in all", list.len()));
            }
            if lines.is_empty() {
                lines.push("no meetings".into());
            }
            parts.push(Part { title: "Tomorrow".into(), lines });
        }
    }
    if crate::pmi::configured_for(&user) {
        let today = Local::now().date_naive();
        if let Ok(v) = crate::pmi::call("pmi_tasks", &json!({ "due": "week" })) {
            let tasks: Vec<&Value> = v["tasks"].as_array().into_iter().flatten().collect();
            let due = |t: &&Value| t["due"].as_str().and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
            let slipped: Vec<String> = tasks.iter().filter(|t| due(t).is_some_and(|d| d <= today)).map(|t| format!("{} (due {})", time(&t["title"]), time(&t["due"]))).collect();
            let tomorrow: Vec<String> = tasks.iter().filter(|t| due(t) == Some(today + Duration::days(1))).map(|t| time(&t["title"])).collect();
            if !slipped.is_empty() {
                parts.push(Part { title: format!("Still open: {} due by today", slipped.len()), lines: slipped.into_iter().take(8).collect() });
            }
            if !tomorrow.is_empty() {
                parts.push(Part { title: format!("Due tomorrow: {}", tomorrow.len()), lines: tomorrow.into_iter().take(8).collect() });
            }
        }
    }
    if crate::mail::connected_for(&user) {
        let mut lines = Vec::new();
        if let Ok(unread) = crate::mail::inbox(true, true, 25)
            && !unread.is_empty()
        {
            lines.push(format!("{} unread in Focused{}", unread.len(), if unread.len() >= 25 { "+" } else { "" }));
        }
        // Mail they sent that hasn't been answered (not marked as nudged: this only looks).
        let mut seen = crate::proactive::Seen::default();
        lines.extend(crate::proactive::followups(&mut seen).into_iter().take(5));
        if !lines.is_empty() {
            parts.push(Part { title: "Mail".into(), lines });
        }
    }
    Recap { at: now, parts }
}

/// The recap as text (`/recap`, the push).
pub fn describe(r: &Recap) -> String {
    if r.parts.is_empty() {
        return "Nothing to recap: connect Outlook (Profile → Connections → Outlook) or PMI for your day's facts.".into();
    }
    let mut out = vec![format!("End of day · {}", r.at.with_timezone(&Local).format("%a %b %-d"))];
    for p in &r.parts {
        out.push(format!("\n{}", p.title));
        out.extend(p.lines.iter().map(|l| format!("  · {l}")));
    }
    out.join("\n")
}

/// The push: the headline of each part.
pub fn push_body(r: &Recap) -> String {
    r.parts.iter().map(|p| p.title.clone()).collect::<Vec<_>>().join(" · ")
}

/// `/recap`: make it now.
pub fn command(user: &str) -> Result<String, String> {
    let r = crate::acting::run(user, || gather(Utc::now()));
    save_for(user, &r);
    Ok(describe(&r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_is_due_once_at_the_end_of_a_working_day() {
        configure(Settings::default());
        // A Wednesday.
        let wed = chrono::NaiveDate::from_ymd_opt(2026, 10, 7).unwrap();
        use chrono::TimeZone;
        let at = |h: u32, m: u32| Local.from_local_datetime(&wed.and_hms_opt(h, m, 0).unwrap()).unwrap();
        assert_eq!(due_at(at(9, 0)).map(|t| t.format("%H:%M").to_string()), Some("16:30".into()), "the planner's day end");
        let sat = Local.from_local_datetime(&chrono::NaiveDate::from_ymd_opt(2026, 10, 10).unwrap().and_hms_opt(17, 0, 0).unwrap()).unwrap();
        assert!(due_at(sat).is_none(), "not on a weekend");
        let r = Recap { at: Utc::now(), parts: vec![Part { title: "Today: 2 meetings".into(), lines: vec!["09:00 Standup".into()] }, Part { title: "Tomorrow".into(), lines: vec!["no meetings".into()] }] };
        assert_eq!(push_body(&r), "Today: 2 meetings · Tomorrow");
        assert!(describe(&r).contains("  · 09:00 Standup"));
    }
}
