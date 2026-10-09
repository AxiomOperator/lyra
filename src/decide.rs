//! The decision model (`[decide]`): a classifier such as Cloudflare's
//! clef-flash, served by llama-server on `/v1/systemone`. It reads a state and
//! typed questions and returns a probability for every option in one pass, in
//! milliseconds. lyra asks it the yes/no and pick-one questions it would
//! otherwise ask the chat model (routing, memory links, step checks, judging,
//! whether a turn is worth a review).
//!
//! Without `[decide]` nothing here is used: every caller asks the chat model
//! exactly as before. With it, a caller uses the answer only when the model is
//! sure enough (`confident`); otherwise, and whenever the endpoint fails, the
//! caller falls back to the chat model.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value, json};

/// `[decide]`.
#[derive(Clone, Deserialize)]
pub struct Settings {
    /// llama-server's base URL, e.g. `http://gpu:8091/v1` (`/systemone` is added).
    pub url: String,
    #[serde(default = "default_model")]
    pub model: String,
    /// Answers below this probability go to the chat model instead.
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f32,
    /// Longest state sent, in characters (its start and end are kept). The
    /// whole input must fit llama-server's batch (`-ub`, 512 tokens by default):
    /// raise both together.
    #[serde(default = "default_max_state_chars")]
    pub max_state_chars: usize,
    /// Its prices (`input_cost_per_mtok` …), for AI usage.
    #[serde(flatten, default)]
    pub price: crate::usage::Price,
}

fn default_max_state_chars() -> usize {
    1000
}

fn default_model() -> String {
    "clef-flash".into()
}

fn default_min_confidence() -> f32 {
    0.75
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);
/// After a failure the endpoint is left alone for a minute, so a model that's
/// down doesn't slow every message.
static DOWN_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
/// Lines for the Activity panel, taken by the app.
static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static CALLS: AtomicU64 = AtomicU64::new(0);
static FALLBACKS: AtomicU64 = AtomicU64::new(0);
static MILLIS: AtomicU64 = AtomicU64::new(0);

/// Set at startup and on reload; `None` turns it off.
pub fn configure(settings: Option<Settings>) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = settings;
    *DOWN_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The decision model's name, when one is set up.
pub fn model() -> Option<String> {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).as_ref().map(|s| s.model.clone())
}

/// `(answered, fell back to the chat model, average ms)` since start.
pub fn stats() -> (u64, u64, u64) {
    let calls = CALLS.load(Ordering::Relaxed);
    (calls, FALLBACKS.load(Ordering::Relaxed), MILLIS.load(Ordering::Relaxed).checked_div(calls).unwrap_or(0))
}

/// What happened since the last call, for the Activity panel.
pub fn take_notes() -> Vec<String> {
    std::mem::take(&mut *NOTES.lock().unwrap_or_else(|e| e.into_inner()))
}

fn note(text: String) {
    let mut notes = NOTES.lock().unwrap_or_else(|e| e.into_inner());
    if notes.len() < 50 {
        notes.push(text);
    }
}

/// One question.
#[derive(Debug, Clone)]
pub enum Question {
    /// Yes or no.
    Yes(String),
    /// One of named options: `(id, what it means)`.
    Choice(String, Vec<(String, String)>),
}

impl Question {
    fn to_json(&self) -> Value {
        match self {
            Question::Yes(instructions) => json!({ "type": "noul", "instructions": instructions }),
            Question::Choice(instructions, options) => {
                let criteria: Map<String, Value> = options.iter().map(|(id, d)| (id.clone(), json!(d))).collect();
                json!({ "type": "choice", "instructions": instructions, "criteria": criteria })
            }
        }
    }
}

/// The model's answer to one question.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub choice: String,
    pub confidence: f32,
}

impl Answer {
    /// A yes/no answer as a bool.
    pub fn yes(&self) -> Option<bool> {
        match self.choice.trim().to_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        }
    }
}

/// Read `/v1/systemone`'s answers.
pub fn parse(body: &Value) -> Result<HashMap<String, Answer>, String> {
    let answers = body["answers"].as_object().ok_or("no answers in the reply")?;
    let mut out = HashMap::new();
    for (id, a) in answers {
        // Yes/no comes as the probability of yes; its confidence is the margin
        // between yes and no, as a choice's is between its top two options.
        if let Some(p) = a["noul"].as_f64() {
            let p = p as f32;
            out.insert(id.clone(), Answer { choice: (p >= 0.5).to_string(), confidence: (2.0 * p - 1.0).abs() });
            continue;
        }
        // The choice, or else the most likely option.
        let probs: Vec<(String, f32)> =
            a["probabilities"].as_object().into_iter().flatten().filter_map(|(k, v)| v.as_f64().map(|p| (k.clone(), p as f32))).collect();
        let best = probs.iter().cloned().max_by(|a, b| a.1.total_cmp(&b.1));
        let choice = match &a["choice"] {
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            _ => best.as_ref().map(|b| b.0.clone()).ok_or_else(|| format!("no choice for {id}"))?,
        };
        let confidence = a["confidence"].as_f64().map(|c| c as f32).or_else(|| probs.iter().find(|p| p.0 == choice).map(|p| p.1)).unwrap_or(0.0);
        out.insert(id.clone(), Answer { choice, confidence });
    }
    Ok(out)
}

/// Ask the decision model. `None` when it isn't set up, is down, or failed
/// (the caller then asks the chat model); `what` names the decision in notes.
pub fn ask(what: &str, state: &str, questions: &[(String, Question)]) -> Option<HashMap<String, Answer>> {
    let settings = SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone()?;
    // Marked known down on the Status page: the chat model decides, nothing waits for it.
    if crate::known_down::is_down("decide") {
        FALLBACKS.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    {
        let mut down = DOWN_UNTIL.lock().unwrap_or_else(|e| e.into_inner());
        match *down {
            Some(t) if Instant::now() < t => {
                FALLBACKS.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            Some(_) => *down = None,
            None => {}
        }
    }
    let started = Instant::now();
    match call(&settings, state, questions) {
        Ok(answers) => {
            let ms = started.elapsed().as_millis() as u64;
            CALLS.fetch_add(1, Ordering::Relaxed);
            MILLIS.fetch_add(ms, Ordering::Relaxed);
            let shown: Vec<String> = questions
                .iter()
                .filter_map(|(id, _)| answers.get(id).map(|a| format!("{} ({:.0}%)", a.choice, a.confidence * 100.0)))
                .take(5)
                .collect();
            note(format!("{what}: {} — {} in {ms} ms", shown.join(", "), settings.model));
            Some(answers)
        }
        // Too much for its batch: only this question goes to the chat model.
        Err(e) if e.contains("too large") => {
            FALLBACKS.fetch_add(1, Ordering::Relaxed);
            note(format!("{what}: too long for the decision model's batch, left to the chat model (raise llama-server's -b/-ub and [decide] max_state_chars)"));
            None
        }
        Err(e) => {
            FALLBACKS.fetch_add(1, Ordering::Relaxed);
            let mut down = DOWN_UNTIL.lock().unwrap_or_else(|e| e.into_inner());
            if down.is_none() {
                note(format!("decision model unavailable ({e}); using the chat model for a minute"));
            }
            *down = Some(Instant::now() + Duration::from_secs(60));
            None
        }
    }
}

/// For Status: one tiny question, timed, without counting or logging it.
pub fn probe() -> Option<Result<(u64, String), String>> {
    let settings = SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone()?;
    let started = Instant::now();
    Some(call(&settings, "The server answered the health check.", &[("q".into(), Question::Yes("Did the server answer?".into()))]).map(|_| (started.elapsed().as_millis() as u64, settings.model.clone())))
}

pub fn url() -> Option<String> {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).as_ref().map(|s| s.url.clone())
}

/// The answer to `id`, if the model was sure enough; counts a fallback when not.
pub fn confident<'a>(answers: &'a HashMap<String, Answer>, id: &str) -> Option<&'a Answer> {
    let min = SETTINGS.read().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(1.0, |s| s.min_confidence);
    let a = answers.get(id).filter(|a| a.confidence >= min);
    if a.is_none() {
        FALLBACKS.fetch_add(1, Ordering::Relaxed);
    }
    a
}

/// One yes/no question; `Some` only when the model answered it confidently.
pub fn yes(what: &str, state: &str, question: &str) -> Option<(bool, f32)> {
    let answers = ask(what, state, &[("q".into(), Question::Yes(question.into()))])?;
    let a = confident(&answers, "q")?;
    a.yes().map(|y| (y, a.confidence))
}

/// The start and end of a long state (a transcript's latest turn is at the end).
pub fn clip(state: &str, max: usize) -> String {
    let n = state.chars().count();
    if n <= max {
        return state.to_string();
    }
    let head: String = state.chars().take(max / 3).collect();
    let tail: String = state.chars().skip(n - (max - max / 3)).collect();
    format!("{head}\n…\n{tail}")
}

fn call(s: &Settings, state: &str, questions: &[(String, Question)]) -> Result<HashMap<String, Answer>, String> {
    let state = clip(state, s.max_state_chars);
    let qs: Map<String, Value> = questions.iter().map(|(id, q)| (id.clone(), q.to_json())).collect();
    let body = json!({ "model": s.model, "state": state, "questions": qs });
    let url = format!("{}/systemone", s.url.trim_end_matches('/'));
    let started = Instant::now();
    let resp = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?
        .post(&url)
        .json(&body)
        .send()
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{status}: {}", resp.text().unwrap_or_default().chars().take(200).collect::<String>()));
    }
    let reply: Value = resp.json().map_err(|e| e.to_string())?;
    crate::usage::record("decision", &s.model, reply["usage"]["input_tokens"].as_u64().unwrap_or(0), 0, reply["usage"]["output_tokens"].as_u64().unwrap_or(0), started.elapsed().as_millis() as u64);
    let answers = parse(&reply)?;
    match questions.iter().find(|(id, _)| !answers.contains_key(id)) {
        Some((id, _)) => Err(format!("no answer for {id}")),
        None => Ok(answers),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// The global settings are shared by every test that touches them.
    pub(crate) static LOCK: Mutex<()> = Mutex::new(());

    /// A one-shot fake llama-server answering `/v1/systemone` with `reply`;
    /// returns its base URL and the request it received.
    pub(crate) fn fake(reply: Value) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 65536];
            let mut got = Vec::new();
            loop {
                let n = s.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let len = head.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                    if body.len() >= len {
                        break;
                    }
                }
            }
            let body = reply.to_string();
            write!(s, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
            String::from_utf8_lossy(&got).to_string()
        });
        (url, handle)
    }

    pub(crate) fn settings(url: &str) -> Settings {
        Settings { url: url.into(), model: "clef-flash".into(), min_confidence: 0.75, max_state_chars: 1000, price: Default::default() }
    }

    #[test]
    fn answers_are_read_whatever_form_the_choice_takes() {
        let body = json!({ "model": "clef-flash", "answers": {
            "dept": { "choice": "technical", "confidence": 0.93, "probabilities": { "technical": 0.93, "billing": 0.07 } },
            "outage": { "choice": true, "confidence": 0.81 },
            // As llama-server answers yes/no: the probability of yes.
            "down": { "type": "noul", "noul": 0.9 },
            "fine": { "type": "noul", "noul": 0.2 },
            "urgency": { "probabilities": { "0": 0.1, "1": 0.2, "2": 0.7 } },
        }});
        let a = parse(&body).unwrap();
        assert_eq!(a["dept"], Answer { choice: "technical".into(), confidence: 0.93 });
        assert_eq!(a["outage"].yes(), Some(true));
        assert_eq!(a["down"].yes(), Some(true));
        assert!((a["down"].confidence - 0.8).abs() < 1e-6, "the margin between yes and no");
        assert_eq!(a["fine"].yes(), Some(false));
        assert_eq!((a["urgency"].choice.as_str(), a["urgency"].confidence), ("2", 0.7), "the most likely option");
        assert!(parse(&json!({ "error": "x" })).is_err());
        let long = format!("start {} end", "x".repeat(5000));
        let short = clip(&long, 300);
        assert!(short.starts_with("start") && short.ends_with("end") && short.chars().count() <= 303);
    }

    #[test]
    fn nothing_is_asked_without_decide_and_a_failure_falls_back() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        configure(None);
        assert!(yes("test", "state", "anything?").is_none(), "no [decide]: the caller uses the chat model");

        let (url, server) = fake(json!({ "answers": { "q": { "type": "noul", "noul": 0.95 } } }));
        configure(Some(settings(&url)));
        assert_eq!(yes("test", "the disk is full", "is something broken?").map(|(y, c)| (y, (c * 100.0).round())), Some((true, 90.0)));
        let request: Value = serde_json::from_str(server.join().unwrap().split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(request["questions"]["q"]["type"], "noul");
        assert_eq!(request["state"], "the disk is full");

        let (url, _server) = fake(json!({ "answers": { "q": { "type": "noul", "noul": 0.7 } } }));
        configure(Some(settings(&url)));
        assert!(yes("test", "s", "q?").is_none(), "not sure enough: the chat model decides");

        configure(Some(settings("http://127.0.0.1:9/v1")));
        assert!(yes("test", "s", "q?").is_none(), "down: the chat model decides");
        assert!(take_notes().iter().any(|n| n.contains("unavailable")));
        configure(None);
    }
}
