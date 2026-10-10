//! Fallbacks for the chat model (`[fallback_model]`, then
//! `[second_fallback_model]`): when the main one can't be reached or answers
//! with a server error before it says anything, the turn (or lyra's own
//! background call) goes to the first fallback instead, and says so; when that
//! one can't be reached either, to the second. After a failure a model is left
//! alone for a couple of minutes (every turn would otherwise wait for it to
//! time out first), then tried again.
//!
//! A fallback with `background = true` also takes lyra's small background
//! jobs while the main model is up (memory capture, skill reviews, mail
//! triage, briefing takeaways, routine checks, feedback analysis), so the
//! main model is left to the chats: the first one in line that does. A
//! failure there goes back to the main one.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Settings {
    /// Its OpenAI-compatible base URL, e.g. `http://gpu2:8080/v1`.
    pub url: String,
    pub model: String,
    /// Also does lyra's small background jobs while the main model is up.
    #[serde(default)]
    pub background: bool,
    /// Its prices (`input_cost_per_mtok` …), for AI usage.
    #[serde(flatten, default)]
    pub price: crate::usage::Price,
}

/// The fallbacks in order, each with its known-down id.
const IDS: [&str; 2] = ["fallback", "fallback2"];

static SETTINGS: RwLock<Vec<(&'static str, Settings)>> = RwLock::new(Vec::new());
/// When the main model last failed.
static DOWN: Mutex<Option<Instant>> = Mutex::new(None);
/// When each fallback last failed.
static RESTING: Mutex<Option<HashMap<&'static str, Instant>>> = Mutex::new(None);

/// How long a model is skipped after a failure.
const REST: Duration = Duration::from_secs(120);

pub fn configure(first: Option<Settings>, second: Option<Settings>) {
    let set = |s: &Option<Settings>| s.clone().filter(|s| !s.url.trim().is_empty() && !s.model.trim().is_empty());
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = IDS.into_iter().zip([set(&first), set(&second)]).filter_map(|(id, s)| Some((id, s?))).collect();
    *RESTING.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Every fallback as set up, in order, with its known-down id ("fallback", "fallback2").
pub fn each() -> Vec<(&'static str, Settings)> {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn full(s: &Settings) -> String {
    format!("{}/chat/completions", s.url.trim_end_matches('/'))
}

fn resting(id: &str) -> bool {
    RESTING.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(id)).is_some_and(|t| t.elapsed() < REST)
}

fn id_of(url: &str, model: &str) -> Option<&'static str> {
    each().into_iter().find(|(_, s)| full(s) == url && s.model == model).map(|(id, _)| id)
}

/// The fallbacks that can answer now, in order (not marked down, not failed
/// a moment ago), as chat completions URL and model.
pub fn stand_ins() -> Vec<(String, String)> {
    each().into_iter().filter(|(id, _)| !crate::known_down::is_down(id) && !resting(id)).map(|(_, s)| (full(&s), s.model)).collect()
}

/// The fallback that answers first, if one is set up (and can answer now).
pub fn target() -> Option<(String, String)> {
    stand_ins().into_iter().next()
}

/// The first fallback not marked down, as set up (for "/retry other").
pub fn first_up() -> Option<Settings> {
    each().into_iter().find(|(id, _)| !crate::known_down::is_down(id)).map(|(_, s)| s)
}

/// The next model in line after this one (the main model's is the first
/// fallback that can answer).
pub fn after(url: &str, model: &str) -> Option<(String, String)> {
    let Some(id) = id_of(url, model) else { return target() };
    let at = IDS.iter().position(|i| *i == id).unwrap_or(0);
    each().into_iter().filter(|(i, _)| IDS.iter().position(|x| x == i).unwrap_or(0) > at && !crate::known_down::is_down(i) && !resting(i)).map(|(_, s)| (full(&s), s.model)).next()
}

/// Where a small background job goes first: the first fallback that takes
/// them (`background = true`) and can answer, while the main model is up
/// (while one stands in for it, the ordinary line does).
pub fn light() -> Option<(String, String)> {
    if skip_main() {
        return None;
    }
    each().into_iter().find(|(id, s)| s.background && !crate::known_down::is_down(id) && !resting(id)).map(|(_, s)| (full(&s), s.model))
}

/// Skip the main model for now (marked known down, or it failed a moment
/// ago), when there's a fallback to use.
pub fn skip_main() -> bool {
    target().is_some() && (crate::known_down::is_down("chat") || DOWN.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| t.elapsed() < REST))
}

/// The main chat model is marked known down and nothing can stand in: why,
/// at once (rather than waiting for it to time out).
pub fn blocked() -> Option<String> {
    let m = crate::known_down::mark("chat")?;
    if each().iter().any(|(id, _)| !crate::known_down::is_down(id)) {
        return None;
    }
    let marked = !each().is_empty();
    Some(format!("the chat model is {}, and there's no fallback to answer instead ([fallback_model]{})", crate::known_down::describe(&m), if marked { ", also marked down" } else { "" }))
}

/// The main model failed: the fallbacks answer for a while.
pub fn main_failed() {
    *DOWN.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
}

/// The main model answered: back to it.
pub fn main_ok() {
    *DOWN.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// A fallback couldn't be reached: the next one in line answers for a while.
pub fn failed(url: &str, model: &str) {
    if let Some(id) = id_of(url, model) {
        RESTING.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(id, Instant::now());
    }
}

/// A fallback answered.
pub fn ok(url: &str, model: &str) {
    if let Some(id) = id_of(url, model)
        && let Some(m) = RESTING.lock().unwrap_or_else(|e| e.into_inner()).as_mut()
    {
        m.remove(id);
    }
}

/// One call down the line (not streamed): a small job (`light`) to the
/// fallback that takes them first; then the main model, unless it's being
/// skipped; then each fallback in turn while they can't be reached. The
/// reply and the model that gave it.
pub fn call<T>(url: &str, model: &str, light: bool, mut send: impl FnMut(&str, &str) -> Result<T, String>) -> Result<(T, String), String> {
    if let Some(why) = blocked() {
        return Err(why);
    }
    let mut tried: Vec<(String, String)> = Vec::new();
    if light && let Some((u, m)) = self::light().filter(|(u, _)| u != url) {
        match send(&u, &m) {
            Ok(r) => return Ok((r, m)),
            Err(e) => {
                if unreachable(&e) {
                    failed(&u, &m);
                }
                tried.push((u, m));
            }
        }
    }
    let skipped = skip_main();
    let mut last = None;
    if !skipped {
        match send(url, model) {
            Ok(r) => {
                main_ok();
                return Ok((r, model.to_string()));
            }
            Err(e) if unreachable(&e) && target().is_some() => {
                main_failed();
                last = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    for (u, m) in stand_ins().into_iter().filter(|s| s.0 != url && !tried.contains(s)) {
        match send(&u, &m) {
            Ok(r) => {
                ok(&u, &m);
                return Ok((r, m));
            }
            Err(e) if unreachable(&e) => {
                failed(&u, &m);
                last = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    // Every fallback failed while the main model was being skipped: it, after all.
    if skipped {
        let r = send(url, model)?;
        main_ok();
        return Ok((r, model.to_string()));
    }
    Err(last.unwrap_or_else(|| "no model answered".into()))
}

/// Whether a failure means "the server isn't there" (worth the fallback),
/// not "the request was wrong" (the fallback would refuse it too).
pub fn unreachable(error: &str) -> bool {
    let e = error.to_lowercase();
    let status = e.split(|c: char| !c.is_ascii_digit()).find(|w| w.len() == 3).and_then(|w| w.parse::<u16>().ok());
    e.contains("error sending request") || e.contains("connection") || e.contains("timed out") || e.contains("dns") || status.is_some_and(|s| s >= 500 || s == 404 || s == 429)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings are process-wide: one test at a time.
    static LOCK: Mutex<()> = Mutex::new(());

    fn fb(url: &str, model: &str, background: bool) -> Option<Settings> {
        Some(Settings { url: url.into(), model: model.into(), background, price: Default::default() })
    }

    #[test]
    fn what_counts_as_the_main_model_being_down() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(unreachable("error sending request for url (http://172.99.99.11:8181/v1/chat/completions)"));
        assert!(unreachable("503 Service Unavailable: loading model"));
        assert!(unreachable("operation timed out"));
        assert!(!unreachable("400 Bad Request: context length exceeded"), "the fallback would refuse it too");
        configure(fb("http://gpu2:8080/v1/", "gemma", false), None);
        assert_eq!(target(), Some(("http://gpu2:8080/v1/chat/completions".into(), "gemma".into())));
        assert_eq!(light(), None, "background jobs stay on the main model unless asked");
        main_failed();
        assert!(skip_main());
        main_ok();
        assert!(!skip_main());
        configure(fb("http://gpu2:8080/v1/", "gemma", true), None);
        assert_eq!(light(), Some(("http://gpu2:8080/v1/chat/completions".into(), "gemma".into())));
        main_failed();
        assert_eq!(light(), None, "standing in for the main model: the ordinary path");
        main_ok();
        configure(None, None);
        main_failed();
        assert!(!skip_main(), "no fallback: always the main model");
        main_ok();
    }

    #[test]
    fn the_second_fallback_answers_when_the_first_cant_and_takes_background_jobs() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        configure(fb("http://qwen:8080/v1", "qwen", false), fb("http://gemma:8080/v1", "gemma", true));
        let (main, qwen, gemma) = ("http://main/v1/chat/completions", "http://qwen:8080/v1/chat/completions", "http://gemma:8080/v1/chat/completions");
        // Background jobs: the one that takes them, though it's second.
        assert_eq!(light(), Some((gemma.into(), "gemma".into())));
        assert_eq!(after(main, "m"), Some((qwen.into(), "qwen".into())));
        assert_eq!(after(qwen, "qwen"), Some((gemma.into(), "gemma".into())));
        assert_eq!(after(gemma, "gemma"), None, "the end of the line");
        // Main and the first both down: the second answers, and each is rested.
        let mut asked = Vec::new();
        let r = call(main, "m", false, |u, m| {
            asked.push(m.to_string());
            if u == gemma { Ok("hi") } else { Err("error sending request".to_string()) }
        });
        assert_eq!(r, Ok(("hi", "gemma".to_string())));
        assert_eq!(asked, ["m", "qwen", "gemma"]);
        assert!(skip_main() && stand_ins() == vec![(gemma.to_string(), "gemma".to_string())], "main and qwen rest for a while");
        // A request the model refuses isn't passed down the line.
        main_ok();
        configure(fb("http://qwen:8080/v1", "qwen", false), fb("http://gemma:8080/v1", "gemma", true));
        let mut asked = 0;
        let r: Result<(&str, String), String> = call(main, "m", false, |_, _| {
            asked += 1;
            Err("400 Bad Request: too long".to_string())
        });
        assert!(r.is_err() && asked == 1);
        // A small job: gemma first; it failing goes to the main model.
        let mut asked = Vec::new();
        let r = call(main, "m", true, |u, m| {
            asked.push(m.to_string());
            if u == main { Ok("ok") } else { Err("connection refused".to_string()) }
        });
        assert_eq!(r, Ok(("ok", "m".to_string())));
        assert_eq!(asked, ["gemma", "m"]);
        configure(None, None);
        main_ok();
    }
}
