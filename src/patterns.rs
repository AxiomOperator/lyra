//! Goals and routines from patterns (4.5): something asked three times in
//! two weeks, or asked to be done again and again ("keep an eye on…",
//! "every morning…"), is offered as a routine; a lasting aim ("my goal is…")
//! as a goal. The offer is a card in the chat that sets it up with one tap
//! (its command), and isn't made again for 30 days.

use chrono::{DateTime, Duration, Timelike, Utc};

use crate::outcomes::similar;
use crate::store::JsonStore;

/// An offer: what to say, and the command that does it.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub text: String,
    pub command: String,
}

/// Asked this often in `WINDOW_DAYS` (this time included).
const TIMES: usize = 3;
const WINDOW_DAYS: i64 = 14;
const QUIET_DAYS: i64 = 30;

const RECURRING: &[&str] = &["keep an eye on", "keep track of", "keep checking", "every morning", "every day", "each day", "every evening", "every week", "each week", "every monday", "every friday", "every weekday", "remind me every", "check daily", "check weekly"];
const AIMS: &[&str] = &["my goal is", "i want to get", "i'm trying to", "i am trying to", "help me finish", "help me get to", "by the end of the month", "by the end of the quarter"];

fn lower(t: &str) -> String {
    t.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// When a recurring request says when ("every morning" → "every day at 08:00").
fn schedule_of(text: &str, hour: Option<u32>) -> String {
    let t = lower(text);
    for day in ["monday", "tuesday", "wednesday", "thursday", "friday"] {
        if t.contains(&format!("every {day}")) {
            return format!("{day} at 09:00");
        }
    }
    if t.contains("every week") || t.contains("each week") || t.contains("check weekly") {
        return "monday at 09:00".into();
    }
    if ["every morning", "each morning", "every day", "each day", "check daily"].iter().any(|p| t.contains(p)) {
        return "every day at 08:00".into();
    }
    if t.contains("every evening") {
        return "every day at 17:00".into();
    }
    if t.contains("every weekday") {
        return "weekdays at 08:00".into();
    }
    match hour {
        Some(h) => format!("every day at {h:02}:00"),
        None => "weekdays at 08:00".into(),
    }
}

/// A short routine name from the request ("check-the-backups").
fn name_of(text: &str) -> String {
    let skip = ["can", "you", "please", "could", "would", "keep", "an", "eye", "on", "every", "morning", "day", "week", "lyra", "me", "the", "a", "to", "and", "for"];
    let words: Vec<String> = lower(text).split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 1 && !skip.contains(w)).take(3).map(str::to_string).collect();
    if words.is_empty() { "my-routine".into() } else { words.join("-") }
}

/// Offers made: their normalized text and when.
type Offered = Vec<(String, DateTime<Utc>)>;

/// What's offered already, so nothing is offered twice.
fn offered_store(owner: &str) -> Option<JsonStore<Offered>> {
    let dir = if crate::acting::is_owner(owner) { crate::config::home()? } else { crate::config::home()?.join("users").join(owner) };
    Some(JsonStore::new(dir.join("patterns").join("offered.json")))
}

fn offered_lately(owner: &str, text: &str) -> bool {
    let cutoff = Utc::now() - Duration::days(QUIET_DAYS);
    offered_store(owner).is_some_and(|s| s.load().iter().any(|(t, at)| *at > cutoff && similar(t, text)))
}

fn note_offered(owner: &str, text: &str) {
    if let Some(s) = offered_store(owner) {
        let cutoff = Utc::now() - Duration::days(QUIET_DAYS);
        let _ = s.update(|all| {
            all.retain(|(_, at)| *at > cutoff);
            all.push((lower(text), Utc::now()));
        });
    }
}

/// An offer for this message, if it shows a pattern. `earlier`: the
/// person's own requests from their saved conversations (text, when).
pub fn offer(owner: &str, message: &str, earlier: &[(String, DateTime<Utc>)], existing_routines: &[String]) -> Option<Offer> {
    let text = message.trim();
    if text.len() < 12 || text.starts_with('/') || offered_lately(owner, text) || existing_routines.iter().any(|p| similar(p, text)) {
        return None;
    }
    let t = lower(text);
    let quote = |s: &str| s.replace('|', "/").chars().take(300).collect::<String>();
    let found = if AIMS.iter().any(|p| t.contains(p)) {
        let title: String = text.chars().take(80).collect();
        Some(Offer { text: format!("This sounds like a goal. Shall I keep it as one and track it until it's done?\n\"{title}\""), command: format!("/goal new {}", quote(&title)) })
    } else if RECURRING.iter().any(|p| t.contains(p)) {
        let schedule = schedule_of(text, None);
        Some(Offer { text: format!("You'd like this done again and again. Shall I make it a routine ({schedule})?"), command: format!("/routine new {} | {schedule} | {}", name_of(text), quote(text)) })
    } else {
        // The same asked three times in two weeks (on different days).
        let since = Utc::now() - Duration::days(WINDOW_DAYS);
        let like: Vec<&(String, DateTime<Utc>)> = earlier.iter().filter(|(e, at)| *at > since && similar(e, text)).collect();
        let mut days: Vec<_> = like.iter().map(|(_, at)| at.date_naive()).collect();
        days.sort();
        days.dedup();
        (days.len() + 1 >= TIMES).then(|| {
            let mut hours: Vec<u32> = like.iter().map(|(_, at)| at.with_timezone(&chrono::Local).hour()).collect();
            hours.sort();
            let schedule = schedule_of(text, hours.get(hours.len() / 2).copied());
            Offer {
                text: format!("You've asked this {} times in the last two weeks. Shall I make it a routine ({schedule}), so it's ready without asking?", like.len() + 1),
                command: format!("/routine new {} | {schedule} | {}", name_of(text), quote(text)),
            }
        })
    };
    if found.is_some() {
        note_offered(owner, text);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_recurring_and_aims_are_offered_once() {
        crate::config::with_test_home(|_| {
            let now = Utc::now();
            let earlier = vec![("summarize the new support tickets for me".to_string(), now - Duration::days(2)), ("summarize new support tickets for me".to_string(), now - Duration::days(5)), ("what's the weather".to_string(), now - Duration::days(1))];
            let o = offer("owner", "summarize the new support tickets for me", &earlier, &[]).expect("asked three times");
            assert!(o.text.contains("asked this 3 times") && o.command.starts_with("/routine new summarize-new-support | every day at "), "{o:?}");
            assert!(offer("owner", "summarize the new support tickets for me", &earlier, &[]).is_none(), "not twice");
            // Twice isn't a pattern yet; once on the same day isn't either.
            let twice = vec![("list my open invoices".to_string(), now - Duration::days(3))];
            assert!(offer("owner", "list my open invoices please", &twice, &[]).is_none());
            let o = offer("owner", "keep an eye on the backup server disk space every morning", &[], &[]).unwrap();
            assert!(o.command.contains("| every day at 08:00 | keep an eye on the backup server disk space every morning"), "{o:?}");
            assert!(offer("owner", "check the vpn every friday afternoon", &[], &["check the vpn every friday afternoon".into()]).is_none(), "a routine does that already");
            let g = offer("dana", "my goal is to clear the audit findings by the end of the month", &[], &[]).unwrap();
            assert!(g.command.starts_with("/goal new my goal is to clear the audit findings"), "{g:?}");
            assert!(offer("owner", "hello there", &[], &[]).is_none());
        });
    }
}
