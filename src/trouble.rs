//! Things that went wrong with nobody watching (an email that didn't go, a
//! store file that didn't read and was set aside, a result that wasn't kept):
//! into lyra's log as before, and also the Activity panel and the Status page,
//! where someone will see them.

use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};

/// Not yet in Activity (the serve loop takes them).
static NEW: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// The last few, for the Status page.
static RECENT: Mutex<Vec<(DateTime<Utc>, String)>> = Mutex::new(Vec::new());

/// Tell someone: the log, Activity, Status.
pub fn report(text: impl Into<String>) {
    let text = text.into();
    eprintln!("lyra: {text}");
    NEW.lock().unwrap_or_else(|e| e.into_inner()).push(text.clone());
    let mut recent = RECENT.lock().unwrap_or_else(|e| e.into_inner());
    recent.push((Utc::now(), text));
    let len = recent.len();
    if len > 50 {
        recent.drain(..len - 50);
    }
}

/// What's happened since last asked (for Activity).
pub fn take_new() -> Vec<String> {
    std::mem::take(&mut *NEW.lock().unwrap_or_else(|e| e.into_inner()))
}

/// The ones in the last `hours`, newest first (for Status).
pub fn recent(hours: i64) -> Vec<(DateTime<Utc>, String)> {
    let since = Utc::now() - Duration::hours(hours);
    let mut out: Vec<_> = RECENT.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(at, _)| *at >= since).cloned().collect();
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_problem_reaches_activity_once_and_status_for_a_day() {
        super::report("the recap wasn't emailed to dana: no address");
        let new = super::take_new();
        assert!(new.iter().any(|t| t.contains("recap wasn't emailed")));
        assert!(!super::take_new().iter().any(|t| t.contains("recap wasn't emailed")), "Activity hears it once");
        assert!(super::recent(24).iter().any(|(_, t)| t.contains("recap wasn't emailed")), "Status keeps showing it");
    }
}
