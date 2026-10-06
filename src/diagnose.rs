//! Problems researched by themselves (`[diagnose]`): when a machine reports a
//! new problem (a failed unit, a full disk) or one of lyra's status checks
//! goes down, `lyra serve` has the Operator look into it on that machine with
//! read-only checks — status, logs, configuration — and write up what's
//! wrong, the likely cause and the exact fix. Nothing is changed: approvals in
//! a diagnosis are declined. The write-up shows next to the problem (Machines,
//! Status), in Activity and as a notification; "Fix it" takes it to chat.
//! Kept in `~/.lyra/diagnoses.json`.

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// `[diagnose]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// Look into new problems by itself (else only on /diagnose or the app's button).
    pub auto: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: true, auto: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Diagnosis {
    /// What it's about: "desktop:unit:nginx.service", "status:embedding".
    pub key: String,
    /// The machine it's on ("server" for lyra's own checks).
    pub machine: String,
    /// The problem as it was reported.
    pub problem: String,
    /// "queued", "running", "done" or "failed".
    pub state: String,
    /// The write-up: what's wrong, the likely cause, the fix.
    pub summary: String,
    /// Its conversation (`/resume <id>`).
    pub session: String,
    pub at: DateTime<Utc>,
    /// The problem has since cleared.
    #[serde(default)]
    pub resolved: bool,
}

/// Write-ups kept.
const KEEP: usize = 50;
/// The same problem isn't looked into again sooner than this (hours).
const AGAIN_AFTER_HOURS: i64 = 24;

static LOCK: Mutex<()> = Mutex::new(());

fn path() -> Option<std::path::PathBuf> {
    Some(crate::config::home()?.join("diagnoses.json"))
}

pub fn all() -> Vec<Diagnosis> {
    path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(list: &[Diagnosis]) {
    if let Some(p) = path()
        && let Ok(text) = serde_json::to_string_pretty(list)
    {
        let _ = std::fs::write(p, text);
    }
}

fn change(f: impl FnOnce(&mut Vec<Diagnosis>)) {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = all();
    f(&mut list);
    // Newest first, a bounded list.
    list.sort_by_key(|d| std::cmp::Reverse(d.at));
    list.truncate(KEEP);
    save(&list);
}

/// Ask for a problem to be looked into. `false` when it already is, or was
/// lately (unless `force`, the user asking).
pub fn queue(key: &str, machine: &str, problem: &str, force: bool) -> bool {
    let mut queued = false;
    change(|list| {
        let recent = list.iter().find(|d| d.key == key);
        let busy = recent.is_some_and(|d| d.state == "queued" || d.state == "running");
        let lately = recent.is_some_and(|d| !d.resolved && d.state == "done" && (Utc::now() - d.at).num_hours() < AGAIN_AFTER_HOURS);
        if busy || (lately && !force) {
            return;
        }
        list.retain(|d| d.key != key);
        list.push(Diagnosis {
            key: key.into(),
            machine: machine.into(),
            problem: problem.into(),
            state: "queued".into(),
            summary: String::new(),
            session: String::new(),
            at: Utc::now(),
            resolved: false,
        });
        queued = true;
    });
    queued
}

/// The oldest waiting one, now marked running.
pub fn start_next(session: impl FnOnce(&Diagnosis) -> String) -> Option<Diagnosis> {
    let mut started = None;
    change(|list| {
        if list.iter().any(|d| d.state == "running") {
            return;
        }
        if let Some(d) = list.iter_mut().filter(|d| d.state == "queued").min_by_key(|d| d.at) {
            d.session = session(d);
            d.state = "running".into();
            started = Some(d.clone());
        }
    });
    started
}

pub fn has_queued() -> bool {
    all().iter().any(|d| d.state == "queued")
}

pub fn finish(key: &str, summary: &str, ok: bool) {
    change(|list| {
        if let Some(d) = list.iter_mut().find(|d| d.key == key) {
            d.state = if ok { "done" } else { "failed" }.into();
            d.summary = summary.trim().into();
            d.at = Utc::now();
        }
    });
}

/// The problem went away.
pub fn resolve(key: &str) {
    change(|list| {
        if let Some(d) = list.iter_mut().find(|d| d.key == key) {
            d.resolved = true;
        }
    });
}

/// After a restart nothing is running: what was is looked into again.
pub fn requeue_running() {
    change(|list| {
        for d in list.iter_mut().filter(|d| d.state == "running") {
            d.state = "queued".into();
        }
    });
}

/// What the Operator is asked.
pub fn message(d: &Diagnosis) -> String {
    let what = if d.key.starts_with("status:") {
        format!("lyra's own check reports: {}. Work from @server.", d.problem)
    } else {
        format!("On @{}: {}.", d.machine, d.problem)
    };
    format!(
        "[diagnosis] {what}\n\nFind out why, with read-only checks only (status, logs, configuration: e.g. systemctl status, \
         journalctl -u … -n 50, the unit or config file, whether a host or port answers). Change nothing. Then report briefly:\n\
         1. What's wrong\n2. The likely cause\n3. The fix: the exact commands, and whether they're safe to run\n\
         Start with a one-line summary."
    )
}

/// The write-ups, newest first, for the app and panels.
pub fn view() -> Value {
    json!(all().iter().take(30).collect::<Vec<_>>())
}

/// The line of a write-up worth showing on its own: one labeled as the
/// summary ("**One-line summary:** …") if there is one, else the first real
/// line; markdown and headings dropped.
pub fn headline(summary: &str) -> String {
    let clean: Vec<String> = summary
        .lines()
        .map(|l| l.replace("**", "").replace('`', "").trim().trim_start_matches(['#', '-', '*', '>', ' ']).trim().to_string())
        .collect();
    let labeled = clean.iter().find_map(|l| {
        let (label, rest) = l.split_once(':')?;
        (label.len() <= 24 && (label.to_lowercase().contains("summary") || label.eq_ignore_ascii_case("tl;dr")) && !rest.trim().is_empty()).then(|| rest.trim().to_string())
    });
    let first = || clean.iter().find(|l| !l.is_empty() && !l.ends_with(':') && !l.eq_ignore_ascii_case("summary")).cloned();
    labeled.or_else(first).unwrap_or_default().chars().take(200).collect()
}

/// `/diagnose` list.
pub fn describe() -> String {
    let list = all();
    if list.is_empty() {
        return "nothing looked into yet. New machine problems and status checks that go down are researched by themselves; /diagnose <machine> <problem> asks now.".into();
    }
    let mut out: Vec<String> = list
        .iter()
        .take(15)
        .map(|d| {
            let state = match d.state.as_str() {
                "done" => headline(&d.summary),
                "running" => "looking into it…".into(),
                "queued" => "waiting".into(),
                _ => format!("couldn't finish: {}", headline(&d.summary)),
            };
            format!(
                "{} {} — {}{}\n    {state}{}",
                d.at.with_timezone(&chrono::Local).format("%m-%d %H:%M"),
                d.machine,
                d.problem,
                if d.resolved { " (cleared since)" } else { "" },
                if d.session.is_empty() { String::new() } else { format!("  /resume {}", d.session) }
            )
        })
        .collect();
    out.push("/diagnose <machine> <problem> looks into something now".into());
    out.join("\n")
}

/// A machine's open problems' write-ups, a line each (for `/machines health`).
pub fn lines_for(machine: &str) -> Vec<String> {
    all()
        .into_iter()
        .filter(|d| d.machine.eq_ignore_ascii_case(machine) && !d.resolved)
        .map(|d| match d.state.as_str() {
            "done" => format!("  🔎 {}: {}  (/resume {})", d.problem, headline(&d.summary), d.session),
            "running" => format!("  🔎 {}: looking into it…", d.problem),
            "queued" => format!("  🔎 {}: waiting to be looked into", d.problem),
            _ => format!("  🔎 {}: couldn't finish", d.problem),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_write_up_leads_with_its_point() {
        assert_eq!(headline("## Summary\n**The NFS server 192.1.3.196 doesn't answer, so the mount times out.**\n\n1. What's wrong:"), "The NFS server 192.1.3.196 doesn't answer, so the mount times out.");
        assert_eq!(headline(""), "");
        assert_eq!(headline("Tool budget is used up.\n\n## Summary\n**One-line summary:** The NFS mount `x` hung."), "The NFS mount x hung.", "the labeled summary wins over a preamble");
        assert_eq!(headline("## Summary\n**One-line summary:** The NFS mount `x` hung."), "The NFS mount x hung.");
        let d = Diagnosis {
            key: "desktop:unit:x.mount".into(),
            machine: "desktop".into(),
            problem: "x.mount failed".into(),
            state: "queued".into(),
            summary: String::new(),
            session: String::new(),
            at: Utc::now(),
            resolved: false,
        };
        let m = message(&d);
        assert!(m.contains("On @desktop: x.mount failed.") && m.contains("Change nothing"));
        let s = Diagnosis { key: "status:embedding".into(), machine: "server".into(), ..d };
        assert!(message(&s).contains("Work from @server"));
    }
}
