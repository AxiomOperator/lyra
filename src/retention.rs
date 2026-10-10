//! How long lyra keeps what grows by itself (`[retention]`): saved
//! conversations, uploaded files, the usage log, the actions log, and devices
//! nobody has used in a while. Tidied once a day (off the loop); the sizes
//! show on the Status page. 0 keeps that kind forever. Pinned conversations
//! are always kept, and paired machines (nodes) never expire here.

use std::path::Path;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use serde_json::{Value, json};

/// `[retention]`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Saved conversations not touched in this many days go (pinned ones stay).
    pub sessions_days: u64,
    /// Files sent from devices, after this many days.
    pub uploads_days: u64,
    /// Months of AI usage kept.
    pub usage_months: u64,
    /// Months of "what lyra did" (About me) kept.
    pub actions_months: u64,
    /// Phones and browsers unseen this long are unpaired.
    pub devices_days: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { sessions_days: 365, uploads_days: 90, usage_months: 13, actions_months: 12, devices_days: 90 }
    }
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

pub fn configure(s: Settings) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

pub fn settings() -> Settings {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

/// Whether a file or folder was last changed more than `days` ago.
fn older_than(path: &Path, days: u64) -> bool {
    days > 0
        && std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok()).is_some_and(|age| age > Duration::from_secs(days * 86_400))
}

/// Month files (`YYYY-MM.jsonl`) older than the last `keep` months.
fn old_months(dir: &Path, keep: u64, now: chrono::DateTime<chrono::Utc>) -> Vec<std::path::PathBuf> {
    use chrono::Datelike;
    if keep == 0 {
        return Vec::new();
    }
    let this = now.year() as i64 * 12 + now.month0() as i64;
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let stamp = name.strip_suffix(".jsonl")?;
            let (y, m) = stamp.split_once('-')?;
            let month = y.parse::<i64>().ok()? * 12 + m.parse::<i64>().ok()? - 1;
            (this - month >= keep as i64).then(|| e.path())
        })
        .collect()
}

/// Tidy everything once (what went, for Activity).
pub fn tidy(home: &Path, devices: Option<&lyra_web::Devices>) -> Vec<String> {
    let s = settings();
    let mut said = Vec::new();
    // Conversations: not touched for a while, not pinned.
    let sessions = home.join("sessions");
    if s.sessions_days > 0 {
        let pinned: Vec<String> = crate::sessions::metas(&sessions).into_iter().filter(|(_, m)| m.pinned).map(|(id, _)| id).collect();
        let mut n = 0;
        for e in std::fs::read_dir(&sessions).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(id) = name.strip_suffix(".json") else { continue };
            if id == "meta" || id.starts_with('.') || pinned.iter().any(|p| p == id) || !older_than(&e.path(), s.sessions_days) {
                continue;
            }
            if std::fs::remove_file(e.path()).is_ok() {
                n += 1;
            }
        }
        if n > 0 {
            said.push(format!("{n} conversation{} not opened in {} days removed (pinned ones are kept)", if n == 1 { "" } else { "s" }, s.sessions_days));
        }
    }
    // Task boards whose conversation is gone.
    let gone = crate::board::tidy(home);
    if gone > 0 {
        said.push(format!("{gone} task board{} of removed conversations removed", if gone == 1 { "" } else { "s" }));
    }
    // Uploaded files.
    let mut n = 0;
    for e in std::fs::read_dir(home.join("uploads")).into_iter().flatten().flatten() {
        if e.path().is_dir() && older_than(&e.path().join("meta.json"), s.uploads_days) && std::fs::remove_dir_all(e.path()).is_ok() {
            n += 1;
        }
    }
    if n > 0 {
        said.push(format!("{n} uploaded file{} older than {} days removed", if n == 1 { "" } else { "s" }, s.uploads_days));
    }
    // Month logs: usage, and everyone's actions.
    let now = chrono::Utc::now();
    let mut logs: Vec<(std::path::PathBuf, u64, &str)> = vec![(home.join("usage"), s.usage_months, "usage"), (home.join("actions"), s.actions_months, "actions")];
    for person in std::fs::read_dir(home.join("users")).into_iter().flatten().flatten() {
        logs.push((person.path().join("actions"), s.actions_months, "actions"));
    }
    for (dir, keep, what) in logs {
        for old in old_months(&dir, keep, now) {
            if std::fs::remove_file(&old).is_ok() {
                said.push(format!("old {what} log {} removed", old.file_name().unwrap_or_default().to_string_lossy()));
            }
        }
    }
    // Phones and browsers nobody has used in a while (machines stay).
    if let Some(d) = devices.filter(|_| s.devices_days > 0) {
        let cutoff = now - chrono::Duration::days(s.devices_days as i64);
        let stale: Vec<lyra_web::Device> = d.list().into_iter().filter(|x| x.kind != "node" && x.last_seen < cutoff).collect();
        let gone = stale.iter().filter(|x| d.remove(&x.id).is_ok()).count();
        if gone > 0 {
            said.push(format!("{gone} device{} not seen for {} days unpaired", if gone == 1 { "" } else { "s" }, s.devices_days));
        }
    }
    said
}

/// A folder's files and bytes (all the way down).
fn size(dir: &Path) -> (u64, u64) {
    let mut out = (0, 0);
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            let (n, b) = size(&p);
            out = (out.0 + n, out.1 + b);
        } else if let Ok(m) = e.metadata() {
            out = (out.0 + 1, out.1 + m.len());
        }
    }
    out
}

static SIZES: Mutex<Option<(Instant, Value)>> = Mutex::new(None);

/// What's kept now, for the Status page (worked out at most every 10 minutes).
pub fn sizes(home: &Path, devices: Option<&lyra_web::Devices>) -> Value {
    if let Some((at, v)) = SIZES.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
        && at.elapsed() < Duration::from_secs(600)
    {
        return v.clone();
    }
    let s = settings();
    let mut actions = size(&home.join("actions"));
    for person in std::fs::read_dir(home.join("users")).into_iter().flatten().flatten() {
        let (n, b) = size(&person.path().join("actions"));
        actions = (actions.0 + n, actions.1 + b);
    }
    let (sessions, uploads, usage) = (size(&home.join("sessions")), size(&home.join("uploads")), size(&home.join("usage")));
    let list = devices.map(|d| d.list()).unwrap_or_default();
    let cutoff = chrono::Utc::now() - chrono::Duration::days(s.devices_days.max(1) as i64);
    let v = json!({
        "sessions": { "files": sessions.0, "bytes": sessions.1, "keep_days": s.sessions_days },
        "uploads": { "files": uploads.0, "bytes": uploads.1, "keep_days": s.uploads_days },
        "usage": { "files": usage.0, "bytes": usage.1, "keep_months": s.usage_months },
        "actions": { "files": actions.0, "bytes": actions.1, "keep_months": s.actions_months },
        "devices": { "paired": list.iter().filter(|d| d.kind != "node").count(), "stale": list.iter().filter(|d| d.kind != "node" && d.last_seen < cutoff).count(), "keep_days": s.devices_days },
    });
    *SIZES.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), v.clone()));
    v
}

/// "1.2 MB".
pub fn bytes_text(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} B"),
    }
}

/// The Status page's line: how much of each is kept.
pub fn line(v: &Value) -> String {
    let part = |k: &str, label: &str| format!("{label} {} ({})", v[k]["files"].as_u64().unwrap_or(0), bytes_text(v[k]["bytes"].as_u64().unwrap_or(0)));
    format!(
        "{} · {} · {} · {} · devices {} ({} unused)",
        part("sessions", "conversations"),
        part("uploads", "uploads"),
        part("usage", "usage logs"),
        part("actions", "action logs"),
        v["devices"]["paired"].as_u64().unwrap_or(0),
        v["devices"]["stale"].as_u64().unwrap_or(0)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_things_go_and_kept_things_stay() {
        crate::config::with_test_home(|home| {
            configure(Settings { sessions_days: 30, uploads_days: 30, usage_months: 2, actions_months: 2, devices_days: 30 });
            let sessions = home.join("sessions");
            std::fs::create_dir_all(&sessions).unwrap();
            let old = SystemTime::now() - Duration::from_secs(60 * 86_400);
            for id in ["old-chat", "old-pinned", "new-chat"] {
                let f = std::fs::File::create(sessions.join(format!("{id}.json"))).unwrap();
                if id.starts_with("old") {
                    f.set_modified(old).unwrap();
                }
            }
            std::fs::write(sessions.join("meta.json"), r#"{"old-pinned":{"pinned":true}}"#).unwrap();
            let up = home.join("uploads/u1");
            std::fs::create_dir_all(&up).unwrap();
            std::fs::File::create(up.join("meta.json")).unwrap().set_modified(old).unwrap();
            std::fs::create_dir_all(home.join("usage")).unwrap();
            let now = chrono::Utc::now();
            let this = now.format("%Y-%m").to_string();
            for m in [this.as_str(), "2001-01"] {
                std::fs::write(home.join("usage").join(format!("{m}.jsonl")), "").unwrap();
            }
            let said = tidy(home, None);
            assert!(!sessions.join("old-chat.json").exists(), "old and unpinned: gone");
            assert!(sessions.join("old-pinned.json").exists() && sessions.join("new-chat.json").exists(), "pinned or recent: kept");
            assert!(!up.exists());
            assert!(home.join("usage").join(format!("{this}.jsonl")).exists() && !home.join("usage/2001-01.jsonl").exists());
            assert!(said.iter().any(|s| s.contains("1 conversation")), "{said:?}");
            configure(Settings::default());
        });
    }

    #[test]
    fn zero_keeps_forever() {
        assert!(!older_than(Path::new("/"), 0));
        assert!(old_months(Path::new("/nonexistent"), 0, chrono::Utc::now()).is_empty());
    }
}
