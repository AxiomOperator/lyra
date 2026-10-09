//! The daily briefing (`[briefing]`): once a morning, what happened since the
//! last one and what needs a look: machines, lyra's own checks, routines,
//! diagnoses, coding jobs and goals. The facts are gathered here from what
//! lyra already keeps; the chat model only adds an optional one-line
//! takeaway. It only reads: no machine is touched. Pushed when it's made,
//! shown on the Status page and by `/briefing`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

use chrono::{DateTime, Duration, Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::coding::Record;
use crate::diagnose::Diagnosis;
use crate::routines::Run;
use crate::status::{Board, State};
use lyra_goals::{Goal, GoalEvent, GoalStatus};

/// `[briefing]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// When, in the routines' words: "every day at 07:30", "weekdays at 8".
    pub schedule: String,
    /// Push it to paired devices.
    pub notify: bool,
    /// Let the chat model add a one-line takeaway.
    pub summary: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: true, schedule: "every day at 07:30".into(), notify: true, summary: true }
    }
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

pub fn configure(s: Settings) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

pub fn settings() -> Settings {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

/// How much an item matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Attention,
    Note,
    Ok,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub level: Level,
    pub text: String,
    /// The app page it's about ("tasks", "machines", "status", "routines", "coding", "goals").
    pub link: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Briefing {
    pub at: DateTime<Utc>,
    /// The window it covers starts here.
    pub since: DateTime<Utc>,
    /// "All quiet" or "2 things need a look".
    pub headline: String,
    pub attention: usize,
    /// The chat model's one line, when there is one.
    #[serde(default)]
    pub takeaway: Option<String>,
    pub sections: Vec<Section>,
}

/// What a briefing is made from (collected by the caller, cheaply).
#[derive(Default)]
pub struct Inputs {
    pub now: DateTime<Utc>,
    pub since: DateTime<Utc>,
    /// The server's own health report.
    pub server_health: Option<Value>,
    /// Paired machines (`machines_detail` rows: name, online, last_seen, update_available, health).
    pub machines: Vec<Value>,
    pub board: Option<Board>,
    pub runs: HashMap<String, Vec<Run>>,
    pub diagnoses: Vec<Diagnosis>,
    pub jobs: Vec<Record>,
    pub goals: Vec<Goal>,
    pub goal_events: Vec<GoalEvent>,
    /// The user's PMI: tasks, what waits on them, projects.
    pub pmi: Option<crate::pmi::State>,
    /// Their Outlook calendar today (`calendar::today`), when connected.
    pub calendar: Option<Value>,
    /// Their mail since the last briefing (`mail::glance`), when connected.
    pub mail: Option<Value>,
    /// Their Teams chats with something unread, when connected.
    pub teams: Option<Vec<Value>>,
}

fn item(level: Level, link: &str, text: impl Into<String>) -> Item {
    Item { level, text: text.into(), link: link.into() }
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").replace("**", "");
    if line.chars().count() > max { format!("{}…", line.chars().take(max).collect::<String>()) } else { line }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// One machine's lines: problems, pending updates, a newer lyra-node.
fn machine_items(name: &str, health: Option<&Value>, out: &mut Vec<Item>) {
    let Some(h) = health else {
        out.push(item(Level::Ok, "machines", format!("{name}: online, no health report yet")));
        return;
    };
    let problems = crate::health::problems(h, &crate::health::settings());
    for p in &problems {
        out.push(item(Level::Attention, "machines", format!("{name}: {}", p.text)));
    }
    if let Some(u) = h["updates"].as_u64().filter(|u| *u > 0) {
        out.push(item(Level::Note, "machines", format!("{name}: {} pending", plural(u as usize, "update", "updates"))));
    }
    if problems.is_empty() {
        out.push(item(Level::Ok, "machines", format!("{name}: {}", crate::health::summary(h))));
    }
}

fn machines(i: &Inputs) -> Vec<Item> {
    let mut out = Vec::new();
    if i.server_health.is_some() {
        machine_items("server", i.server_health.as_ref(), &mut out);
    }
    for m in &i.machines {
        let name = m["name"].as_str().unwrap_or("?");
        if m["online"].as_bool() != Some(true) {
            let seen = m["last_seen"].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|t| format!(" (last seen {})", ago(i.now, t.with_timezone(&Utc))));
            out.push(item(Level::Note, "machines", format!("{name} is offline{}", seen.unwrap_or_default())));
            continue;
        }
        if m["update_available"].as_bool() == Some(true) {
            out.push(item(Level::Note, "machines", format!("{name}: a newer lyra-node is ready (/machines update {name})")));
        }
        machine_items(name, m.get("health").filter(|h| !h.is_null()), &mut out);
    }
    out
}

/// lyra's own checks; machines are left to their section.
fn checks(i: &Inputs) -> Vec<Item> {
    let Some(board) = &i.board else { return vec![] };
    let mut out = Vec::new();
    let mut fine = 0;
    for r in board.rows.iter().filter(|r| !r.probe.id.starts_with("machine:")) {
        let p = &r.probe;
        match p.state {
            State::Down => out.push(item(Level::Attention, "status", format!("{} is down: {}", p.name, p.detail))),
            State::Degraded => out.push(item(Level::Note, "status", format!("{}: {}", p.name, p.detail))),
            State::Up => match r.uptime_24h.filter(|u| *u < 0.99) {
                Some(u) => out.push(item(Level::Note, "status", format!("{} answered {:.0}% of checks in the last day", p.name, u * 100.0))),
                None => fine += 1,
            },
            State::Off => {}
        }
    }
    if fine > 0 {
        out.push(item(Level::Ok, "status", format!("{} normal", plural(fine, "check", "checks"))));
    }
    out
}

fn routines(i: &Inputs) -> Vec<Item> {
    let mut out = Vec::new();
    let mut names: Vec<&String> = i.runs.keys().collect();
    names.sort();
    for name in names {
        let runs: Vec<&Run> = i.runs[name].iter().filter(|r| r.at > i.since && r.at <= i.now).collect();
        if runs.is_empty() {
            continue;
        }
        // Newest first: the latest that went wrong or wants the user, else all fine.
        if let Some(r) = runs.iter().find(|r| r.outcome == "error") {
            out.push(item(Level::Attention, "routines", format!("{name} failed: {}", first_line(&r.summary, 140))));
        } else if let Some(r) = runs.iter().find(|r| r.needs_user) {
            out.push(item(Level::Attention, "routines", format!("{name}: {}", first_line(&r.summary, 140))));
        } else {
            out.push(item(Level::Ok, "routines", format!("{name}: {} fine", plural(runs.len(), "run", "runs"))));
        }
    }
    out
}

/// PMI: overdue and today's tasks, what waits on the user, projects off track.
fn tasks(i: &Inputs) -> Vec<Item> {
    let Some(p) = &i.pmi else { return vec![] };
    let mut out = Vec::new();
    if let Some(e) = &p.error {
        out.push(item(Level::Note, "tasks", format!("PMI couldn't be read: {e}")));
    }
    let today = i.now.with_timezone(&Local).date_naive();
    let due = |t: &Value| t["due"].as_str().and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    let title = |t: &Value| format!("{}{}", t["title"].as_str().unwrap_or("?"), if t["where"] == "personal" { String::new() } else { format!(" ({})", t["where"].as_str().unwrap_or("")) });
    for t in p.tasks.iter().filter(|t| due(t).is_some_and(|d| d < today)) {
        out.push(item(Level::Attention, "tasks", format!("overdue since {}: {}", t["due"].as_str().unwrap_or("?"), title(t))));
    }
    for t in p.tasks.iter().filter(|t| due(t) == Some(today)) {
        out.push(item(Level::Note, "tasks", format!("due today: {}", title(t))));
    }
    for (key, what) in [("task_transfers", "task"), ("project_transfers", "project")] {
        for w in p.waiting[key].as_array().into_iter().flatten() {
            out.push(item(Level::Attention, "tasks", format!("a {what} is being handed to you: {}", w[what].as_str().unwrap_or("?"))));
        }
    }
    for a in p.waiting["approvals"].as_array().into_iter().flatten() {
        out.push(item(Level::Attention, "tasks", format!("approval asked: {}", a["task"]["title"].as_str().unwrap_or("a task"))));
    }
    for pr in &p.projects {
        if matches!(pr["health"].as_str(), Some("at_risk" | "off_track")) {
            out.push(item(Level::Note, "tasks", format!("project {} is {}", pr["name"].as_str().unwrap_or("?"), pr["health"].as_str().unwrap_or("").replace('_', " "))));
        }
    }
    if p.inbox_unread > 0 {
        out.push(item(Level::Note, "tasks", format!("{} in your PMI inbox", plural(p.inbox_unread as usize, "unread item", "unread items"))));
    }
    if out.is_empty() {
        out.push(item(Level::Ok, "tasks", format!("{} open, nothing due today", plural(p.tasks.len(), "task", "tasks"))));
    }
    out
}

/// Today's meetings, clashes and invites waiting for an answer.
fn calendar(i: &Inputs) -> Vec<Item> {
    let Some(c) = &i.calendar else { return vec![] };
    let mut out = Vec::new();
    for clash in c["clashes"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        out.push(item(Level::Attention, "tasks", format!("double-booked: {clash}")));
    }
    for inv in c["invites"].as_array().into_iter().flatten() {
        out.push(item(Level::Attention, "tasks", format!("invite to answer: {} ({}, from {})", inv["title"].as_str().unwrap_or("?"), inv["start"].as_str().unwrap_or("?"), inv["organizer"].as_str().unwrap_or("?"))));
    }
    let events = c["events"].as_array().cloned().unwrap_or_default();
    for e in events.iter().take(10) {
        let at = e["start"].as_str().and_then(|s| s.rsplit(' ').next()).unwrap_or("");
        let at = if e["all_day"] == true { "all day" } else { at };
        out.push(item(Level::Note, "tasks", format!("{at} {}", e["title"].as_str().unwrap_or("?")).trim().to_string()));
    }
    if events.is_empty() && out.is_empty() {
        out.push(item(Level::Ok, "tasks", "nothing on the calendar today"));
    }
    out
}

/// New mail from people (not newsletters) since the last briefing, and what's flagged.
fn mail(i: &Inputs) -> Vec<Item> {
    let Some(m) = &i.mail else { return vec![] };
    let mut out = Vec::new();
    let new = m["new"].as_array().cloned().unwrap_or_default();
    for msg in new.iter().take(6) {
        let level = if msg["important"] == true { Level::Attention } else { Level::Note };
        let from = msg["from"].as_str().unwrap_or("?");
        let from = from.split(" <").next().unwrap_or(from);
        out.push(item(level, "tasks", format!("from {from}: {}", msg["subject"].as_str().unwrap_or("(no subject)"))));
    }
    if new.len() > 6 {
        out.push(item(Level::Note, "tasks", format!("and {} more new", new.len() - 6)));
    }
    let flagged = m["flagged"].as_array().map_or(0, Vec::len);
    if flagged > 0 {
        out.push(item(Level::Note, "tasks", format!("{} flagged to follow up", plural(flagged, "message", "messages"))));
    }
    if out.is_empty() {
        let unread = m["unread"].as_u64().unwrap_or(0);
        out.push(item(Level::Ok, "tasks", if unread > 0 { format!("no new mail from people ({unread} unread)") } else { "inbox clear".to_string() }));
    }
    out
}

/// Teams chats waiting for them.
fn teams(i: &Inputs) -> Vec<Item> {
    let Some(chats) = &i.teams else { return vec![] };
    let mut out: Vec<Item> = chats
        .iter()
        .take(6)
        .map(|c| {
            let last = c["last"].as_str().unwrap_or("");
            item(Level::Note, "tasks", format!("{}: {}{}", c["chat"].as_str().unwrap_or("a chat"), last.chars().take(80).collect::<String>(), if last.chars().count() > 80 { "…" } else { "" }))
        })
        .collect();
    if chats.len() > 6 {
        out.push(item(Level::Note, "tasks", format!("and {} more chats with something new", chats.len() - 6)));
    }
    out
}

fn diagnoses(i: &Inputs) -> Vec<Item> {
    i.diagnoses
        .iter()
        .filter(|d| d.at > i.since && d.at <= i.now)
        .filter_map(|d| {
            let what = format!("{}: {}", d.machine, d.problem);
            match (d.state.as_str(), d.resolved) {
                (_, true) => Some(item(Level::Ok, "machines", format!("{what} (cleared)"))),
                ("done", _) => Some(item(Level::Note, "machines", format!("{what} → {}", crate::diagnose::headline(&d.summary)))),
                ("failed", _) => Some(item(Level::Note, "machines", format!("{what}: couldn't be looked into"))),
                _ => None,
            }
        })
        .collect()
}

fn coding(i: &Inputs) -> Vec<Item> {
    i.jobs
        .iter()
        .filter(|j| j.at > i.since && j.at <= i.now)
        .map(|j| {
            let task = first_line(&j.task, 80);
            if j.ok {
                let files = if j.files.is_empty() { "no files changed".to_string() } else { format!("{} changed", plural(j.files.len(), "file", "files")) };
                item(Level::Ok, "coding", format!("{} on {}: {task} ({files})", j.harness, j.machine))
            } else {
                item(Level::Attention, "coding", format!("{} on {} didn't finish: {task}", j.harness, j.machine))
            }
        })
        .collect()
}

fn goals(i: &Inputs) -> Vec<Item> {
    let mut out = Vec::new();
    let title = |id| i.goals.iter().find(|g| g.id == id).map_or_else(|| "a goal".to_string(), |g| g.title.clone());
    let mut seen = Vec::new();
    // Newest first: one line per goal, from its latest event in the window.
    let mut events: Vec<&GoalEvent> = i.goal_events.iter().filter(|e| e.created_at > i.since && e.created_at <= i.now).collect();
    events.sort_by_key(|e| std::cmp::Reverse(e.created_at));
    for e in events {
        if seen.contains(&e.goal_id) {
            continue;
        }
        let level = match e.kind.as_str() {
            "completed" => Level::Ok,
            "blocked" | "failed" => Level::Attention,
            "progress_updated" | "plan_finished" | "unblocked" => Level::Note,
            _ => continue,
        };
        seen.push(e.goal_id);
        let progress = i.goals.iter().find(|g| g.id == e.goal_id).filter(|g| g.status == GoalStatus::Active).map(|g| format!(" ({:.0}%)", g.progress * 100.0)).unwrap_or_default();
        out.push(item(level, "goals", format!("{}{progress}: {}", title(e.goal_id), first_line(&e.message, 120))));
    }
    for g in &i.goals {
        let open = matches!(g.status, GoalStatus::Active | GoalStatus::Blocked);
        if open && g.due_at.is_some_and(|d| d < i.now) && !seen.contains(&g.id) {
            out.push(item(Level::Attention, "goals", format!("{} is overdue ({:.0}% done)", g.title, g.progress * 100.0)));
        }
    }
    out
}

/// Make a briefing from what's known.
pub fn gather(i: &Inputs) -> Briefing {
    let mut sections: Vec<Section> = [("Today", calendar(i)), ("Mail", mail(i)), ("Teams", teams(i)), ("Tasks", tasks(i)), ("Machines", machines(i)), ("lyra", checks(i)), ("Routines", routines(i)), ("Diagnoses", diagnoses(i)), ("Coding", coding(i)), ("Goals", goals(i))]
        .into_iter()
        .filter(|(_, items)| !items.is_empty())
        .map(|(name, items)| Section { name: name.into(), items })
        .collect();
    for s in &mut sections {
        s.items.sort_by_key(|it| it.level);
    }
    let attention = sections.iter().flat_map(|s| &s.items).filter(|it| it.level == Level::Attention).count();
    let headline = match attention {
        0 => "All quiet".to_string(),
        1 => "1 thing needs a look".to_string(),
        n => format!("{n} things need a look"),
    };
    Briefing { at: i.now, since: i.since, headline, attention, takeaway: None, sections }
}

fn ago(now: DateTime<Utc>, t: DateTime<Utc>) -> String {
    let s = (now - t).num_seconds().max(0);
    match s {
        s if s < 5400 => format!("{} min ago", s / 60),
        s if s < 172_800 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86400),
    }
}

// ---- the takeaway

/// The facts, small, for the chat model.
fn facts(b: &Briefing) -> String {
    let mut out = vec![format!("{} (since {})", b.headline, b.since.with_timezone(&Local).format("%a %H:%M"))];
    for s in &b.sections {
        for it in &s.items {
            let mark = match it.level {
                Level::Attention => "!",
                Level::Note => "-",
                Level::Ok => "ok",
            };
            out.push(format!("[{}] {mark} {}", s.name, it.text));
        }
    }
    out.join("\n").chars().take(6000).collect()
}

const TAKEAWAY: &str = "You write the one-line takeaway of a home lab's morning briefing. Given the facts, \
answer with ONE plain sentence (no markdown, under 30 words) saying what matters most today and why; \
if nothing needs attention, say so plainly. Use only the facts given.";

/// One sentence from the chat model, or none (it's optional).
pub fn takeaway(url: &str, model: &str, b: &Briefing) -> Option<String> {
    let (reply, _) = crate::learn::complete_light(url, model, TAKEAWAY, &facts(b)).ok()?;
    let reply = reply.rsplit_once("</think>").map_or(reply.as_str(), |(_, after)| after);
    let line = first_line(reply, 300).trim_matches('"').to_string();
    (!line.is_empty()).then_some(line)
}

// ---- text

/// `/briefing`.
pub fn describe(b: &Briefing) -> String {
    let when = b.at.with_timezone(&Local).format("%a %H:%M");
    let mut out = vec![format!("briefing {when}: {}", b.headline)];
    if let Some(t) = &b.takeaway {
        out.push(format!("  {t}"));
    }
    for s in &b.sections {
        out.push(format!("{}:", s.name));
        for it in &s.items {
            let mark = match it.level {
                Level::Attention => "⚠",
                Level::Note => "·",
                Level::Ok => "✓",
            };
            out.push(format!("  {mark} {}", it.text));
        }
    }
    out.push("/briefing now makes a new one · [briefing] schedule sets when".into());
    out.join("\n")
}

/// The push: the takeaway or the first few things that need a look.
pub fn push_body(b: &Briefing) -> String {
    let attention: Vec<&str> = b.sections.iter().flat_map(|s| &s.items).filter(|it| it.level == Level::Attention).map(|it| it.text.as_str()).collect();
    let mut lines: Vec<String> = Vec::new();
    if let Some(t) = &b.takeaway {
        lines.push(t.clone());
    }
    lines.extend(attention.iter().take(3).map(|t| format!("⚠ {t}")));
    if attention.len() > 3 {
        lines.push(format!("and {} more", attention.len() - 3));
    }
    if lines.is_empty() {
        let fine = b.sections.iter().map(|s| s.items.len()).sum::<usize>();
        lines.push(format!("Nothing needs you · {} looked at", plural(fine, "thing", "things")));
    }
    let body = lines.join("\n");
    if body.chars().count() > 400 { format!("{}…", body.chars().take(400).collect::<String>()) } else { body }
}

/// For the panels: "07:30 · All quiet".
pub fn line(b: &Briefing) -> String {
    format!("{} · {}", b.at.with_timezone(&Local).format("%H:%M"), b.headline)
}

/// What every briefing can read without a server: lyra's checks, routine
/// runs, diagnoses, coding jobs and goals (machines are added by `lyra serve`).
pub fn local_inputs(goals: Option<&crate::goals::Goals>, now: DateTime<Utc>, since: DateTime<Utc>) -> Inputs {
    let (goals, goal_events) = goals.map_or_else(Default::default, |g| (g.manager.all().unwrap_or_default(), g.manager.events(None, 300).unwrap_or_default()));
    Inputs {
        now,
        since,
        board: crate::status::latest(),
        runs: crate::routines::runs(),
        diagnoses: crate::diagnose::all(),
        jobs: crate::coding::jobs(),
        goals,
        goal_events,
        ..Default::default()
    }
}

/// Whose briefing was asked for now (`/briefing now`, the Status page's button).
static WANTED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Make this person's briefing now (theirs only: nobody else is pushed one).
pub fn request_for(user: &str) {
    let mut w = WANTED.lock().unwrap_or_else(|e| e.into_inner());
    if !w.iter().any(|u| u == user) {
        w.push(user.to_string());
    }
}

/// Who asked for one since the last look.
pub fn take_requests() -> Vec<String> {
    std::mem::take(&mut *WANTED.lock().unwrap_or_else(|e| e.into_inner()))
}

// ---- storage and timing

fn path(user: &str) -> Option<PathBuf> {
    if user == lyra_web::users::OWNER { Some(crate::config::home()?.join("briefing").join("last.json")) } else { Some(crate::context::user_dir(user)?.join("briefing").join("last.json")) }
}

/// The last briefing made (it survives restarts, so one isn't sent twice).
pub fn last() -> Option<Briefing> {
    last_for(lyra_web::users::OWNER)
}

/// One person's last briefing.
pub fn last_for(user: &str) -> Option<Briefing> {
    crate::store::read_json::<Option<Briefing>>(&path(user)?)
}

pub fn save_for(user: &str, b: &Briefing) {
    if let Some(p) = path(user) {
        let _ = crate::store::write_json(&p, b);
    }
}

/// When the next one is due. One missed while lyra was down is made on start
/// if it's less than 3 hours late; an older one is skipped.
pub fn next(schedule: &str, last: Option<DateTime<Utc>>, now: DateTime<Local>) -> Option<DateTime<Local>> {
    let s = crate::routines::parse_schedule(schedule).ok()?;
    let from = last.map_or(now, |t| t.with_timezone(&Local));
    let next = crate::routines::next_after(&s, from)?;
    if next < now - Duration::hours(3) { crate::routines::next_after(&s, now) } else { Some(next) }
}

/// The window a new briefing covers: since the last one (at most a week back), else a day.
pub fn window_start(last: Option<DateTime<Utc>>, now: DateTime<Utc>) -> DateTime<Utc> {
    last.filter(|t| *t < now).map_or(now - Duration::days(1), |t| t.max(now - Duration::days(7)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{Probe, Row};
    use serde_json::json;

    fn run(at: DateTime<Utc>, outcome: &str, needs_user: bool, summary: &str) -> Run {
        Run { at, seconds: 5, needs_user, outcome: outcome.into(), summary: summary.into(), session: "s".into(), decided_by: String::new(), emailed: String::new(), calls: 0, tokens_in: 0, tokens_out: 0, cost: 0.0 }
    }

    fn row(id: &str, name: &str, state: State, uptime: Option<f64>) -> Row {
        Row {
            probe: Probe { id: id.into(), group: "lyra".into(), name: name.into(), target: String::new(), state, latency_ms: None, detail: "no answer".into() },
            uptime_24h: uptime,
            uptime_7d: None,
            spark: vec![],
            since: None,
            changes: vec![],
        }
    }

    fn healthy() -> Value {
        json!({ "disks": [{ "mount": "/", "used_pct": 40, "avail_kb": 1000 }], "memory": { "used_pct": 30 }, "load": [0.2, 0.2, 0.2], "cpus": 4, "failed_units": [], "updates": 0 })
    }

    #[test]
    fn a_quiet_day() {
        let now = Utc::now();
        let mut runs = HashMap::new();
        runs.insert("disk check".to_string(), vec![run(now - Duration::hours(2), "ok", false, "All fine."), run(now - Duration::days(3), "error", false, "old")]);
        let i = Inputs {
            now,
            since: now - Duration::days(1),
            server_health: Some(healthy()),
            machines: vec![json!({ "name": "desktop", "online": true, "health": healthy() }), json!({ "name": "win", "online": true, "health": null })],
            board: Some(Board { at: now, overall: State::Up, rows: vec![row("chat", "Chat model", State::Up, Some(1.0)), row("machine:desktop", "desktop", State::Up, None)] }),
            runs,
            ..Default::default()
        };
        let b = gather(&i);
        assert_eq!((b.headline.as_str(), b.attention), ("All quiet", 0));
        assert!(b.sections.iter().flat_map(|s| &s.items).all(|it| it.level == Level::Ok), "{b:#?}");
        assert!(push_body(&b).starts_with("Nothing needs you"));
        assert!(describe(&b).contains("disk check: 1 run fine"), "the old failure is outside the window");
        assert!(describe(&b).contains("win: online, no health report yet"), "a machine isn't left out before its first report");
    }

    #[test]
    fn what_needs_a_look_comes_first() {
        let now = Utc::now();
        let mut runs = HashMap::new();
        runs.insert("backup check".to_string(), vec![run(now - Duration::hours(1), "error", false, "**Backup failed**: disk full")]);
        let mut goal = Goal::new("Move the wiki", "");
        goal.status = GoalStatus::Active;
        goal.progress = 0.4;
        goal.due_at = Some(now - Duration::days(1));
        let mut sick = healthy();
        sick["disks"][0]["used_pct"] = json!(97);
        sick["updates"] = json!(12);
        let i = Inputs {
            now,
            since: now - Duration::days(1),
            server_health: Some(sick),
            machines: vec![json!({ "name": "laptop", "online": false })],
            board: Some(Board { at: now, overall: State::Down, rows: vec![row("embedding", "Embeddings", State::Down, Some(0.5)), row("chat", "Chat model", State::Up, Some(0.9))] }),
            runs,
            goals: vec![goal],
            ..Default::default()
        };
        let b = gather(&i);
        assert_eq!(b.attention, 4, "{b:#?}");
        assert_eq!(b.headline, "4 things need a look");
        let m = &b.sections[0];
        assert_eq!(m.name, "Machines");
        assert_eq!(m.items[0].level, Level::Attention);
        assert!(m.items.iter().any(|it| it.text == "server: 12 updates pending"), "{m:#?}");
        assert!(m.items.iter().any(|it| it.text.starts_with("laptop is offline")));
        let all = describe(&b);
        assert!(all.contains("⚠ backup check failed: Backup failed: disk full"), "{all}");
        assert!(all.contains("Chat model answered 90% of checks"));
        assert!(all.contains("Move the wiki is overdue (40% done)"));
        let push = push_body(&b);
        assert_eq!(push.lines().count(), 4, "three, then how many more: {push}");
    }

    #[test]
    fn tasks_from_pmi_lead() {
        let now = Utc::now();
        let today = now.with_timezone(&Local).date_naive();
        let pmi = crate::pmi::State {
            tasks: vec![
                json!({ "title": "Renew the cert", "where": "personal", "due": (today - Duration::days(2)).to_string() }),
                json!({ "title": "Order phones", "where": "project Phones", "due": today.to_string() }),
                json!({ "title": "Someday", "where": "personal", "due": null }),
            ],
            waiting: json!({ "task_transfers": [{ "task": "Budget review" }], "project_transfers": [], "approvals": [] }),
            projects: vec![json!({ "name": "Website", "health": "at_risk" }), json!({ "name": "Phones", "health": "on_track" })],
            inbox_unread: 2,
            ..Default::default()
        };
        let b = gather(&Inputs { now, since: now - Duration::days(1), pmi: Some(pmi), ..Default::default() });
        assert_eq!(b.sections[0].name, "Tasks");
        let texts: Vec<&str> = b.sections[0].items.iter().map(|i| i.text.as_str()).collect();
        assert!(texts[0].starts_with("overdue since") && texts[0].ends_with("Renew the cert"), "{texts:?}");
        assert!(texts.contains(&"a task is being handed to you: Budget review"));
        assert!(texts.contains(&"due today: Order phones (project Phones)"));
        assert!(texts.contains(&"project Website is at risk"));
        assert!(texts.contains(&"2 unread items in your PMI inbox"));
        assert_eq!(b.attention, 2);
    }

    #[test]
    fn once_a_day_even_across_restarts() {
        let now = Local::now();
        let schedule = "every day at 07:30";
        // Made this morning: the next is tomorrow.
        let made = now.date_naive().and_hms_opt(7, 30, 5).unwrap().and_local_timezone(Local).unwrap();
        let after = next(schedule, Some(made.with_timezone(&Utc)), made + Duration::minutes(1)).unwrap();
        assert_eq!(after.date_naive(), made.date_naive() + Duration::days(1));
        // Down at 7:30 and back at 8:00: made on start.
        let yesterday = made - Duration::days(1);
        let back = made + Duration::minutes(30);
        assert!(next(schedule, Some(yesterday.with_timezone(&Utc)), back).unwrap() <= back);
        // Back at 16:00: today's is skipped.
        let late = made + Duration::hours(8);
        assert!(next(schedule, Some(yesterday.with_timezone(&Utc)), late).unwrap() > late);
        let utc = made.with_timezone(&Utc);
        assert_eq!(window_start(None, utc), utc - Duration::days(1));
        assert_eq!(window_start(Some(utc - Duration::days(30)), utc), utc - Duration::days(7));
    }
}
