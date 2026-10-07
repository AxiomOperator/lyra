//! Coding work handed to a harness (`[coding]`): Claude Code or OpenCode, run
//! headless in a project folder on a machine that has them (through its
//! lyra-node) or on the server. Simple work goes to OpenCode and complex work
//! to Claude Code — the decision model (or the chat model) rates the task —
//! and when OpenCode can't finish, Claude Code takes over with what it tried.
//! Naming one ("with Claude Code") wins. One approval starts a job; inside it
//! the harness runs on its own (full auto) but never pushes. Jobs are kept in
//! `~/.lyra/coding/jobs.json`, so "continue" resumes the last session.

use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use chrono::{DateTime, Utc};
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use lyra_node::coding::Harness;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::caps::{Ask, Caps, HERE};

/// `[coding]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// The harness for simple work, for complex work, and the one that takes over.
    pub simple: String,
    pub complex: String,
    pub fallback: String,
    pub timeout_minutes: u64,
    /// Let harnesses `git push` (off: they never do).
    pub allow_push: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: true, simple: "opencode".into(), complex: "claude".into(), fallback: "claude".into(), timeout_minutes: 30, allow_push: false }
    }
}

static SETTINGS: std::sync::RwLock<Option<Settings>> = std::sync::RwLock::new(None);

pub fn configure(s: Settings) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

pub fn settings() -> Settings {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

pub fn capability() -> Capability {
    let mut c = Capability::new(
        "code_task",
        CapabilityKind::NativeTool,
        "Hand coding work in a project folder to a coding agent (Claude Code or OpenCode) on a machine: fix a bug, \
         implement a feature, refactor, write tests. It works on its own and reports what it changed (files, diff, \
         commits). Leave harness empty to let lyra pick (OpenCode for simple work, Claude Code for complex, Claude \
         Code taking over if OpenCode can't finish); name one if the user did. continue=true resumes the last job \
         in that folder.",
        RiskLevel::Destructive,
    );
    c.input_schema = json!({ "type": "object", "properties": {
        "dir": { "type": "string", "description": "The project folder on that machine, e.g. ~/Projects/lyra." },
        "task": { "type": "string", "description": "What to do, self-contained: the goal, what done looks like, constraints." },
        "machine": { "type": "string", "description": "Only when the user named a machine (\"desktop\"); otherwise leave it empty: work runs on the server." },
        "harness": { "type": "string", "enum": ["", "claude", "opencode"], "description": "Only when the user named one." },
        "mode": { "type": "string", "enum": ["edit", "plan"], "description": "edit (default) changes the code; plan only reads and proposes." },
        "continue": { "type": "boolean", "description": "Continue the last coding job in this folder (same session)." },
    }, "required": ["dir", "task"] });
    c.source = "coding".into();
    c.tags = vec!["code".into(), "coding".into(), "programming".into(), "claude".into(), "opencode".into(), "refactor".into(), "bug".into()];
    c.permissions = vec!["coding.run".into()];
    c.metadata.requires_approval = true;
    c
}

/// What the user approves.
pub fn approval(args: &Value) -> Ask {
    let plan = args["mode"] == "plan";
    let who = Harness::parse(args["harness"].as_str().unwrap_or("")).map_or("a coding agent (OpenCode for simple work, Claude Code for complex)".to_string(), |h| h.title().to_string());
    let machine = args["machine"].as_str().filter(|m| !m.is_empty()).unwrap_or(HERE);
    // Shown as "<agent> asked to <what>".
    Ask {
        what: format!("have {who} {} {} on {machine}", if plan { "read and plan in" } else { "work unattended in" }, args["dir"].as_str().unwrap_or("?")),
        detail: args["task"].as_str().unwrap_or("").chars().take(500).collect(),
        why: if plan { "plan only: it reads and proposes, changing nothing".into() } else { "full auto: it edits files and runs commands in that project on its own (never pushes)".into() },
        dangerous: !plan,
    }
}

// ---- choosing a harness

/// A harness the request names ("with Claude Code", "use opencode").
pub fn named(text: &str) -> Option<Harness> {
    let t = text.to_lowercase();
    if t.contains("claude code") || t.contains("with claude") || t.contains("use claude") {
        Some(Harness::Claude)
    } else if t.contains("opencode") || t.contains("open code") {
        Some(Harness::OpenCode)
    } else {
        None
    }
}

/// A request that's coding work in a project (routed to the Coder even with
/// an @machine in it, which otherwise goes to the Operator).
pub fn looks_like_code(text: &str) -> bool {
    let t = format!(" {} ", text.to_lowercase());
    named(text).is_some()
        || [
            " fix the bug", " failing test", " fix the test", " refactor", " implement ", " write tests", " add tests", " unit test",
            " compile error", " doesn't compile", " build fails", " pull request", " code review", " in the code", " the codebase",
            " add a feature", " new feature", " fix the typo in",
        ]
        .iter()
        .any(|k| t.contains(k))
}

const SIMPLE: &str = "a single-file edit, a rename, a small fix with a clear cause, docs, formatting, a test for existing code";
const COMPLEX: &str = "a multi-file or architectural change, debugging something unknown, a new feature across modules, anything security-related";

/// Simple or complex, and who said so.
pub fn rate(task: &str, llm: Option<(&str, &str)>) -> (bool, String) {
    use crate::decide::{Question, ask, confident};
    let options = vec![("simple".to_string(), SIMPLE.to_string()), ("complex".to_string(), COMPLEX.to_string())];
    if let Some(answers) = ask("coding task size", task, &[("size".into(), Question::Choice("How big is this coding task?".into(), options))])
        && let Some(a) = confident(&answers, "size")
    {
        return (a.choice == "complex", format!("rated {} by the decision model", a.choice));
    }
    if let Some((url, model)) = llm {
        let system = format!("Rate a coding task. Answer with one word: simple or complex.\nsimple: {SIMPLE}.\ncomplex: {COMPLEX}.");
        if let Ok((text, _)) = crate::learn::complete(url, model, &system, task) {
            let t = text.rsplit_once("</think>").map_or(text.as_str(), |(_, a)| a).trim().to_lowercase();
            if t.starts_with("simple") || t.starts_with("complex") {
                let complex = t.starts_with("complex");
                return (complex, format!("rated {} by the chat model", if complex { "complex" } else { "simple" }));
            }
        }
    }
    (true, "couldn't rate it, so treated as complex".into())
}

/// The result shows it didn't get there.
pub fn gave_up(result: &Value) -> bool {
    if result["ok"] != true {
        return true;
    }
    let s = result["summary"].as_str().unwrap_or("").to_lowercase();
    ["couldn't", "could not", "unable to", "wasn't able", "was not able", "i can't", "cannot complete", "didn't manage", "still fail"].iter().any(|p| s.contains(p))
}

// ---- job records

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub at: DateTime<Utc>,
    pub machine: String,
    pub dir: String,
    pub harness: String,
    pub why: String,
    pub task: String,
    pub session: Option<String>,
    pub ok: bool,
    pub summary: String,
    pub files: Vec<String>,
    pub diff_stat: String,
    #[serde(default)]
    pub handed_over: bool,
}

static LOCK: Mutex<()> = Mutex::new(());

fn jobs_path() -> Option<std::path::PathBuf> {
    Some(crate::config::home()?.join("coding").join("jobs.json"))
}

pub fn jobs() -> Vec<Record> {
    jobs_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn keep(r: Record) {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(path) = jobs_path() else { return };
    let mut all = jobs();
    all.insert(0, r);
    all.truncate(100);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&all) {
        let _ = std::fs::write(path, text);
    }
}

/// The last job in a folder on a machine (to continue it).
pub fn last_in(machine: &str, dir: &str) -> Option<Record> {
    let dir = dir.trim_end_matches('/');
    jobs().into_iter().find(|r| r.machine.eq_ignore_ascii_case(machine) && r.dir.trim_end_matches('/') == dir && r.session.is_some())
}

/// `/coding`.
pub fn describe() -> String {
    let all = jobs();
    if all.is_empty() {
        return "no coding jobs yet — ask lyra to fix or build something in a project (\"fix the failing test in ~/Projects/foo on @desktop\")".into();
    }
    all.iter()
        .take(15)
        .map(|r| {
            format!(
                "{} {} {} on {} · {}{}\n    {} → {}",
                r.at.with_timezone(&chrono::Local).format("%m-%d %H:%M"),
                if r.ok { "✓" } else { "✗" },
                r.harness,
                r.machine,
                r.dir,
                if r.handed_over { " (took over from OpenCode)" } else { "" },
                r.task.lines().next().unwrap_or("").chars().take(80).collect::<String>(),
                if r.files.is_empty() { r.summary.lines().next().unwrap_or("").chars().take(100).collect::<String>() } else { format!("{} files · {}", r.files.len(), r.diff_stat) }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---- running

/// Which machine runs a harness: the one asked for, else one that has it.
fn place(caps: &Caps, asked: Option<&str>, harness: Harness) -> Result<String, String> {
    let remote = caps.remote();
    let have = remote.as_ref().map(|r| r.harnesses()).unwrap_or_default();
    let here = harness.program().is_some();
    match asked.filter(|m| !m.is_empty()) {
        Some(m) if m.eq_ignore_ascii_case(HERE) => {
            if here { Ok(HERE.into()) } else { Err(format!("{} isn't installed on the server", harness.title())) }
        }
        Some(m) => match have.iter().find(|(n, _)| n.eq_ignore_ascii_case(m)) {
            Some((n, h)) if !h[harness.id()].is_null() => Ok(n.clone()),
            Some((n, _)) => Err(format!("{} isn't installed on {n}", harness.title())),
            None => Err(format!("{m} isn't connected, or has no coding agents")),
        },
        // Nothing runs anywhere but the server unless the user names the machine.
        None => {
            if here {
                Ok(HERE.into())
            } else {
                let elsewhere: Vec<&str> = have.iter().filter(|(_, h)| !h[harness.id()].is_null()).map(|(n, _)| n.as_str()).collect();
                Err(format!(
                    "{} isn't installed on the server{}",
                    harness.title(),
                    if elsewhere.is_empty() { String::new() } else { format!("; it is on {} — say \"on @{}\" to run it there", elsewhere.join(", "), elsewhere[0]) }
                ))
            }
        }
    }
}

fn run_one(caps: &Caps, machine: &str, request: Value, cancel: &AtomicBool, progress: &dyn Fn(Value)) -> Result<Value, String> {
    let timeout = Duration::from_secs(settings().timeout_minutes.clamp(1, 240) * 60 + 60);
    if machine == HERE {
        let job = lyra_node::coding::Job::from_request(&request)?;
        return lyra_node::coding::run(&job, cancel, progress);
    }
    let remote = caps.remote().ok_or("other machines connect to lyra serve")?;
    remote.call_streaming(machine, request, timeout, cancel, progress)
}

/// Do a coding task: pick the harness (or take the named one), run it, hand
/// over to the fallback if it couldn't finish, keep the record.
pub fn run(caps: &Caps, args: &Value, cancel: &AtomicBool, progress: &dyn Fn(Value), llm: Option<(&str, &str)>) -> Value {
    let s = settings();
    if !s.enabled {
        return json!({ "error": "coding is off ([coding] enabled)" });
    }
    let (Some(dir), Some(task)) = (args["dir"].as_str().filter(|d| !d.trim().is_empty()), args["task"].as_str().filter(|t| !t.trim().is_empty())) else {
        return json!({ "error": "dir and task are required" });
    };
    let asked_machine = args["machine"].as_str();
    let plan = args["mode"] == "plan";
    // Continue: the same harness and session as the last job in that folder.
    let previous = if args["continue"] == true { asked_machine.and_then(|m| last_in(m, dir)).or_else(|| jobs().into_iter().find(|r| r.dir.trim_end_matches('/') == dir.trim_end_matches('/') && r.session.is_some())) } else { None };
    let (harness, why) = if let Some(h) = Harness::parse(args["harness"].as_str().unwrap_or("")).or_else(|| named(task)) {
        (h, "named in the request".to_string())
    } else if let Some(p) = &previous
        && let Some(h) = Harness::parse(&p.harness)
    {
        (h, "continuing the last job".to_string())
    } else {
        let (complex, why) = rate(task, llm);
        let pick = if complex { &s.complex } else { &s.simple };
        (Harness::parse(pick).unwrap_or(Harness::Claude), why)
    };
    let machine = match place(caps, asked_machine.or(previous.as_ref().map(|p| p.machine.as_str())), harness) {
        Ok(m) => m,
        // The preferred one isn't there: the other one, if it is.
        Err(e) => {
            let other = if harness == Harness::Claude { Harness::OpenCode } else { Harness::Claude };
            match place(caps, asked_machine, other) {
                Ok(_) if args["harness"].as_str().is_some_and(|h| !h.is_empty()) => return json!({ "error": e }),
                Ok(_) => return run(caps, &{ let mut a = args.clone(); a["harness"] = json!(other.id()); a }, cancel, progress, llm),
                Err(_) => return json!({ "error": e }),
            }
        }
    };
    progress(json!({ "kind": "harness", "text": format!("{} on {machine} · {why}", harness.title()) }));
    let request = |h: Harness, task: &str, session: Option<&str>| {
        json!({
            "type": "code", "harness": h.id(), "dir": dir, "task": task, "mode": if plan { "plan" } else { "edit" },
            "session": session, "allow_push": s.allow_push, "timeout_minutes": s.timeout_minutes,
        })
    };
    let session = previous.as_ref().filter(|p| p.harness == harness.id()).and_then(|p| p.session.clone());
    let mut result = run_one(caps, &machine, request(harness, task, session.as_deref()), cancel, progress).unwrap_or_else(|e| json!({ "ok": false, "error": e, "harness": harness.id() }));
    let mut handed_over = false;
    // OpenCode couldn't finish: the fallback continues from where it stopped.
    let fallback = Harness::parse(&s.fallback);
    if gave_up(&result)
        && !plan
        && !cancel.load(std::sync::atomic::Ordering::SeqCst)
        && let Some(fb) = fallback.filter(|f| *f != harness)
        && args["harness"].as_str().is_none_or(str::is_empty)
        && named(task).is_none()
        && place(caps, Some(&machine), fb).is_ok()
    {
        progress(json!({ "kind": "handover", "text": format!("{} couldn't finish → handed to {}", harness.title(), fb.title()) }));
        let tried = format!(
            "{task}\n\n{} tried this first and couldn't finish{}. What it said: {}\nIts changes so far: {}\nContinue from the files as they are now.",
            harness.title(),
            result["error"].as_str().map_or(String::new(), |e| format!(" ({e})")),
            result["summary"].as_str().unwrap_or("(nothing)").chars().take(1500).collect::<String>(),
            result["files"].as_array().map_or("none".to_string(), |f| f.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
        );
        let first = result.clone();
        result = run_one(caps, &machine, request(fb, &tried, None), cancel, progress).unwrap_or_else(|e| json!({ "ok": false, "error": e, "harness": fb.id() }));
        result["handed_over_from"] = json!({ "harness": harness.id(), "error": first["error"], "summary": first["summary"], "files": first["files"] });
        handed_over = true;
    }
    result["machine"] = json!(machine);
    result["why"] = json!(if handed_over { format!("{why}; took over from {}", harness.title()) } else { why });
    keep(Record {
        at: Utc::now(),
        machine,
        dir: dir.to_string(),
        harness: result["harness"].as_str().unwrap_or(harness.id()).to_string(),
        why: result["why"].as_str().unwrap_or("").to_string(),
        task: task.to_string(),
        session: result["session"].as_str().map(str::to_string),
        ok: result["ok"] == true,
        summary: result["summary"].as_str().or(result["error"].as_str()).unwrap_or("").to_string(),
        files: result["files"].as_array().map(|f| f.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        diff_stat: result["diff_stat"].as_str().unwrap_or("").to_string(),
        handed_over,
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_harness_wins_and_giving_up_is_noticed() {
        assert_eq!(named("fix it with Claude Code please"), Some(Harness::Claude));
        assert_eq!(named("have OpenCode rename the field"), Some(Harness::OpenCode));
        assert_eq!(named("fix the failing test"), None);
        assert!(gave_up(&json!({ "ok": false, "error": "timed out" })));
        assert!(gave_up(&json!({ "ok": true, "summary": "I couldn't find where the parser is configured." })));
        assert!(!gave_up(&json!({ "ok": true, "summary": "Fixed the typo in README.md." })));
        let ask = approval(&json!({ "dir": "~/Projects/foo", "task": "fix it", "machine": "desktop" }));
        assert!(ask.dangerous && ask.what.starts_with("have a coding agent") && ask.what.contains("work unattended in ~/Projects/foo on desktop"));
        assert!(!approval(&json!({ "dir": "x", "task": "y", "mode": "plan" })).dangerous);
    }

    #[test]
    fn without_a_rater_a_task_counts_as_complex() {
        crate::decide::configure(None);
        assert!(rate("fix the typo", None).0);
    }
}
