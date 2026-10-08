//! Machine health (`[health]`): each machine's report (`lyra_node::health`)
//! checked against limits — a disk nearly full, memory or load too high, a
//! failed systemd unit — and a machine gone quiet. A new problem is logged
//! and pushed once; it's reported again when it clears.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

/// `[health]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// A disk this full (percent) is a problem.
    pub disk_percent: u64,
    pub memory_percent: u64,
    /// The 15-minute load per CPU above which a machine is overloaded.
    pub load_per_cpu: f64,
    /// A failed systemd unit is a problem.
    pub failed_units: bool,
    /// A machine silent this long (minutes) is reported; 0 turns it off.
    pub offline_minutes: u64,
    /// Push notifications for problems (they're logged either way).
    pub notify: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: true, disk_percent: 90, memory_percent: 95, load_per_cpu: 2.0, failed_units: true, offline_minutes: 10, notify: true }
    }
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

pub fn configure(s: Settings) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

pub fn settings() -> Settings {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

/// One thing wrong: a stable key (to notice when it clears) and what to say.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub key: String,
    pub text: String,
}

/// What's wrong in a health report.
pub fn problems(h: &Value, s: &Settings) -> Vec<Problem> {
    if !s.enabled {
        return Vec::new();
    }
    let mut out = Vec::new();
    for d in h["disks"].as_array().into_iter().flatten() {
        let pct = d["used_pct"].as_u64().unwrap_or(0);
        if pct >= s.disk_percent {
            let mount = d["mount"].as_str().unwrap_or("?");
            out.push(Problem { key: format!("disk:{mount}"), text: format!("disk {mount} is {pct}% full ({} free)", kb(d["avail_kb"].as_u64().unwrap_or(0))) });
        }
    }
    if let Some(pct) = h["memory"]["used_pct"].as_u64().filter(|p| *p >= s.memory_percent) {
        out.push(Problem { key: "memory".into(), text: format!("memory is {pct}% used") });
    }
    let cpus = h["cpus"].as_u64().unwrap_or(1).max(1) as f64;
    if let Some(load) = h["load"][2].as_f64().filter(|l| l / cpus >= s.load_per_cpu) {
        out.push(Problem { key: "load".into(), text: format!("load {load:.1} on {cpus} CPUs (15 min)") });
    }
    if s.failed_units {
        for u in h["failed_units"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            out.push(Problem { key: format!("unit:{u}"), text: format!("{u} failed") });
        }
    }
    out
}

fn kb(n: u64) -> String {
    match n {
        n if n >= 1024 * 1024 * 1024 => format!("{:.1} TB", n as f64 / 1073741824.0),
        n if n >= 1024 * 1024 => format!("{:.1} GB", n as f64 / 1048576.0),
        n if n >= 1024 => format!("{} MB", n / 1024),
        n => format!("{n} KB"),
    }
}

/// A report in a line: "disk 72% · mem 41% · load 0.3 · 2 failed · 14 updates".
pub fn summary(h: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(top) = h["disks"].as_array().and_then(|d| d.iter().max_by_key(|d| d["used_pct"].as_u64().unwrap_or(0))) {
        parts.push(format!("disk {}%", top["used_pct"].as_u64().unwrap_or(0)));
    }
    if let Some(m) = h["memory"]["used_pct"].as_u64() {
        parts.push(format!("mem {m}%"));
    }
    if let Some(l) = h["load"][0].as_f64() {
        parts.push(format!("load {l:.1}"));
    }
    let failed = h["failed_units"].as_array().map_or(0, Vec::len);
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if let Some(u) = h["updates"].as_u64().filter(|u| *u > 0) {
        parts.push(format!("{u} updates"));
    }
    parts.join(" · ")
}

/// A report for a machine's page: the report with what's wrong in it.
pub fn view(h: &Value) -> Value {
    let mut v = h.clone();
    v["problems"] = json!(problems(h, &settings()).iter().map(|p| p.text.clone()).collect::<Vec<_>>());
    v["summary"] = json!(summary(h));
    v
}

/// The full report as text (`/machines health <name>`).
pub fn describe(name: &str, h: &Value) -> String {
    let mut out = vec![format!("{name}: {}", summary(h))];
    for p in problems(h, &settings()) {
        out.push(format!("  ⚠ {}", p.text));
    }
    for d in h["disks"].as_array().into_iter().flatten() {
        out.push(format!("  disk {} {}% used, {} free", d["mount"].as_str().unwrap_or("?"), d["used_pct"].as_u64().unwrap_or(0), kb(d["avail_kb"].as_u64().unwrap_or(0))));
    }
    if let Some(m) = h["memory"].as_object() {
        out.push(format!("  memory: {}% used of {}", m["used_pct"].as_u64().unwrap_or(0), kb(m["total_kb"].as_u64().unwrap_or(0))));
    }
    if let Some(l) = h["load"].as_array() {
        out.push(format!("  load: {} ({} CPUs)", l.iter().filter_map(Value::as_f64).map(|x| format!("{x:.2}")).collect::<Vec<_>>().join(" "), h["cpus"].as_u64().unwrap_or(1)));
    }
    if let Some(up) = h["uptime_s"].as_u64() {
        out.push(format!("  up {}d {}h", up / 86400, up % 86400 / 3600));
    }
    match h["failed_units"].as_array() {
        Some(f) if !f.is_empty() => out.push(format!("  failed: {}", f.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))),
        Some(_) => out.push("  no failed units".into()),
        None => {}
    }
    match h["updates"].as_u64() {
        Some(n) => out.push(format!("  {n} pending updates")),
        None => out.push("  updates: unknown (no dnf/apt cache)".into()),
    }
    if let Some(at) = h["at"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()) {
        out.push(format!("  reported {}", at.with_timezone(&chrono::Local).format("%H:%M")));
    }
    out.join("\n")
}

/// Problems already reported, and machines gone quiet (`alerts/health.json`:
/// `problem:<machine>:<key>` and `offline:<machine>`).
#[derive(Default)]
pub struct Alerts {
    told: crate::alerts::Ledger,
    /// Since when each paired machine has been offline (not kept: a restart starts over).
    offline: HashMap<String, Instant>,
}

/// The file as it was before `alerts::Ledger`.
#[derive(Deserialize, Default)]
struct Legacy {
    #[serde(default)]
    active: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    offline_told: HashSet<String>,
}

fn legacy(v: Value) -> HashMap<String, crate::alerts::Told> {
    let old: Legacy = serde_json::from_value(v).unwrap_or_default();
    let now = chrono::Utc::now();
    let told = |text: &str| crate::alerts::Told { text: text.to_string(), since: now };
    let problems = old.active.iter().flat_map(|(m, ps)| ps.iter().map(move |(k, t)| (format!("problem:{m}:{k}"), told(t))));
    problems.chain(old.offline_told.iter().map(|m| (format!("offline:{m}"), told("offline")))).collect()
}

impl Alerts {
    /// What was told before a restart (`~/.lyra/alerts/health.json`).
    pub fn load() -> Alerts {
        Alerts { told: crate::alerts::Ledger::load("health", legacy), offline: HashMap::new() }
    }
}

/// What changed for a machine: new problems and ones that cleared.
#[derive(Debug, Default, PartialEq)]
pub struct Change {
    pub new: Vec<String>,
    pub cleared: Vec<String>,
    /// The same, as (key, text): what diagnoses are filed under.
    pub new_keys: Vec<(String, String)>,
    pub cleared_keys: Vec<String>,
}

impl Alerts {
    /// A new report from `machine`.
    pub fn report(&mut self, machine: &str, found: Vec<Problem>) -> Change {
        let prefix = format!("problem:{}:", machine.to_lowercase());
        let known = self.told.under(&prefix);
        let at = chrono::Utc::now();
        let mut change = Change::default();
        for p in &found {
            if self.told.raise(&format!("{prefix}{}", p.key), &p.text, at) {
                change.new.push(p.text.clone());
                change.new_keys.push((p.key.clone(), p.text.clone()));
            }
        }
        for key in known.keys().filter(|k| !found.iter().any(|p| &p.key == *k)) {
            if let Some(t) = self.told.clear(&format!("{prefix}{key}")) {
                change.cleared.push(t.text);
                change.cleared_keys.push(key.clone());
            }
        }
        self.told.save();
        change
    }

    /// Machines connected now; returns (gone quiet too long, back after that).
    pub fn connected(&mut self, paired: &[String], online: &[String], quiet: Duration) -> (Vec<String>, Vec<String>) {
        let is_online = |m: &String| online.iter().any(|o| o.eq_ignore_ascii_case(m));
        let mut back = Vec::new();
        for m in paired {
            if is_online(m) {
                self.offline.remove(m);
                if self.told.clear(&format!("offline:{m}")).is_some() {
                    back.push(m.clone());
                }
            } else {
                self.offline.entry(m.clone()).or_insert_with(Instant::now);
            }
        }
        // Unpaired machines are forgotten.
        self.offline.retain(|m, _| paired.contains(m));
        self.told.retain(|k| k.strip_prefix("offline:").is_none_or(|m| paired.iter().any(|p| p == m)));
        let mut gone = Vec::new();
        if !quiet.is_zero() {
            let at = chrono::Utc::now();
            for (m, since) in &self.offline {
                if since.elapsed() >= quiet && self.told.raise(&format!("offline:{m}"), "offline", at) {
                    gone.push(m.clone());
                }
            }
        }
        gone.sort();
        self.told.save();
        (gone, back)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(disk: u64, failed: &[&str]) -> Value {
        json!({ "disks": [{ "mount": "/", "used_pct": disk, "avail_kb": 1048576 }], "memory": { "used_pct": 40, "total_kb": 8000000 },
                "load": [0.5, 0.4, 9.0], "cpus": 4, "failed_units": failed, "updates": 3 })
    }

    #[test]
    fn problems_are_told_once_and_again_when_they_clear() {
        let s = Settings::default();
        let p = problems(&report(93, &["nginx.service"]), &s);
        let texts: Vec<&str> = p.iter().map(|p| p.text.as_str()).collect();
        assert!(texts.contains(&"disk / is 93% full (1.0 GB free)") && texts.contains(&"nginx.service failed"), "{texts:?}");
        assert!(texts.iter().any(|t| t.starts_with("load 9.0 on 4 CPUs")), "15-minute load per CPU over the limit");
        assert!(problems(&report(50, &[]), &Settings { load_per_cpu: 3.0, ..s.clone() }).is_empty());

        let mut alerts = Alerts::default();
        let first = alerts.report("web1", problems(&report(93, &["nginx.service"]), &s));
        assert_eq!(first.new.len(), 3);
        assert!(alerts.report("web1", problems(&report(94, &["nginx.service"]), &s)).new.is_empty(), "still the same problems: quiet");
        let fixed = alerts.report("web1", problems(&report(60, &["nginx.service"]), &s));
        assert_eq!(fixed.cleared, vec!["disk / is 94% full (1.0 GB free)".to_string()]);
        assert!(summary(&report(60, &["x"])).contains("disk 60% · mem 40% · load 0.5 · 1 failed · 3 updates"));
    }

    #[test]
    fn a_quiet_machine_is_told_after_a_while() {
        let mut a = Alerts::default();
        let paired = vec!["web1".to_string(), "nas".to_string()];
        assert_eq!(a.connected(&paired, &["web1".into()], Duration::from_millis(30)), (vec![], vec![]));
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(a.connected(&paired, &["web1".into()], Duration::from_millis(30)), (vec!["nas".to_string()], vec![]));
        assert_eq!(a.connected(&paired, &["web1".into()], Duration::from_millis(30)), (vec![], vec![]), "once");
        assert_eq!(a.connected(&paired, &["web1".into(), "NAS".into()], Duration::from_millis(30)), (vec![], vec!["nas".to_string()]));
        assert_eq!(a.connected(&paired, &["web1".into()], Duration::ZERO), (vec![], vec![]), "0 turns it off");
    }

    #[test]
    fn what_was_told_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("lyra-alerts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("health.json");
        let s = Settings::default();
        let mut a = Alerts { told: crate::alerts::Ledger::at(path.clone(), legacy), ..Alerts::default() };
        assert_eq!(a.report("desktop", problems(&report(50, &["x.mount"]), &s)).new.len(), 2, "the failed unit and the high load");
        // lyra restarts: the same problem isn't news.
        let mut again = Alerts { told: crate::alerts::Ledger::at(path.clone(), legacy), ..Alerts::default() };
        assert!(again.report("desktop", problems(&report(50, &["x.mount"]), &s)).new.is_empty());
        assert_eq!(again.report("desktop", problems(&report(50, &[]), &s)).cleared.len(), 1, "and it's still told when it clears");
        // A file from before the ledger is read too: nothing is told twice after an update.
        std::fs::write(&path, r#"{"active":{"desktop":{"unit:x.mount":"x.mount failed"}},"offline_told":["nas"]}"#).unwrap();
        let mut old = Alerts { told: crate::alerts::Ledger::at(path.clone(), legacy), ..Alerts::default() };
        assert!(old.report("desktop", problems(&report(50, &["x.mount"]), &s)).new.iter().all(|t| !t.contains("x.mount")));
        let paired = vec!["nas".to_string()];
        assert_eq!(old.connected(&paired, &["nas".into()], Duration::from_secs(60)), (vec![], vec!["nas".to_string()]), "nas was told offline: back");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
