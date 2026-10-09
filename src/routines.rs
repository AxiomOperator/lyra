//! Routines: something to ask lyra on a schedule ("every day at 07:00:
//! check disks, updates and failed services on all machines"), each a TOML
//! file in `~/.lyra/routines/`. `lyra serve` runs a due routine in its own
//! conversation, then asks whether the result needs the user (the decision
//! model when there is one, else the chat model) and, with `notify =
//! "problems"`, pushes only when it does. Their runs are kept in `runs.json`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};

/// When a run tells the user.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Notify {
    /// Only when the result shows a problem or something to act on.
    #[default]
    Problems,
    Always,
    Never,
}

impl Notify {
    pub fn as_str(self) -> &'static str {
        match self {
            Notify::Problems => "problems",
            Notify::Always => "always",
            Notify::Never => "never",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim() {
            "problems" | "problem" | "issues" => Ok(Notify::Problems),
            "always" => Ok(Notify::Always),
            "never" | "off" => Ok(Notify::Never),
            other => Err(format!("notify is problems, always or never (not {other:?})")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Routine {
    pub name: String,
    /// "every day at 07:00", "weekdays at 8:30", "monday at 9:00", "every 30m", "every 6h", "hourly".
    pub schedule: String,
    /// What to ask lyra, as you'd type it.
    pub prompt: String,
    #[serde(default)]
    pub notify: Notify,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// It may change things (each change still asks the user). Off: it only
    /// looks, and anything needing approval is declined at once.
    #[serde(default)]
    pub changes: bool,
    /// Each run's result is emailed to its person (only them).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub email: bool,
    /// Each run sees its last results (so a daily brief doesn't repeat itself).
    #[serde(default = "yes")]
    pub remember: bool,
    pub created: DateTime<Utc>,
}

fn yes() -> bool {
    true
}

/// One run of a routine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub at: DateTime<Utc>,
    pub seconds: u64,
    /// The result shows something the user should look at.
    pub needs_user: bool,
    /// "ok", "error" or "stopped".
    pub outcome: String,
    /// The start of the reply.
    pub summary: String,
    /// Its conversation (`/resume <id>`).
    pub session: String,
    /// Who decided `needs_user`: the decision model or the chat model.
    #[serde(default)]
    pub decided_by: String,
    /// Emailed: how ("sent to … with Postmark"), or why not ("not emailed: …"); empty when it isn't emailed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub emailed: String,
    /// What it took: model calls, tokens in and out, and their cost (0 when no prices are set).
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub tokens_in: u64,
    #[serde(default)]
    pub tokens_out: u64,
    #[serde(default)]
    pub cost: f64,
}

/// Runs kept per routine.
const KEEP_RUNS: usize = 20;

// ---- schedules

/// A parsed schedule.
#[derive(Debug, Clone, PartialEq)]
pub enum Schedule {
    Every(Duration),
    /// At a time of day on some weekdays (all seven for "every day").
    At { time: NaiveTime, days: Vec<Weekday> },
}

const ALL_DAYS: [Weekday; 7] = [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri, Weekday::Sat, Weekday::Sun];

fn day(word: &str) -> Option<Weekday> {
    let w = word.trim_end_matches('s');
    ALL_DAYS.into_iter().find(|d| {
        let full = match d {
            Weekday::Mon => "monday",
            Weekday::Tue => "tuesday",
            Weekday::Wed => "wednesday",
            Weekday::Thu => "thursday",
            Weekday::Fri => "friday",
            Weekday::Sat => "saturday",
            Weekday::Sun => "sunday",
        };
        w == full || (w.len() >= 3 && full.starts_with(w))
    })
}

pub(crate) fn time_of_day(s: &str) -> Option<NaiveTime> {
    let s = s.trim().to_lowercase();
    let (s, pm, am) = if let Some(x) = s.strip_suffix("pm") {
        (x.trim().to_string(), true, false)
    } else if let Some(x) = s.strip_suffix("am") {
        (x.trim().to_string(), false, true)
    } else {
        (s, false, false)
    };
    let (h, m) = match s.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (s.parse::<u32>().ok()?, 0),
    };
    let h = match (pm, am, h) {
        (true, _, h) if h < 12 => h + 12,
        (_, true, 12) => 0,
        (_, _, h) => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// Read a schedule: "every day at 7:00", "daily at 07:00", "at 7am",
/// "weekdays at 8:30", "every monday and friday at 9", "every 30m", "every 6h",
/// "every 2d", "hourly".
pub fn parse_schedule(text: &str) -> Result<Schedule, String> {
    let t = text.trim().to_lowercase().replace(',', " ");
    let usage = || format!("can't read the schedule {text:?}: try \"every day at 07:00\", \"weekdays at 8:30\", \"monday at 9:00\", \"every 30m\" or \"every 6h\"");
    if t == "hourly" {
        return Ok(Schedule::Every(Duration::hours(1)));
    }
    let words: Vec<&str> = t.split_whitespace().filter(|w| !matches!(*w, "every" | "and" | "on")).collect();
    // "every 30m", "every 6 hours"
    if let Some(first) = words.first() {
        let joined = words.join("");
        let digits: String = joined.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() && !t.contains(" at ") && !first.contains(':') {
            let n: i64 = digits.parse().map_err(|_| usage())?;
            let unit = &joined[digits.len()..];
            let d = match unit {
                "m" | "min" | "mins" | "minute" | "minutes" => Duration::minutes(n),
                "h" | "hr" | "hrs" | "hour" | "hours" => Duration::hours(n),
                "d" | "day" | "days" => Duration::days(n),
                _ => return Err(usage()),
            };
            if d < Duration::minutes(5) {
                return Err("routines run at most every 5 minutes".into());
            }
            return Ok(Schedule::Every(d));
        }
    }
    let (when, at) = t.rsplit_once("at ").map_or((t.as_str(), ""), |(w, a)| (w, a));
    let time = time_of_day(at).ok_or_else(usage)?;
    let mut days = Vec::new();
    for w in when.split_whitespace().filter(|w| !matches!(*w, "every" | "and" | "on")) {
        match w {
            "day" | "days" | "daily" | "morning" | "evening" | "night" => days.extend(ALL_DAYS),
            "weekday" | "weekdays" => days.extend(&ALL_DAYS[..5]),
            "weekend" | "weekends" => days.extend(&ALL_DAYS[5..]),
            w => days.push(day(w).ok_or_else(usage)?),
        }
    }
    if days.is_empty() {
        days.extend(ALL_DAYS);
    }
    days.sort_by_key(Weekday::num_days_from_monday);
    days.dedup();
    Ok(Schedule::At { time, days })
}

/// The first time strictly after `after` the schedule fires.
pub fn next_after(s: &Schedule, after: DateTime<Local>) -> Option<DateTime<Local>> {
    match s {
        Schedule::Every(d) => Some(after + *d),
        Schedule::At { time, days } => (0..8).find_map(|i| {
            let date = after.date_naive() + Duration::days(i);
            let at = Local.from_local_datetime(&date.and_time(*time)).earliest()?;
            (at > after && days.contains(&date.weekday())).then_some(at)
        }),
    }
}

/// When a routine runs next: after its last run (or its creation).
pub fn next_run(r: &Routine, last: Option<DateTime<Utc>>) -> Option<DateTime<Local>> {
    let from = last.unwrap_or(r.created).with_timezone(&Local);
    next_after(&parse_schedule(&r.schedule).ok()?, from)
}

// ---- storage

/// Whose routines: the person this thread works for (the owner's are in
/// `~/.lyra/routines`, anyone else's in `~/.lyra/users/<id>/routines`).
pub fn dir() -> Option<PathBuf> {
    dir_for(&crate::acting::current())
}

pub fn dir_for(user: &str) -> Option<PathBuf> {
    if crate::acting::is_owner(user) { Some(crate::config::home()?.join("routines")) } else { Some(crate::context::user_dir(user)?.join("routines")) }
}

/// Everyone who has routines: the owner and anyone with a routines folder.
pub fn people() -> Vec<String> {
    let mut out = vec![lyra_web::users::OWNER.to_string()];
    let users = crate::config::home().map(|h| h.join("users"));
    for e in users.and_then(|d| std::fs::read_dir(d).ok()).into_iter().flatten().flatten() {
        if e.path().join("routines").is_dir() {
            out.push(e.file_name().to_string_lossy().to_string());
        }
    }
    out
}

/// A routine's file name: lowercase letters, digits and dashes.
pub fn slug(name: &str) -> String {
    let s: String = name.trim().to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-").chars().take(40).collect()
}

pub fn list() -> Vec<Routine> {
    let Some(dir) = dir() else { return Vec::new() };
    let mut all: Vec<Routine> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
        .filter_map(|e| toml::from_str(&std::fs::read_to_string(e.path()).ok()?).ok())
        .collect();
    all.sort_by(|a, b| a.name.cmp(&b.name));
    all
}

pub fn find(name: &str) -> Result<Routine, String> {
    let key = slug(name);
    list().into_iter().find(|r| r.name == key).ok_or_else(|| format!("no routine {name:?} (/routine lists them)"))
}

pub fn save(r: &Routine) -> Result<(), String> {
    parse_schedule(&r.schedule)?;
    if r.prompt.trim().is_empty() {
        return Err("a routine needs something to do".into());
    }
    let dir = dir().ok_or("no lyra home")?;
    let text = toml::to_string_pretty(r).map_err(|e| e.to_string())?;
    crate::store::write_text(&dir.join(format!("{}.toml", r.name)), &text)
}

/// From the Routines page: a new routine (no `original`) or changes to one,
/// the prompt kept as written (Markdown, lines and all).
pub fn put(arg: &serde_json::Value) -> Result<serde_json::Value, String> {
    let text = |k: &str| arg[k].as_str().map(|s| s.trim().to_string());
    let notify = text("notify").filter(|n| !n.is_empty()).map(|n| Notify::parse(&n)).transpose()?;
    let r = match text("original").filter(|o| !o.is_empty()) {
        None => create(&text("name").unwrap_or_default(), &text("schedule").unwrap_or_default(), &text("prompt").unwrap_or_default(), notify.unwrap_or_default(), arg["changes"] == true, arg["email"] == true)?,
        Some(original) => {
            let mut r = find(&original)?;
            if let Some(s) = text("schedule") {
                r.schedule = s;
            }
            if let Some(p) = text("prompt") {
                r.prompt = p;
            }
            if let Some(n) = notify {
                r.notify = n;
            }
            if let Some(c) = arg["changes"].as_bool() {
                r.changes = c;
            }
            if let Some(e) = arg["email"].as_bool() {
                r.email = e;
            }
            if let Some(m) = arg["remember"].as_bool() {
                r.remember = m;
            }
            save(&r)?;
            r
        }
    };
    Ok(serde_json::json!({ "ok": true, "name": r.name }))
}

pub fn delete(name: &str) -> Result<Routine, String> {
    let r = find(name)?;
    let dir = dir().ok_or("no lyra home")?;
    std::fs::remove_file(dir.join(format!("{}.toml", r.name))).map_err(|e| e.to_string())?;
    let _guard = RUNS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut runs = runs();
    runs.remove(&r.name);
    save_runs(&runs);
    Ok(r)
}

/// A new routine (checked: name, schedule, prompt).
pub fn create(name: &str, schedule: &str, prompt: &str, notify: Notify, changes: bool, email: bool) -> Result<Routine, String> {
    let name = slug(name);
    if name.is_empty() {
        return Err("a routine needs a name".into());
    }
    if find(&name).is_ok() {
        return Err(format!("there's already a routine called {name} (/routine edit or delete it)"));
    }
    let r = Routine { name, schedule: schedule.trim().into(), prompt: prompt.trim().into(), notify, enabled: true, changes, email, remember: true, created: Utc::now() };
    save(&r)?;
    Ok(r)
}

static RUNS_LOCK: Mutex<()> = Mutex::new(());

pub fn runs() -> HashMap<String, Vec<Run>> {
    let mut all: HashMap<String, Vec<Run>> = dir().map(|d| crate::store::read_json(&d.join("runs.json"))).unwrap_or_default();
    // "running" with nothing running it: lyra stopped (a restart) part way.
    let live: Vec<String> = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(_, _, r)| r.session.clone()).collect();
    for x in all.values_mut().flatten() {
        if x.outcome == "running" && !live.contains(&x.session) {
            x.outcome = "interrupted".into();
            x.summary = "lyra stopped (restarted) before this run finished".into();
        }
    }
    all
}

fn save_runs(runs: &HashMap<String, Vec<Run>>) {
    if let Some(dir) = dir() {
        let _ = crate::store::write_json(&dir.join("runs.json"), runs);
    }
}

/// Keep a finished run (newest first).
pub fn record(name: &str, run: Run) {
    let _guard = RUNS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = runs();
    let list = all.entry(name.to_string()).or_default();
    // The finished run takes the place of its "running" entry (read as
    // "interrupted" once it's no longer running, a moment before this).
    list.retain(|x| !(x.session == run.session && x.seconds == 0 && matches!(x.outcome.as_str(), "running" | "interrupted")));
    list.insert(0, run);
    list.truncate(KEEP_RUNS);
    save_runs(&all);
}

/// Every routine with its next run and recent runs, for the app (with the
/// one running now: since when, what it's doing, its conversation).
pub fn view(recent: usize) -> serde_json::Value {
    let user = crate::acting::current();
    let now_running = running_for(&user);
    let running: Vec<String> = now_running.iter().map(|(n, _)| n.clone()).collect();
    let runs = runs();
    serde_json::json!(list()
        .iter()
        .map(|r| {
            let mine = runs.get(&r.name).cloned().unwrap_or_default();
            serde_json::json!({
                "name": r.name, "schedule": r.schedule, "prompt": r.prompt, "notify": r.notify.as_str(), "enabled": r.enabled, "changes": r.changes, "email": r.email, "remember": r.remember, "results": results(&r.name).len(),
                "valid": parse_schedule(&r.schedule).is_ok(),
                "next": if r.enabled { next_run(r, mine.first().map(|x| x.at)).map(|n| n.to_rfc3339()) } else { None },
                "running": running.contains(&r.name),
                "run_now": now_running.iter().find(|(n, _)| *n == r.name).map(|(_, x)| serde_json::json!({ "started": x.started, "session": x.session, "doing": x.doing })),
                "runs": mine.iter().take(recent).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>())
}

// ---- running (lyra serve)

/// A run in progress: since when, its conversation, what it's doing now.
#[derive(Debug, Clone, PartialEq)]
pub struct Running {
    pub started: DateTime<Utc>,
    pub session: String,
    pub doing: String,
}

/// Runs in progress, whose and which (lost with a restart: their runs.json entry then says so).
static RUNNING: Mutex<Vec<(String, String, Running)>> = Mutex::new(Vec::new());
/// Moves when a run starts, ends or does something else: the app's page looks again.
static RUNNING_REV: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn running_rev() -> u64 {
    RUNNING_REV.load(std::sync::atomic::Ordering::Relaxed)
}

fn bump() {
    RUNNING_REV.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// A run started: kept here, and in runs.json as "running" (so a restart
/// leaves "interrupted", not nothing).
pub fn running_start(user: &str, name: &str, session: &str) {
    let r = Running { started: Utc::now(), session: session.to_string(), doing: "starting".into() };
    let mut all = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|(u, n, _)| !(u == user && n == name));
    all.push((user.to_string(), name.to_string(), r));
    drop(all);
    bump();
    crate::acting::run(user, || record(name, Run { at: Utc::now(), seconds: 0, needs_user: false, outcome: "running".into(), summary: String::new(), session: session.to_string(), decided_by: String::new(), emailed: String::new(), calls: 0, tokens_in: 0, tokens_out: 0, cost: 0.0 }));
}

/// What it's doing now ("searching the web", "Coder: writing…").
pub fn running_doing(user: &str, name: &str, doing: &str) {
    if let Some((_, _, r)) = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).iter_mut().find(|(u, n, _)| u == user && n == name)
        && r.doing != doing
    {
        r.doing = doing.to_string();
        bump();
    }
}

pub fn running_end(user: &str, name: &str) {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner()).retain(|(u, n, _)| !(u == user && n == name));
    bump();
}

/// This person's runs in progress.
pub fn running_for(user: &str) -> Vec<(String, Running)> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(u, _, _)| u == user).map(|(_, n, r)| (n.clone(), r.clone())).collect()
}

/// Runs a stop cut off (lyra restarting), to start again when it's back.
fn resume_path() -> Option<PathBuf> {
    Some(crate::config::home()?.join("status").join("resume.json"))
}

pub fn save_resume(runs: &[(String, String)]) {
    if let Some(p) = resume_path() {
        let _ = crate::store::write_json(&p, &runs.to_vec());
    }
}

/// The runs to start again (once: the list goes).
pub fn take_resume() -> Vec<(String, String)> {
    let Some(p) = resume_path() else { return Vec::new() };
    let runs: Vec<(String, String)> = crate::store::read_json::<Option<Vec<(String, String)>>>(&p).unwrap_or_default();
    let _ = std::fs::remove_file(&p);
    runs
}

/// Routines asked to run now (`/routine run`), whose and which, for `lyra serve` to pick up.
static WANTED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

pub fn request_run(name: &str) {
    WANTED.lock().unwrap_or_else(|e| e.into_inner()).push((crate::acting::current(), name.to_string()));
}

pub fn has_requests() -> bool {
    !WANTED.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
}

/// Ask for a run as someone (a run a restart cut off).
pub fn request_run_for(user: &str, name: &str) {
    WANTED.lock().unwrap_or_else(|e| e.into_inner()).push((user.to_string(), name.to_string()));
}

pub fn take_requests() -> Vec<(String, String)> {
    std::mem::take(&mut *WANTED.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Everyone's routines due now: (whose, which).
pub fn due_all(now: DateTime<Local>) -> Vec<(String, Routine)> {
    people().into_iter().flat_map(|p| crate::acting::run(&p, || due(now)).into_iter().map(move |r| (p.clone(), r))).collect()
}

/// Routines due now (enabled, schedule passed since their last run).
pub fn due(now: DateTime<Local>) -> Vec<Routine> {
    let runs = runs();
    list()
        .into_iter()
        .filter(|r| r.enabled && next_run(r, runs.get(&r.name).and_then(|x| x.first()).map(|x| x.at)).is_some_and(|n| n <= now))
        .collect()
}

/// The message a run sends: the prompt, marked as a routine so the reply is a
/// report, with its last results when it remembers them.
pub fn message(r: &Routine) -> String {
    let mut text = base_message(r);
    if r.remember {
        let past: Vec<(String, String)> = results(&r.name).into_iter().take(REMEMBER).filter_map(|(at, _)| result(&r.name, &at).ok().map(|t| (at, t))).collect();
        if !past.is_empty() {
            text += "\n\nYour previous results, newest first: build on them. Don't repeat what they already covered unless something new happened; say what changed since.";
            for (at, t) in past {
                let when = parse_stamp(&at).map_or(at.clone(), |d| d.with_timezone(&Local).format("%a %Y-%m-%d %H:%M").to_string());
                let cut: String = t.chars().take(PAST_CHARS).collect();
                text += &format!("\n\n--- result from {when} ---\n{cut}{}", if t.chars().count() > PAST_CHARS { "\n(…cut)" } else { "" });
            }
        }
    }
    text
}

/// How many past results a run sees, and how much of each.
const REMEMBER: usize = 2;
const PAST_CHARS: usize = 8_000;
/// Results kept per routine.
const KEEP_RESULTS: usize = 90;

fn results_dir(name: &str) -> Option<PathBuf> {
    Some(dir()?.join("results").join(slug(name)))
}

fn parse_stamp(at: &str) -> Option<DateTime<Utc>> {
    chrono::NaiveDateTime::parse_from_str(at, "%Y%m%d-%H%M%S").ok().map(|n| n.and_utc())
}

/// Keep a run's whole result (the oldest past `KEEP_RESULTS` go).
pub fn save_result(name: &str, at: DateTime<Utc>, text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Ok(());
    }
    let d = results_dir(name).ok_or("no lyra home")?;
    crate::store::write_text(&d.join(format!("{}.md", at.format("%Y%m%d-%H%M%S"))), text)?;
    for (old, _) in results(name).into_iter().skip(KEEP_RESULTS) {
        let _ = std::fs::remove_file(d.join(format!("{old}.md")));
    }
    Ok(())
}

/// A routine's kept results, newest first: (stamp, size).
pub fn results(name: &str) -> Vec<(String, u64)> {
    let Some(d) = results_dir(name) else { return Vec::new() };
    let mut out: Vec<(String, u64)> = std::fs::read_dir(d)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            let stamp = n.strip_suffix(".md")?.to_string();
            parse_stamp(&stamp)?;
            Some((stamp, e.metadata().map(|m| m.len()).unwrap_or(0)))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

pub fn result(name: &str, at: &str) -> Result<String, String> {
    parse_stamp(at).ok_or("no such result")?;
    std::fs::read_to_string(results_dir(name).ok_or("no lyra home")?.join(format!("{at}.md"))).map_err(|_| format!("no result {at} for {name}"))
}

/// For the app's past results: every routine's (or one's), newest first,
/// only those with every word of `query`.
pub fn results_page(only: Option<&str>, query: &str) -> serde_json::Value {
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_string).collect();
    let mut all: Vec<(String, String, String, String)> = Vec::new();
    for r in list().iter().filter(|r| only.is_none_or(|o| slug(o) == r.name)) {
        for (at, _) in results(&r.name) {
            let Ok(text) = result(&r.name, &at) else { continue };
            let lower = text.to_lowercase();
            if !words.iter().all(|w| lower.contains(w.as_str())) {
                continue;
            }
            let title = text.lines().map(|l| l.trim().trim_start_matches('#').trim().trim_matches('*').trim()).find(|l| !l.is_empty()).unwrap_or("").chars().take(120).collect();
            // Where a search word is: a little around it.
            let around = words.first().and_then(|w| lower.find(w.as_str())).map(|i| {
                let before: Vec<usize> = text.char_indices().map(|(j, _)| j).take_while(|j| *j <= i).collect();
                let start = before.len().checked_sub(81).map_or(0, |k| before[k]);
                text[start..].chars().take(220).collect::<String>().replace('\n', " ")
            });
            let preview = around.unwrap_or_else(|| text.chars().take(220).collect::<String>().replace('\n', " "));
            all.push((at, r.name.clone(), title, preview));
        }
    }
    all.sort_by(|a, b| b.0.cmp(&a.0));
    serde_json::json!(all.iter().take(200).map(|(at, name, title, preview)| serde_json::json!({
        "name": name, "at": at, "when": parse_stamp(at).map(|d| d.to_rfc3339()), "title": title, "preview": preview,
    })).collect::<Vec<_>>())
}

fn base_message(r: &Routine) -> String {
    format!(
        "[routine \"{}\", {}] {}\n\n(This runs on a schedule with nobody watching: do it now, then report briefly. Start with a one-line verdict: all clear, or what's wrong.{}{})",
        r.name,
        r.schedule,
        r.prompt,
        if r.changes {
            " Changes ask the user, who may not answer quickly."
        } else {
            " Only look: use read-only checks. Anything that would change something is refused, so say what you'd do instead."
        },
        if r.email {
            " Your answer is emailed to the user as it is (don't call email_me): write it as the email, in Markdown, a short summary first, then the details, every source as a [title](url) link."
        } else {
            ""
        }
    )
}

/// The question whether a result needs the user.
pub const NEEDS_USER: &str = "Does this scheduled report show a problem, a failure, a warning or something the user must act on? \
     An all-clear report with nothing to do is not.";

/// Does a run's result need the user? (true/false, and who decided).
pub fn needs_user(url: &str, model: &str, r: &Routine, reply: &str) -> (bool, String) {
    let state = format!("Routine: {}\nAsked: {}\n\nReport:\n{reply}", r.name, r.prompt);
    if let Some((yes, _)) = crate::decide::yes("routine result", &state, NEEDS_USER) {
        return (yes, crate::decide::model().unwrap_or_else(|| "decision model".into()));
    }
    let system = format!("{NEEDS_USER} Answer with only yes or no.");
    match crate::learn::complete_light(url, model, &system, &state) {
        Ok((text, _)) => {
            let text = text.rsplit_once("</think>").map_or(text.as_str(), |(_, a)| a).trim().to_lowercase();
            (text.starts_with("yes"), "chat model".into())
        }
        // Can't tell: better to tell the user.
        Err(_) => (true, "unsure".into()),
    }
}

// ---- tools: "every morning at 7, check …" in plain words

/// `routine_create` (asks the user first) and `routine_list`.
pub fn capabilities() -> Vec<lyra_capabilities::Capability> {
    use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
    let tool = |name: &str, description: &str, risk: RiskLevel, parameters: serde_json::Value| {
        let mut c = Capability::new(name, CapabilityKind::NativeTool, description, risk);
        c.input_schema = parameters;
        c.source = "routines".into();
        c.tags = vec!["routine".into(), "schedule".into(), "recurring".into(), "every".into()];
        c
    };
    let mut create = tool(
        "routine_create",
        "Schedule something for lyra to do regularly (\"every morning at 7, check disks on all machines\"). It runs by itself in lyra serve; the user gets a notification only when a run finds something that needs them (notify=problems), always, or never. The user approves it first.",
        RiskLevel::LowWrite,
        serde_json::json!({ "type": "object", "properties": {
            "name": { "type": "string", "description": "Short name, e.g. morning-check." },
            "schedule": { "type": "string", "description": "When: \"every day at 07:00\", \"weekdays at 8:30\", \"monday at 9:00\", \"every 30m\", \"every 6h\"." },
            "prompt": { "type": "string", "description": "What to do each time, as the user would ask it (mention machines with @name or @all)." },
            "notify": { "type": "string", "enum": ["problems", "always", "never"], "description": "When to tell the user (default problems)." },
            "changes": { "type": "boolean", "description": "It must change things (restart, clean up, update), each change asked of the user. Default false: it only checks and reports." },
            "email": { "type": "boolean", "description": "Email each result to the user (only them): a morning digest, a summary with links. Default false." },
        }, "required": ["name", "schedule", "prompt"] }),
    );
    create.metadata.requires_approval = true;
    vec![
        create,
        tool("routine_list", "The routines lyra runs on a schedule, with their next and last runs.", RiskLevel::ReadOnly, serde_json::json!({ "type": "object", "properties": {} })),
    ]
}

pub fn call(name: &str, args: &serde_json::Value) -> Result<serde_json::Value, String> {
    match name {
        "routine_create" => {
            let notify = args["notify"].as_str().map_or(Ok(Notify::Problems), Notify::parse)?;
            let r = create(args["name"].as_str().unwrap_or(""), args["schedule"].as_str().unwrap_or(""), args["prompt"].as_str().unwrap_or(""), notify, args["changes"] == true, args["email"] == true)?;
            let next = next_run(&r, None).map(|n| n.format("%a %Y-%m-%d %H:%M").to_string());
            Ok(serde_json::json!({ "created": r.name, "schedule": r.schedule, "next_run": next, "notify": r.notify.as_str(), "email": r.email.then(|| crate::mailout::address(&crate::acting::current()).unwrap_or_else(|| "no address on their account yet: an admin adds one (Users)".into())) }))
        }
        "routine_list" => Ok(serde_json::json!({ "routines": describe() })),
        other => Err(format!("{other} isn't a routine tool")),
    }
}

/// `/routine` list.
pub fn describe() -> String {
    let all = list();
    if all.is_empty() {
        return "no routines yet. /routine new <name> | <schedule> | <what to do>   e.g.\n/routine new morning-check | every day at 07:00 | check disk, updates and failed services on all machines\n(or just ask lyra: \"every morning at 7, check …\")".into();
    }
    let runs = runs();
    let mut out: Vec<String> = all
        .iter()
        .map(|r| {
            let last = runs.get(&r.name).and_then(|x| x.first());
            let next = if r.enabled { next_run(r, last.map(|l| l.at)).map_or("?".into(), |n| n.format("%a %m-%d %H:%M").to_string()) } else { "paused".into() };
            let last = last.map_or("never run".into(), |l| {
                format!("last {} {}", l.at.with_timezone(&Local).format("%m-%d %H:%M"), if l.outcome != "ok" { l.outcome.as_str() } else if l.needs_user { "⚠ needs you" } else { "all clear" })
            });
            format!("{} — {} · next {next} · {last} · notify {}{}{}\n    {}", r.name, r.schedule, r.notify.as_str(), if r.changes { " · may change things" } else { "" }, if r.email { " · emailed to you" } else { "" }, r.prompt)
        })
        .collect();
    out.push("/routine run|pause|resume|delete|show <name> · /routine edit <name> schedule|prompt|notify|changes|email <value>".into());
    out.join("\n")
}

/// `/routine show <name>`: its runs.
pub fn show(name: &str) -> Result<String, String> {
    let r = find(name)?;
    let mut out = vec![format!("{} — {} · notify {}{}\n  {}", r.name, r.schedule, r.notify.as_str(), if r.enabled { "" } else { " · paused" }, r.prompt)];
    let runs = runs().remove(&r.name).unwrap_or_default();
    if runs.is_empty() {
        out.push("  never run".into());
    }
    for x in runs.iter().take(10) {
        out.push(format!(
            "  {} {} ({}s, {}): {}  /resume {}",
            x.at.with_timezone(&Local).format("%m-%d %H:%M"),
            if x.outcome != "ok" { x.outcome.as_str() } else if x.needs_user { "⚠" } else { "✓" },
            x.seconds,
            x.decided_by,
            x.summary.lines().next().unwrap_or(""),
            x.session
        ));
    }
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_persons_routines_are_their_own() {
        let owner = dir().unwrap();
        assert!(owner.ends_with("routines") && !owner.to_string_lossy().contains("/users/"));
        let dana = crate::acting::run("oid-dana", dir).unwrap();
        assert!(dana.ends_with("users/oid-dana/routines"), "{}", dana.display());
        assert_eq!(dir().unwrap(), owner, "back to the owner afterwards");
        crate::acting::run("oid-dana", || request_run("standup"));
        assert!(take_requests().contains(&("oid-dana".to_string(), "standup".to_string())), "a request remembers whose");
    }

    #[test]
    fn schedules_read_like_people_write_them() {
        let at = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        assert_eq!(parse_schedule("every day at 07:00").unwrap(), Schedule::At { time: at(7, 0), days: ALL_DAYS.to_vec() });
        assert_eq!(parse_schedule("every morning at 7").unwrap(), Schedule::At { time: at(7, 0), days: ALL_DAYS.to_vec() });
        assert_eq!(parse_schedule("weekdays at 8:30").unwrap(), Schedule::At { time: at(8, 30), days: ALL_DAYS[..5].to_vec() });
        assert_eq!(parse_schedule("every Monday and Friday at 9pm").unwrap(), Schedule::At { time: at(21, 0), days: vec![Weekday::Mon, Weekday::Fri] });
        assert_eq!(parse_schedule("at 6:15am").unwrap(), Schedule::At { time: at(6, 15), days: ALL_DAYS.to_vec() });
        assert_eq!(parse_schedule("every 30m").unwrap(), Schedule::Every(Duration::minutes(30)));
        assert_eq!(parse_schedule("every 6 hours").unwrap(), Schedule::Every(Duration::hours(6)));
        assert_eq!(parse_schedule("hourly").unwrap(), Schedule::Every(Duration::hours(1)));
        assert!(parse_schedule("every 1m").is_err(), "not that often");
        assert!(parse_schedule("whenever").is_err());
        assert!(parse_schedule("every blursday at 7").is_err());
    }

    #[test]
    fn the_next_run_follows_the_schedule() {
        let t = |d, h, m| Local.with_ymd_and_hms(2026, 10, d, h, m, 0).unwrap(); // 2026-10-05 is a Monday
        let daily = parse_schedule("every day at 07:00").unwrap();
        assert_eq!(next_after(&daily, t(6, 6, 0)), Some(t(6, 7, 0)));
        assert_eq!(next_after(&daily, t(6, 7, 0)), Some(t(7, 7, 0)), "strictly after");
        let weekdays = parse_schedule("weekdays at 8:30").unwrap();
        assert_eq!(next_after(&weekdays, t(9, 9, 0)), Some(t(12, 8, 30)), "Friday after 8:30 → Monday");
        assert_eq!(next_after(&parse_schedule("every 2h").unwrap(), t(6, 7, 0)), Some(t(6, 9, 0)));
    }

    #[test]
    fn names_become_file_names() {
        assert_eq!(slug("Morning check!"), "morning-check");
        assert_eq!(slug("  ../etc/passwd "), "etc-passwd");
        assert_eq!(Notify::parse("always").unwrap(), Notify::Always);
        assert!(Notify::parse("sometimes").is_err());
    }
}
