//! Reminders: each person's own, on any task they can see. lyra serve looks
//! every half minute: one whose time has come is pushed (Done / In 1 hour /
//! Tomorrow); while the task is still open it's pushed again every
//! `nag_minutes`, at most `nag_max` times, outside the person's quiet time.

use chrono::{DateTime, Duration, Utc};

use super::store;

/// A reminder to push now.
#[derive(Debug, Clone, PartialEq)]
pub struct Due {
    pub user: String,
    pub task: String,
    pub title: String,
    /// 0: the reminder itself; then how many times it's been pushed again.
    pub nag: u32,
    /// When it went off.
    pub fired: DateTime<Utc>,
}

fn time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&Utc))
}

/// The reminders to push now (marked as pushed). `quiet(user)`: they're in
/// quiet time, so repeats wait (a reminder they set still comes at its time).
pub fn due(now: DateTime<Utc>, quiet: &dyn Fn(&str) -> bool) -> Vec<Due> {
    let s = super::settings();
    if !s.enabled {
        return Vec::new();
    }
    let Ok(all) = store::reminders(None) else { return Vec::new() };
    let titles: std::collections::HashMap<String, String> = store::tasks_by_id(&all.iter().map(|r| r.task.clone()).collect::<Vec<_>>()).unwrap_or_default().into_iter().map(|t| (t.id, t.title)).collect();
    let mut out = Vec::new();
    for r in all {
        let Some(at) = time(&r.at) else { continue };
        let title = titles.get(&r.task).cloned().unwrap_or_default();
        match r.fired.as_deref().and_then(time) {
            None if at <= now => {
                let stamp = now.to_rfc3339();
                let _ = store::reminder_fired(&r.task, &r.user, Some(&stamp), 0, None);
                // From before lyra was looking (it was down a day): not brought up now.
                if now - at <= Duration::hours(24) {
                    out.push(Due { user: r.user, task: r.task, title, nag: 0, fired: now });
                }
            }
            Some(fired) if s.nag && r.nags < s.nag_max && !quiet(&r.user) => {
                let last = r.nag_last.as_deref().and_then(time).unwrap_or(fired);
                if now - last >= Duration::minutes(s.nag_minutes.max(1) as i64) && now - fired <= Duration::hours(24) {
                    let stamp = now.to_rfc3339();
                    let _ = store::reminder_fired(&r.task, &r.user, Some(&fired.to_rfc3339()), r.nags + 1, Some(&stamp));
                    out.push(Due { user: r.user, task: r.task, title, nag: r.nags + 1, fired });
                }
            }
            _ => {}
        }
    }
    out
}

/// A button on a task's push, pressed by the person it was for (their own
/// answer, so it acts at once): done, in an hour, tomorrow, accept, decline.
pub fn push_action(action: &str, task: &str) -> Result<String, String> {
    match action {
        "done" => {
            let (t, next) = super::complete(task, "done (from the reminder)")?;
            Ok(format!("done: {}{}", t.title, next.and_then(|n| n.due).map(|d| format!(" (next on {d})")).unwrap_or_default()))
        }
        "snooze1h" | "tomorrow" => {
            let (t, at) = super::remind(task, Some(if action == "tomorrow" { "tomorrow 9am" } else { "in 1h" }))?;
            Ok(format!("{}: reminder moved to {}", t.title, at.map(|a| a.format("%a %H:%M").to_string()).unwrap_or_default()))
        }
        "accept" | "decline" => {
            let t = super::answer(task, action == "accept", "")?;
            Ok(format!("{} {}", if action == "accept" { "accepted" } else { "declined" }, t.title))
        }
        other => Err(format!("unknown action {other}")),
    }
}

/// How many reminders are set (everyone's), for Status.
pub fn count() -> Result<usize, String> {
    Ok(store::reminders(None)?.len())
}
