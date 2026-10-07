//! Times as people say them: "tomorrow", "friday at 3pm", "in 2h", "next
//! monday 9:30", "oct 14", "10/14", "2026-10-14", "tonight", "eod". Used for
//! task due dates and reminders.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone, Weekday};

/// A day, maybe with a time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct When {
    pub date: NaiveDate,
    pub time: Option<NaiveTime>,
}

impl When {
    /// The moment it names; a day without a time is 9:00.
    pub fn at(&self) -> Option<DateTime<Local>> {
        let t = self.time.unwrap_or_else(|| NaiveTime::from_hms_opt(9, 0, 0).unwrap_or_default());
        Local.from_local_datetime(&self.date.and_time(t)).earliest()
    }
}

const DAYS: [(&str, Weekday); 7] =
    [("monday", Weekday::Mon), ("tuesday", Weekday::Tue), ("wednesday", Weekday::Wed), ("thursday", Weekday::Thu), ("friday", Weekday::Fri), ("saturday", Weekday::Sat), ("sunday", Weekday::Sun)];

const MONTHS: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];

fn weekday(w: &str) -> Option<Weekday> {
    let w = w.trim_end_matches('.');
    DAYS.iter().find(|(name, _)| *name == w || (w.len() >= 3 && name.starts_with(w)) || (w == "thurs" && *name == "thursday") || (w == "tues" && *name == "tuesday")).map(|(_, d)| *d)
}

fn month(w: &str) -> Option<u32> {
    let w = w.trim_end_matches('.');
    (w.len() >= 3).then(|| MONTHS.iter().position(|m| m.starts_with(w))).flatten().map(|i| i as u32 + 1)
}

/// The next date (from `today`) in a month and day: this year, or next if it's past.
fn upcoming(today: NaiveDate, m: u32, d: u32) -> Option<NaiveDate> {
    let this = NaiveDate::from_ymd_opt(today.year(), m, d)?;
    if this < today { NaiveDate::from_ymd_opt(today.year() + 1, m, d) } else { Some(this) }
}

/// "3pm", "15:00", "9", "noon", "midnight".
fn time(w: &str, bare_ok: bool) -> Option<NaiveTime> {
    match w {
        "noon" | "midday" => return NaiveTime::from_hms_opt(12, 0, 0),
        "midnight" => return NaiveTime::from_hms_opt(0, 0, 0),
        _ => {}
    }
    let marked = w.ends_with("am") || w.ends_with("pm") || w.contains(':');
    if !marked && !bare_ok {
        return None;
    }
    let digits_ok = w.trim_end_matches("am").trim_end_matches("pm").chars().all(|c| c.is_ascii_digit() || c == ':');
    if !digits_ok {
        return None;
    }
    crate::routines::time_of_day(w)
}

/// Read a whole phrase, or nothing.
pub fn parse(text: &str, now: DateTime<Local>) -> Option<When> {
    let today = now.date_naive();
    let lower = text.to_lowercase().replace(',', " ");
    let words: Vec<&str> = lower.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let (mut date, mut clock): (Option<NaiveDate>, Option<NaiveTime>) = (None, None);
    let mut i = 0;
    let mut next = false;
    let mut at = false;
    while i < words.len() {
        let w = words[i];
        let peek = words.get(i + 1).copied();
        match w {
            "on" | "by" | "due" | "this" | "the" => {}
            "at" | "@" => at = true,
            "next" => next = true,
            "today" => date = Some(today),
            "tonight" => {
                date = Some(today);
                clock = clock.or(NaiveTime::from_hms_opt(20, 0, 0));
            }
            "tomorrow" | "tmrw" | "tmr" => date = Some(today + Duration::days(1)),
            "morning" => clock = clock.or(NaiveTime::from_hms_opt(9, 0, 0)),
            "afternoon" => clock = clock.or(NaiveTime::from_hms_opt(14, 0, 0)),
            "evening" => clock = clock.or(NaiveTime::from_hms_opt(18, 0, 0)),
            "eod" => {
                date = date.or(Some(today));
                clock = clock.or(NaiveTime::from_hms_opt(17, 0, 0));
            }
            "week" if next => {
                // Next week: its Monday.
                let days = 7 - today.weekday().num_days_from_monday() as i64;
                date = Some(today + Duration::days(days));
                next = false;
            }
            "in" => {
                // "in 2h", "in 3 days", "in 30 min", "in an hour"
                let n_word = peek?;
                let (n, unit, used) = match n_word.find(|c: char| !c.is_ascii_digit()) {
                    _ if n_word == "a" || n_word == "an" => (1, words.get(i + 2).copied()?, 2),
                    Some(0) => return None,
                    Some(k) => (n_word[..k].parse::<i64>().ok()?, &n_word[k..], 1),
                    None => (n_word.parse::<i64>().ok()?, words.get(i + 2).copied()?, 2),
                };
                let moment = match unit.trim_end_matches('s') {
                    "m" | "min" | "minute" | "mn" => now + Duration::minutes(n),
                    "h" | "hr" | "hour" => now + Duration::hours(n),
                    "d" | "day" => now + Duration::days(n),
                    "w" | "wk" | "week" => now + Duration::weeks(n),
                    _ => return None,
                };
                date = Some(moment.date_naive());
                if !matches!(unit.trim_end_matches('s'), "d" | "day" | "w" | "wk" | "week") {
                    clock = Some(moment.time());
                }
                i += used;
            }
            _ => {
                if let Some(d) = weekday(w) {
                    let ahead = (d.num_days_from_monday() as i64 - today.weekday().num_days_from_monday() as i64).rem_euclid(7);
                    // "friday" on a Friday is today; "next friday" then is a week on.
                    let ahead = if next && ahead == 0 { 7 } else { ahead };
                    date = Some(today + Duration::days(ahead));
                    next = false;
                } else if let Some(m) = month(w) {
                    // "oct 14"
                    let d: u32 = peek?.trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse().ok()?;
                    date = Some(upcoming(today, m, d)?);
                    i += 1;
                } else if let Some(m) = peek.and_then(month).filter(|_| w.trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse::<u32>().is_ok()) {
                    // "14 oct"
                    let d: u32 = w.trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse().ok()?;
                    date = Some(upcoming(today, m, d)?);
                    i += 1;
                } else if let Ok(d) = NaiveDate::parse_from_str(w, "%Y-%m-%d") {
                    date = Some(d);
                } else if let Some((m, d)) = w.split_once('/').filter(|_| !w.contains(':')) {
                    // "10/14" or "10/14/2026"
                    let (d, y) = d.split_once('/').map_or((d, None), |(d, y)| (d, Some(y)));
                    let (m, d): (u32, u32) = (m.parse().ok()?, d.parse().ok()?);
                    date = Some(match y {
                        Some(y) => NaiveDate::from_ymd_opt(y.parse::<i32>().ok().map(|y| if y < 100 { y + 2000 } else { y })?, m, d)?,
                        None => upcoming(today, m, d)?,
                    });
                } else {
                    clock = Some(time(w, at)?);
                    at = false;
                }
            }
        }
        i += 1;
    }
    if date.is_none() && clock.is_none() {
        return None;
    }
    // A time alone is today, or tomorrow when it has passed.
    let date = date.unwrap_or_else(|| if clock.is_some_and(|t| t <= now.time()) { today + Duration::days(1) } else { today });
    Some(When { date, time: clock })
}

/// "call the vendor friday at 3pm" → ("call the vendor", friday 15:00): the
/// longest ending (or beginning) that reads as a time.
pub fn split(text: &str, now: DateTime<Local>) -> (String, Option<When>) {
    let words: Vec<&str> = text.split_whitespace().collect();
    for k in (1..words.len()).rev() {
        let (head, tail) = (words[..words.len() - k].join(" "), words[words.len() - k..].join(" "));
        if let Some(w) = parse(&tail, now) {
            return (head, Some(w));
        }
    }
    for k in (1..words.len()).rev() {
        let (head, tail) = (words[..k].join(" "), words[k..].join(" "));
        if let Some(w) = parse(&head, now) {
            return (tail, Some(w));
        }
    }
    (text.trim().to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wednesday 2026-10-07 10:00.
    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 7, 10, 0, 0).unwrap()
    }

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }

    fn t(h: u32, m: u32) -> Option<NaiveTime> {
        NaiveTime::from_hms_opt(h, m, 0)
    }

    #[test]
    fn days_and_times() {
        let p = |s: &str| parse(s, now());
        assert_eq!(p("tomorrow"), Some(When { date: d(10, 8), time: None }));
        assert_eq!(p("friday at 3pm"), Some(When { date: d(10, 9), time: t(15, 0) }));
        assert_eq!(p("Fri 15:30"), Some(When { date: d(10, 9), time: t(15, 30) }));
        assert_eq!(p("wednesday"), Some(When { date: d(10, 7), time: None }), "today is Wednesday");
        assert_eq!(p("next wednesday"), Some(When { date: d(10, 14), time: None }));
        assert_eq!(p("next monday 9:30am"), Some(When { date: d(10, 12), time: t(9, 30) }));
        assert_eq!(p("next week"), Some(When { date: d(10, 12), time: None }));
        assert_eq!(p("at 3"), Some(When { date: d(10, 8), time: t(3, 0) }), "3 o'clock has passed: tomorrow");
        assert_eq!(p("at 3pm"), Some(When { date: d(10, 7), time: t(15, 0) }));
        assert_eq!(p("in 2h"), Some(When { date: d(10, 7), time: t(12, 0) }));
        assert_eq!(p("in 30 minutes"), Some(When { date: d(10, 7), time: t(10, 30) }));
        assert_eq!(p("in an hour"), Some(When { date: d(10, 7), time: t(11, 0) }));
        assert_eq!(p("in 3 days"), Some(When { date: d(10, 10), time: None }));
        assert_eq!(p("oct 14"), Some(When { date: d(10, 14), time: None }));
        assert_eq!(p("on the 14 oct at noon"), Some(When { date: d(10, 14), time: t(12, 0) }));
        assert_eq!(p("10/14"), Some(When { date: d(10, 14), time: None }));
        assert_eq!(p("1/5"), Some(When { date: NaiveDate::from_ymd_opt(2027, 1, 5).unwrap(), time: None }), "a past date is next year");
        assert_eq!(p("2026-12-01"), Some(When { date: d(12, 1), time: None }));
        assert_eq!(p("tonight"), Some(When { date: d(10, 7), time: t(20, 0) }));
        assert_eq!(p("tomorrow morning"), Some(When { date: d(10, 8), time: t(9, 0) }));
        assert_eq!(p("eod"), Some(When { date: d(10, 7), time: t(17, 0) }));
        assert_eq!(p("call bob"), None);
        assert_eq!(p("3"), None, "a bare number isn't a time");
    }

    #[test]
    fn titles_and_times_apart() {
        let (title, w) = split("call the vendor friday at 3pm", now());
        assert_eq!((title.as_str(), w), ("call the vendor", Some(When { date: d(10, 9), time: t(15, 0) })));
        let (title, w) = split("tomorrow renew the domain", now());
        assert_eq!((title.as_str(), w.map(|w| w.date)), ("renew the domain", Some(d(10, 8))));
        let (title, w) = split("buy 3 printers", now());
        assert_eq!((title.as_str(), w), ("buy 3 printers", None));
        let (title, w) = split("send the PO in 2h", now());
        assert_eq!((title.as_str(), w.and_then(|w| w.time)), ("send the PO", t(12, 0)));
    }
}
