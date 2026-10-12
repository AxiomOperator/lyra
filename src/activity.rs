//! Everything lyra does, kept: each line it logs (a reply's rounds, tools,
//! agents, routines, reminders, its own background work) goes to
//! `activity/<YYYY-MM>.jsonl` with whose it is and the conversation. Each
//! person sees their own (the Activity page); admins see everyone's, and
//! lyra's own work (All activity). Month files are read only from where the
//! last look left off; old months go after `[retention] activity_months`.

use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One line of what lyra did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub at: DateTime<Utc>,
    /// Whose it is (the conversation's person).
    pub user: String,
    /// The conversation it happened in.
    #[serde(default)]
    pub session: String,
    /// info, tool, learn, memory, plan, evolve, agent, error.
    pub level: String,
    pub text: String,
}

fn dir() -> Option<PathBuf> {
    Some(crate::config::home()?.join("activity"))
}

static WRITE: Mutex<()> = Mutex::new(());

/// Keep a line (never fails the caller: a line that can't be written is dropped).
pub fn record(user: &str, session: &str, level: &str, text: &str) {
    if cfg!(test) && std::env::var_os("LYRA_TEST_ACTIVITY").is_none() {
        return;
    }
    let Some(d) = dir() else { return };
    let line = Line { at: Utc::now(), user: user.to_string(), session: session.to_string(), level: level.to_string(), text: text.chars().take(2000).collect() };
    let Ok(text) = serde_json::to_string(&line) else { return };
    let _held = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::create_dir_all(&d);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(d.join(format!("{}.jsonl", line.at.format("%Y-%m")))) {
        let _ = writeln!(f, "{text}");
    }
}

/// A month file as read so far.
struct Month {
    id: u64,
    read: u64,
    lines: Vec<Line>,
}

static MONTHS: LazyLock<Mutex<HashMap<PathBuf, Month>>> = LazyLock::new(Default::default);

/// A month's lines, reading only what was added since the last look.
fn month(path: &Path, with: impl FnOnce(&[Line])) {
    let mut all = MONTHS.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(meta) = std::fs::metadata(path) else {
        all.remove(path);
        return;
    };
    #[cfg(unix)]
    let id = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let id = 0;
    let m = all.entry(path.to_path_buf()).or_insert(Month { id, read: 0, lines: Vec::new() });
    if m.id != id || meta.len() < m.read {
        *m = Month { id, read: 0, lines: Vec::new() };
    }
    if meta.len() > m.read
        && let Ok(mut f) = std::fs::File::open(path)
        && f.seek(std::io::SeekFrom::Start(m.read)).is_ok()
    {
        let mut new = Vec::new();
        if f.read_to_end(&mut new).is_ok() {
            // Whole lines only: one being written is read next time.
            let whole = new.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
            for line in String::from_utf8_lossy(&new[..whole]).lines() {
                if let Ok(l) = serde_json::from_str::<Line>(line) {
                    m.lines.push(l);
                }
            }
            m.read += whole as u64;
        }
    }
    with(&m.lines);
}

/// What to show.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Only this person's (`None`: everyone's, admins only).
    pub user: Option<String>,
    /// Only these levels (empty: all).
    pub levels: Vec<String>,
    /// Words that must all be in the line.
    pub words: String,
    /// How far back.
    pub days: i64,
    /// Older than this (paging back).
    pub before: Option<DateTime<Utc>>,
    pub limit: usize,
}

/// The newest lines that pass, newest first.
pub fn lines(f: &Filter) -> Vec<Line> {
    let Some(d) = dir() else { return Vec::new() };
    let now = Utc::now();
    let since = now - Duration::days(f.days.clamp(1, 92));
    let words: Vec<String> = f.words.split_whitespace().map(str::to_lowercase).collect();
    let mut months: Vec<String> = Vec::new();
    let mut m = since;
    while m <= now + Duration::days(31) {
        let name = m.format("%Y-%m").to_string();
        if !months.contains(&name) {
            months.push(name);
        }
        m += Duration::days(28);
    }
    let mut out: Vec<Line> = Vec::new();
    for name in months {
        month(&d.join(format!("{name}.jsonl")), |all| {
            out.extend(
                all.iter()
                    .filter(|l| l.at >= since && f.before.is_none_or(|b| l.at < b))
                    .filter(|l| f.user.as_ref().is_none_or(|u| l.user == *u))
                    .filter(|l| f.levels.is_empty() || f.levels.contains(&l.level))
                    .filter(|l| {
                        let t = l.text.to_lowercase();
                        words.iter().all(|w| t.contains(w))
                    })
                    .cloned(),
            );
        });
    }
    out.sort_by_key(|l| std::cmp::Reverse(l.at));
    out.truncate(f.limit.clamp(1, 1000));
    out
}

/// The page's answer: the lines, and (admins) who's in them.
pub fn page(viewer: &str, admin: bool, everyone: bool, arg: &Value) -> Value {
    let user = if admin && everyone { arg["user"].as_str().filter(|u| !u.is_empty()).map(str::to_string) } else { Some(viewer.to_string()) };
    let f = Filter {
        user,
        levels: arg["levels"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        words: arg["q"].as_str().unwrap_or("").to_string(),
        days: arg["days"].as_i64().unwrap_or(7),
        before: arg["before"].as_str().and_then(|b| DateTime::parse_from_rfc3339(b).ok()).map(|b| b.with_timezone(&Utc)),
        limit: arg["limit"].as_u64().unwrap_or(300) as usize,
    };
    let found = lines(&f);
    let names: HashMap<String, String> = crate::config::home().map(|h| lyra_web::Users::open(&h.join("web")).list()).unwrap_or_default().into_iter().map(|u| (u.id, u.name)).collect();
    let name = |u: &str| names.get(u).cloned().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| if u == lyra_web::users::OWNER { "the owner".into() } else { u.to_string() });
    let mut v = json!({
        "lines": found.iter().map(|l| json!({ "at": l.at, "user": l.user, "name": name(&l.user), "session": l.session, "level": l.level, "text": l.text })).collect::<Vec<_>>(),
        "more": found.len() >= f.limit.clamp(1, 1000),
    });
    if admin && everyone {
        let mut people: Vec<(String, String)> = names.iter().map(|(id, n)| (id.clone(), n.clone())).collect();
        if !people.iter().any(|(id, _)| id == lyra_web::users::OWNER) {
            people.push((lyra_web::users::OWNER.into(), name(lyra_web::users::OWNER)));
        }
        people.sort_by_key(|p| p.1.to_lowercase());
        v["people"] = json!(people.iter().map(|(id, n)| json!({ "id": id, "name": n })).collect::<Vec<_>>());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_persons_lines_are_kept_and_found() {
        crate::config::with_test_home(|home| {
            // SAFETY: only this test reads it, and it's set before anything runs.
            unsafe { std::env::set_var("LYRA_TEST_ACTIVITY", "1") };
            record("owner", "s1", "tool", "web_search: printer drivers");
            record("dana", "s2", "info", "from Browser: hello");
            record("dana", "s2", "error", "the model didn't answer");
            let all = lines(&Filter { days: 7, limit: 100, ..Default::default() });
            assert_eq!(all.len(), 3);
            assert_eq!(all[0].text, "the model didn't answer", "newest first");
            let danas = page("dana", false, true, &json!({ "user": "owner" }));
            assert_eq!(danas["lines"].as_array().unwrap().len(), 2, "a member sees only theirs, whatever they ask");
            assert!(danas.get("people").is_none());
            let admin = page("owner", true, true, &json!({ "user": "dana", "levels": ["error"] }));
            assert_eq!(admin["lines"][0]["text"], "the model didn't answer");
            let found = page("owner", true, true, &json!({ "q": "PRINTER" }));
            assert_eq!(found["lines"].as_array().unwrap().len(), 1);
            // Appended later: read from where it was left.
            record("dana", "s2", "info", "later");
            assert_eq!(lines(&Filter { user: Some("dana".into()), days: 7, limit: 100, ..Default::default() }).len(), 3);
            assert!(home.join("activity").read_dir().unwrap().count() >= 1);
            unsafe { std::env::remove_var("LYRA_TEST_ACTIVITY") };
        });
    }
}
