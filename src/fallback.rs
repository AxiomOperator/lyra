//! A fallback for the chat model (`[fallback_model]`): when the main one
//! can't be reached or answers with a server error before it says anything,
//! the turn (or lyra's own background call) goes to the fallback instead, and
//! says so. After a failure the main model is left alone for a couple of
//! minutes (every turn would otherwise wait for it to time out first), then
//! tried again.

use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Settings {
    /// Its OpenAI-compatible base URL, e.g. `http://gpu2:8080/v1`.
    pub url: String,
    pub model: String,
    /// Its prices (`input_cost_per_mtok` …), for AI usage.
    #[serde(flatten, default)]
    pub price: crate::usage::Price,
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);
/// When the main model last failed.
static DOWN: Mutex<Option<Instant>> = Mutex::new(None);

/// How long the main model is skipped after a failure.
const REST: Duration = Duration::from_secs(120);

pub fn configure(s: Option<Settings>) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = s.filter(|s| !s.url.trim().is_empty() && !s.model.trim().is_empty());
}

pub fn settings() -> Option<Settings> {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The fallback's chat completions URL and model, if one is set up.
pub fn target() -> Option<(String, String)> {
    settings().map(|s| (format!("{}/chat/completions", s.url.trim_end_matches('/')), s.model))
}

/// Skip the main model for now (it failed a moment ago), when there's a fallback to use.
pub fn skip_main() -> bool {
    settings().is_some() && DOWN.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| t.elapsed() < REST)
}

/// The main model failed: the fallback answers for a while.
pub fn main_failed() {
    *DOWN.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
}

/// The main model answered: back to it.
pub fn main_ok() {
    *DOWN.lock().unwrap_or_else(|e| e.into_inner()) = None;
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

    #[test]
    fn what_counts_as_the_main_model_being_down() {
        assert!(unreachable("error sending request for url (http://172.99.99.11:8181/v1/chat/completions)"));
        assert!(unreachable("503 Service Unavailable: loading model"));
        assert!(unreachable("operation timed out"));
        assert!(!unreachable("400 Bad Request: context length exceeded"), "the fallback would refuse it too");
        configure(Some(Settings { url: "http://gpu2:8080/v1/".into(), model: "gemma".into(), price: Default::default() }));
        assert_eq!(target(), Some(("http://gpu2:8080/v1/chat/completions".into(), "gemma".into())));
        main_failed();
        assert!(skip_main());
        main_ok();
        assert!(!skip_main());
        configure(None);
        main_failed();
        assert!(!skip_main(), "no fallback: always the main model");
        main_ok();
    }
}
