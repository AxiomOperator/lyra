//! Plan my day (`[planner]`): private focus blocks on a person's own Outlook
//! calendar for their PMI tasks due soon, fitted around meetings within the
//! working day (lunch kept free), and moved or removed when the day changes.
//! lyra does this by itself (the blocks are only theirs) and says what it did.
//! The planning is a pure function (`plan`); `run` reads and writes Outlook.

use std::sync::RwLock;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use serde::Deserialize;
use serde_json::{Value, json};

/// `[planner]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// The working day.
    pub day_start: String,
    pub day_end: String,
    /// Kept free.
    pub lunch_start: String,
    pub lunch_end: String,
    /// Working days ("weekdays", or names: "mon tue wed thu fri").
    pub days: String,
    /// At most this many focus blocks a day.
    pub max_blocks: usize,
    /// Tasks due within this many days get blocks (overdue first).
    pub horizon_days: i64,
    /// Blocks take at most this share of the day's free time.
    pub max_share: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            day_start: "07:30".into(),
            day_end: "16:30".into(),
            lunch_start: "11:30".into(),
            lunch_end: "12:30".into(),
            days: "weekdays".into(),
            max_blocks: 3,
            horizon_days: 3,
            max_share: 0.5,
        }
    }
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

pub fn configure(s: Settings) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

pub fn settings() -> Settings {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

fn hm(s: &str, fallback: (u32, u32)) -> NaiveTime {
    crate::routines::time_of_day(s).unwrap_or_else(|| NaiveTime::from_hms_opt(fallback.0, fallback.1, 0).unwrap_or_default())
}

impl Settings {
    pub fn workday(&self, d: NaiveDate) -> bool {
        let days = self.days.to_lowercase();
        let wd = d.weekday();
        if days.contains("weekday") {
            return !matches!(wd, Weekday::Sat | Weekday::Sun);
        }
        let name = match wd {
            Weekday::Mon => "mon",
            Weekday::Tue => "tue",
            Weekday::Wed => "wed",
            Weekday::Thu => "thu",
            Weekday::Fri => "fri",
            Weekday::Sat => "sat",
            Weekday::Sun => "sun",
        };
        days.contains(name) || days.contains("every")
    }

    fn at(&self, d: NaiveDate, t: NaiveTime) -> Option<DateTime<Utc>> {
        Local.from_local_datetime(&d.and_time(t)).earliest().map(|x| x.with_timezone(&Utc))
    }

    /// The working day's span and lunch, in UTC.
    pub(crate) fn hours(&self, d: NaiveDate) -> Option<(Span, Span)> {
        Some((
            (self.at(d, hm(&self.day_start, (7, 30)))?, self.at(d, hm(&self.day_end, (16, 30)))?),
            (self.at(d, hm(&self.lunch_start, (11, 30)))?, self.at(d, hm(&self.lunch_end, (12, 30)))?),
        ))
    }

    /// Within working hours on a working day (for quiet times).
    pub fn working_now(&self, now: DateTime<Local>) -> bool {
        let d = now.date_naive();
        self.workday(d) && self.hours(d).is_some_and(|((s, e), _)| now.with_timezone(&Utc) >= s && now.with_timezone(&Utc) < e)
    }
}

/// From, to.
type Span = (DateTime<Utc>, DateTime<Utc>);

/// A task that could get time.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub due: Option<NaiveDate>,
    pub priority: String,
}

impl Task {
    /// How long it gets: urgent and high work longer.
    fn minutes(&self) -> i64 {
        match self.priority.as_str() {
            "urgent" | "high" => 90,
            "medium" => 60,
            _ => 30,
        }
    }
}

/// A focus block lyra made (on the calendar already, or to make).
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// The calendar event, once there.
    pub event: Option<String>,
    pub task: String,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// What to change on the calendar.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Create(Block),
    Move { event: String, title: String, start: DateTime<Utc>, end: DateTime<Utc> },
    Remove { event: String, title: String, why: &'static str },
}

fn overlaps(a: (DateTime<Utc>, DateTime<Utc>), b: (DateTime<Utc>, DateTime<Utc>)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// Free stretches of at least 30 minutes from `from` to the day's end, around `busy`.
fn free(day: (DateTime<Utc>, DateTime<Utc>), from: DateTime<Utc>, busy: &[(DateTime<Utc>, DateTime<Utc>)]) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    let mut busy = busy.to_vec();
    busy.sort();
    let mut cursor = day.0.max(from);
    let mut out = Vec::new();
    for (s, e) in busy {
        if s > cursor && s.min(day.1) - cursor >= Duration::minutes(30) {
            out.push((cursor, s.min(day.1)));
        }
        cursor = cursor.max(e);
        if cursor >= day.1 {
            return out;
        }
    }
    if day.1 - cursor >= Duration::minutes(30) {
        out.push((cursor, day.1));
    }
    out
}

/// Plan the rest of `date` (from `now`): keep the blocks that still fit, move
/// the ones a meeting landed on, drop the ones whose task is done, and add
/// blocks for the most pressing tasks without one, within the limits.
pub fn plan(s: &Settings, date: NaiveDate, now: DateTime<Utc>, meetings: &[(DateTime<Utc>, DateTime<Utc>)], tasks: &[Task], existing: &[Block]) -> Vec<Change> {
    let Some((day, lunch)) = s.hours(date) else { return vec![] };
    if !s.workday(date) {
        return vec![];
    }
    // From the next quarter hour.
    let from = {
        let m = now.timestamp() / 60;
        DateTime::<Utc>::from_timestamp((m + (15 - m % 15) % 15) * 60, 0).unwrap_or(now)
    };
    let mut busy: Vec<(DateTime<Utc>, DateTime<Utc>)> = meetings.to_vec();
    busy.push(lunch);
    let total_free: i64 = free(day, day.0, &busy).iter().map(|(a, b)| (*b - *a).num_minutes()).sum();
    let budget = (total_free as f64 * s.max_share) as i64;

    let horizon = date + Duration::days(s.horizon_days);
    let rank = |t: &Task| (t.due.unwrap_or(NaiveDate::MAX), match t.priority.as_str() { "urgent" => 0, "high" => 1, "medium" => 2, _ => 3 });
    let mut wanted: Vec<&Task> = tasks.iter().filter(|t| t.due.is_some_and(|d| d <= horizon)).collect();
    wanted.sort_by_key(|t| rank(t));

    let mut changes = Vec::new();
    let mut kept: Vec<Block> = Vec::new();
    let mut to_place: Vec<(Option<String>, &Task)> = Vec::new();
    for b in existing {
        let still = wanted.iter().find(|t| t.id == b.task);
        match still {
            None => changes.push(Change::Remove { event: b.event.clone().unwrap_or_default(), title: b.title.clone(), why: "its task is done or no longer due soon" }),
            // Already over: leave it be.
            Some(_) if b.end <= now => kept.push(b.clone()),
            // A meeting landed on it: it moves.
            Some(t) if busy.iter().any(|m| overlaps(*m, (b.start, b.end))) => to_place.push((b.event.clone(), t)),
            Some(_) => kept.push(b.clone()),
        }
    }
    let mut used: i64 = kept.iter().map(|b| (b.end - b.start).num_minutes()).sum();
    let mut count = kept.len();
    for t in &wanted {
        if count + to_place.len() >= s.max_blocks {
            break;
        }
        if kept.iter().any(|b| b.task == t.id) || to_place.iter().any(|(_, x)| x.id == t.id) {
            continue;
        }
        to_place.push((None, t));
    }
    for (event, t) in to_place {
        let mut taken: Vec<(DateTime<Utc>, DateTime<Utc>)> = busy.clone();
        taken.extend(kept.iter().map(|b| (b.start, b.end)));
        let want = t.minutes().min((budget - used).max(0));
        // The first slot it fits in whole, else the first that takes at least half an hour.
        let slots = free(day, from, &taken);
        let slot = slots
            .iter()
            .find(|(a, b)| (*b - *a).num_minutes() >= want && want >= 30)
            .map(|(a, _)| (*a, *a + Duration::minutes(want)))
            .or_else(|| slots.iter().find_map(|(a, b)| {
                let len = (*b - *a).num_minutes().min(want);
                (len >= 30).then(|| (*a, *a + Duration::minutes(len)))
            }));
        match (slot, event) {
            (Some((a, b)), Some(ev)) if count < s.max_blocks => {
                changes.push(Change::Move { event: ev.clone(), title: t.title.clone(), start: a, end: b });
                kept.push(Block { event: Some(ev), task: t.id.clone(), title: t.title.clone(), start: a, end: b });
                used += (b - a).num_minutes();
                count += 1;
            }
            (Some((a, b)), None) if count < s.max_blocks => {
                let block = Block { event: None, task: t.id.clone(), title: t.title.clone(), start: a, end: b };
                changes.push(Change::Create(block.clone()));
                kept.push(block);
                used += (b - a).num_minutes();
                count += 1;
            }
            (_, Some(ev)) => changes.push(Change::Remove { event: ev, title: t.title.clone(), why: "a meeting took its time and there's no other free slot today" }),
            _ => {}
        }
    }
    changes
}

// ---- Outlook

const MARK: &str = "lyra focus block · PMI task ";

/// lyra's own blocks among a day's events.
fn blocks_in(events: &[Value]) -> Vec<Block> {
    events
        .iter()
        .filter(|e| e["categories"].as_array().is_some_and(|c| c.iter().any(|x| x == "lyra")))
        .filter_map(|e| {
            let task = e["bodyPreview"].as_str()?.split(MARK).nth(1)?.split_whitespace().next()?.to_string();
            Some(Block {
                event: e["id"].as_str().map(str::to_string),
                task,
                title: e["subject"].as_str().unwrap_or("").trim_start_matches("Focus: ").to_string(),
                start: crate::graph::utc(&e["start"])?,
                end: crate::graph::utc(&e["end"])?,
            })
        })
        .collect()
}

/// Everything else that takes time (meetings, not lyra's blocks, not "free").
fn meetings_in(events: &[Value]) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    events
        .iter()
        .filter(|e| e["isCancelled"] != true && e["showAs"] != "free" && !e["categories"].as_array().is_some_and(|c| c.iter().any(|x| x == "lyra")))
        .filter_map(|e| Some((crate::graph::utc(&e["start"])?, crate::graph::utc(&e["end"])?)))
        .collect()
}

fn tasks_from(v: &Value) -> Vec<Task> {
    v["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["status"] != "done")
        .map(|t| Task {
            id: t["id"].as_str().unwrap_or("").to_string(),
            title: t["title"].as_str().unwrap_or("").to_string(),
            due: t["due"].as_str().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()),
            priority: t["priority"].as_str().unwrap_or("medium").to_string(),
        })
        .collect()
}

fn local_hm(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%H:%M").to_string()
}

/// Plan today for the person this thread works for, and do it. Returns what
/// changed, in words (empty: nothing to do).
pub fn run() -> Result<Vec<String>, String> {
    let s = settings();
    let user = crate::acting::current();
    if !s.enabled || !crate::graph::connected_for(&user) || !crate::pmi::configured_for(&user) {
        return Ok(vec![]);
    }
    let now = Local::now();
    let date = now.date_naive();
    if !s.workday(date) {
        return Ok(vec![]);
    }
    let Some((day, _)) = s.hours(date) else { return Ok(vec![]) };
    let events = crate::calendar::events(day.0, day.1)?;
    let tasks = tasks_from(&crate::pmi::call("pmi_tasks", &json!({ "due": "week" }))?);
    let changes = plan(&s, date, now.with_timezone(&Utc), &meetings_in(&events), &tasks, &blocks_in(&events));
    let mut said = Vec::new();
    for c in changes {
        match c {
            Change::Create(b) => {
                let body = json!({
                    "subject": format!("Focus: {}", b.title),
                    "start": crate::graph::graph_time(b.start),
                    "end": crate::graph::graph_time(b.end),
                    "showAs": "busy",
                    "sensitivity": "private",
                    "isReminderOn": false,
                    "categories": ["lyra"],
                    "body": { "contentType": "text", "content": format!("{MARK}{} (lyra moves or removes this block as your day changes)", b.task) },
                });
                crate::graph::graph(reqwest::Method::POST, "/me/events", Some(&body))?;
                said.push(format!("focus block {}–{} for {}", local_hm(b.start), local_hm(b.end), b.title));
            }
            Change::Move { event, title, start, end } => {
                let body = json!({ "start": crate::graph::graph_time(start), "end": crate::graph::graph_time(end) });
                crate::graph::graph(reqwest::Method::PATCH, &format!("/me/events/{}", lyra_web::oidc::encode(&event)), Some(&body))?;
                said.push(format!("moved the focus block for {title} to {}–{} (a meeting took its time)", local_hm(start), local_hm(end)));
            }
            Change::Remove { event, title, why } => {
                crate::graph::graph(reqwest::Method::DELETE, &format!("/me/events/{}", lyra_web::oidc::encode(&event)), None)?;
                said.push(format!("removed the focus block for {title}: {why}"));
            }
        }
    }
    Ok(said)
}

/// The day as lyra sees it now: meetings and blocks, due tasks, mail to answer.
pub fn today() -> Result<Value, String> {
    let user = crate::acting::current();
    let s = settings();
    let date = Local::now().date_naive();
    let (day, lunch) = s.hours(date).ok_or("no such day")?;
    let events = if crate::graph::connected_for(&user) { crate::calendar::events(day.0 - Duration::hours(3), day.1 + Duration::hours(6)).unwrap_or_default() } else { vec![] };
    let blocks: Vec<Value> = blocks_in(&events).iter().map(|b| json!({ "title": b.title, "start": local_hm(b.start), "end": local_hm(b.end), "task": b.task })).collect();
    let tasks = if crate::pmi::configured_for(&user) { crate::pmi::call("pmi_tasks", &json!({ "due": "week" })).map(|v| tasks_from(&v)).unwrap_or_default() } else { vec![] };
    let mail = if crate::mail::connected_for(&user) { crate::mail::inbox(true, true, 3).map(|m| m.iter().map(|x| json!({ "from": x["from"]["emailAddress"]["name"], "subject": x["subject"] })).collect::<Vec<_>>()).unwrap_or_default() } else { vec![] };
    Ok(json!({
        "workday": s.workday(date),
        "hours": format!("{}–{}, lunch {}–{}", local_hm(day.0), local_hm(day.1), local_hm(lunch.0), local_hm(lunch.1)),
        "blocks": blocks,
        "due": tasks.iter().filter(|t| t.due.is_some_and(|d| d <= date + Duration::days(s.horizon_days))).map(|t| json!({ "title": t.title, "due": t.due.map(|d| d.to_string()) })).collect::<Vec<_>>(),
        "answer_first": mail,
    }))
}

/// `/today [plan]`: the day; plan re-plans it now.
pub fn command(arg: &str) -> Result<String, String> {
    let user = crate::acting::current();
    if !crate::graph::connected_for(&user) || !crate::pmi::configured_for(&user) {
        return Err("plan my day needs your Outlook calendar and PMI connected (More in the app; /pmi token)".into());
    }
    let mut out = Vec::new();
    if arg.trim() == "plan" {
        let did = run()?;
        out.push(if did.is_empty() { "the plan stands: nothing to change".to_string() } else { did.join("\n") });
    }
    let t = today()?;
    if t["workday"] == false {
        out.push("not a working day: no focus blocks today".into());
    }
    let blocks = t["blocks"].as_array().cloned().unwrap_or_default();
    out.push(if blocks.is_empty() { "no focus blocks today (/today plan makes them)".into() } else { format!("focus: {}", blocks.iter().map(|b| format!("{}–{} {}", b["start"].as_str().unwrap_or(""), b["end"].as_str().unwrap_or(""), b["title"].as_str().unwrap_or(""))).collect::<Vec<_>>().join(" · ")) });
    let due = t["due"].as_array().cloned().unwrap_or_default();
    if !due.is_empty() {
        out.push(format!("due soon: {}", due.iter().map(|d| format!("{} ({})", d["title"].as_str().unwrap_or(""), d["due"].as_str().unwrap_or(""))).collect::<Vec<_>>().join(" · ")));
    }
    let mail = t["answer_first"].as_array().cloned().unwrap_or_default();
    if !mail.is_empty() {
        out.push(format!("answer first: {}", mail.iter().map(|m| format!("{}: {}", m["from"].as_str().unwrap_or("?"), m["subject"].as_str().unwrap_or(""))).collect::<Vec<_>>().join(" · ")));
    }
    Ok(out.join("\n"))
}

/// Quiet time for a person: outside working hours, or in a meeting now.
pub fn quiet(user: &str) -> bool {
    let s = settings();
    let now = Local::now();
    if !s.working_now(now) {
        return true;
    }
    if !crate::graph::connected_for(user) {
        return false;
    }
    let t = now.with_timezone(&Utc);
    crate::acting::run(user, || crate::calendar::events(t - Duration::hours(4), t + Duration::minutes(1)))
        .map(|evs| meetings_in(&evs).iter().any(|m| m.0 <= t && t < m.1))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wednesday 2026-10-07.
    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 7).unwrap()
    }

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Local.from_local_datetime(&day().and_hms_opt(h, m, 0).unwrap()).earliest().unwrap().with_timezone(&Utc)
    }

    fn task(id: &str, due_in: i64, priority: &str) -> Task {
        Task { id: id.into(), title: format!("task {id}"), due: Some(day() + Duration::days(due_in)), priority: priority.into() }
    }

    fn hours(c: &Change) -> String {
        match c {
            Change::Create(b) => format!("+{} {}-{}", b.task, local_hm(b.start), local_hm(b.end)),
            Change::Move { title, start, end, .. } => format!("~{title} {}-{}", local_hm(*start), local_hm(*end)),
            Change::Remove { title, .. } => format!("-{title}"),
        }
    }

    #[test]
    fn blocks_go_around_meetings_and_lunch_most_pressing_first() {
        let s = Settings::default();
        let meetings = vec![(at(8, 0), at(9, 30)), (at(13, 0), at(14, 0))];
        let tasks = vec![task("later", 2, "low"), task("overdue", -1, "high"), task("today", 0, "medium"), task("far", 10, "urgent"), task("fourth", 1, "low")];
        let plan = plan(&s, day(), at(7, 40), &meetings, &tasks, &[]);
        let shown: Vec<String> = plan.iter().map(hours).collect();
        // From 07:45 (07:45–08:00 is too short). The overdue one first, each
        // where it fits whole; the day has 330 free minutes, so blocks take at
        // most 165: after 90 + 60 the third doesn't fit. The far one waits.
        assert_eq!(shown, ["+overdue 09:30-11:00", "+today 14:00-15:00"]);
        // Room for a third on a lighter day: at most three.
        let light = super::plan(&s, day(), at(7, 40), &[], &tasks, &[]);
        assert_eq!(light.len(), 3, "{:?}", light.iter().map(hours).collect::<Vec<_>>());
    }

    #[test]
    fn a_block_a_meeting_landed_on_moves_and_a_done_task_goes() {
        let s = Settings::default();
        let existing = vec![
            Block { event: Some("e1".into()), task: "a".into(), title: "task a".into(), start: at(10, 0), end: at(11, 0) },
            Block { event: Some("e2".into()), task: "gone".into(), title: "old".into(), start: at(14, 0), end: at(15, 0) },
        ];
        let meetings = vec![(at(10, 0), at(10, 30))];
        let plan = plan(&s, day(), at(9, 50), &meetings, &[task("a", 0, "medium")], &existing);
        let shown: Vec<String> = plan.iter().map(hours).collect();
        assert_eq!(shown, ["-old", "~task a 10:30-11:30"], "{shown:?}");
    }

    #[test]
    fn weekends_and_full_days_are_left_alone() {
        let s = Settings::default();
        let saturday = NaiveDate::from_ymd_opt(2026, 10, 10).unwrap();
        assert!(plan(&s, saturday, at(9, 0), &[], &[task("a", 0, "high")], &[]).is_empty());
        let full = vec![(at(7, 30), at(16, 30))];
        assert!(plan(&s, day(), at(7, 30), &full, &[task("a", 0, "high")], &[]).is_empty(), "no time, no block");
        let blocks = blocks_in(&[json!({ "id": "x", "subject": "Focus: write the report", "categories": ["lyra"], "bodyPreview": "lyra focus block · PMI task t-42 (lyra moves…)", "start": crate::graph::graph_time(at(9, 0)), "end": crate::graph::graph_time(at(10, 0)) })]);
        assert_eq!((blocks[0].task.as_str(), blocks[0].title.as_str()), ("t-42", "write the report"));
    }
}
