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

fn time_of_day(s: &str) -> Option<NaiveTime> {
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

pub fn dir() -> Option<PathBuf> {
    Some(crate::config::home()?.join("routines"))
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
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = toml::to_string_pretty(r).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{}.toml", r.name)), text).map_err(|e| e.to_string())
}

pub fn delete(name: &str) -> Result<Routine, String> {
    let r = find(name)?;
    let dir = dir().ok_or("no lyra home")?;
    std::fs::remove_file(dir.join(format!("{}.toml", r.name))).map_err(|e| e.to_string())?;
    let mut runs = runs();
    runs.remove(&r.name);
    save_runs(&runs);
    Ok(r)
}

/// A new routine (checked: name, schedule, prompt).
pub fn create(name: &str, schedule: &str, prompt: &str, notify: Notify, changes: bool) -> Result<Routine, String> {
    let name = slug(name);
    if name.is_empty() {
        return Err("a routine needs a name".into());
    }
    if find(&name).is_ok() {
        return Err(format!("there's already a routine called {name} (/routine edit or delete it)"));
    }
    let r = Routine { name, schedule: schedule.trim().into(), prompt: prompt.trim().into(), notify, enabled: true, changes, created: Utc::now() };
    save(&r)?;
    Ok(r)
}

static RUNS_LOCK: Mutex<()> = Mutex::new(());

pub fn runs() -> HashMap<String, Vec<Run>> {
    dir().and_then(|d| std::fs::read_to_string(d.join("runs.json")).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_runs(runs: &HashMap<String, Vec<Run>>) {
    if let Some(dir) = dir() {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(text) = serde_json::to_string_pretty(runs) {
            let _ = std::fs::write(dir.join("runs.json"), text);
        }
    }
}

/// Keep a finished run (newest first).
pub fn record(name: &str, run: Run) {
    let _guard = RUNS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = runs();
    let list = all.entry(name.to_string()).or_default();
    list.insert(0, run);
    list.truncate(KEEP_RUNS);
    save_runs(&all);
}

/// Every routine with its next run and recent runs, for the app (`running`:
/// the ones running now).
pub fn view(running: &[String], recent: usize) -> serde_json::Value {
    let runs = runs();
    serde_json::json!(list()
        .iter()
        .map(|r| {
            let mine = runs.get(&r.name).cloned().unwrap_or_default();
            serde_json::json!({
                "name": r.name, "schedule": r.schedule, "prompt": r.prompt, "notify": r.notify.as_str(), "enabled": r.enabled, "changes": r.changes,
                "valid": parse_schedule(&r.schedule).is_ok(),
                "next": if r.enabled { next_run(r, mine.first().map(|x| x.at)).map(|n| n.to_rfc3339()) } else { None },
                "running": running.contains(&r.name),
                "runs": mine.iter().take(recent).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>())
}

// ---- running (lyra serve)

/// Routines asked to run now (`/routine run`), for `lyra serve` to pick up.
static WANTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn request_run(name: &str) {
    WANTED.lock().unwrap_or_else(|e| e.into_inner()).push(name.to_string());
}

pub fn has_requests() -> bool {
    !WANTED.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
}

pub fn take_requests() -> Vec<String> {
    std::mem::take(&mut *WANTED.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Routines due now (enabled, schedule passed since their last run).
pub fn due(now: DateTime<Local>) -> Vec<Routine> {
    let runs = runs();
    list()
        .into_iter()
        .filter(|r| r.enabled && next_run(r, runs.get(&r.name).and_then(|x| x.first()).map(|x| x.at)).is_some_and(|n| n <= now))
        .collect()
}

/// The message a run sends: the prompt, marked as a routine so the reply is a report.
pub fn message(r: &Routine) -> String {
    format!(
        "[routine \"{}\", {}] {}\n\n(This runs on a schedule with nobody watching: do it now, then report briefly. Start with a one-line verdict: all clear, or what's wrong.{})",
        r.name,
        r.schedule,
        r.prompt,
        if r.changes {
            " Changes ask the user, who may not answer quickly."
        } else {
            " Only look: use read-only checks. Anything that would change something is refused, so say what you'd do instead."
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
    match crate::learn::complete(url, model, &system, &state) {
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
            let r = create(args["name"].as_str().unwrap_or(""), args["schedule"].as_str().unwrap_or(""), args["prompt"].as_str().unwrap_or(""), notify, args["changes"] == true)?;
            let next = next_run(&r, None).map(|n| n.format("%a %Y-%m-%d %H:%M").to_string());
            Ok(serde_json::json!({ "created": r.name, "schedule": r.schedule, "next_run": next, "notify": r.notify.as_str() }))
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
            format!("{} — {} · next {next} · {last} · notify {}{}\n    {}", r.name, r.schedule, r.notify.as_str(), if r.changes { " · may change things" } else { "" }, r.prompt)
        })
        .collect();
    out.push("/routine run|pause|resume|delete|show <name> · /routine edit <name> schedule|prompt|notify <value>".into());
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
