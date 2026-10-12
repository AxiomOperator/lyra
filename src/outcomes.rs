//! How each reply went, for everything that learns from it (skills, memories,
//! agents' delegations, evolution): the person's rating (👍 / 👎, with what
//! was wrong), the model's reading of what they said next, and signals that
//! cost them nothing (asking again, Stop, the tool-call limit, Keep going,
//! copying or reading the reply aloud). Each reply keeps one verdict: a
//! stronger one replaces a weaker one, never the other way round.
//!
//! Tool failures are read here too, from the result's structure (`ok: false`,
//! a top-level `error`), never from the word "error" appearing somewhere.

use std::time::Duration;

use chrono::{DateTime, Utc};
use lyra_learning::{SkillOutcome, Uuid};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How sure a verdict is: a stronger one replaces a weaker one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strength {
    /// A copy or a read-aloud: a hint the reply was useful.
    Mild,
    /// Read from their next message (the decision model, the fallback, words).
    Judged,
    /// Something they did: asked again, stopped it, Keep going, the limit.
    Signal,
    /// Their own 👍 / 👎 (or `/outcome`).
    Rated,
}

/// How a reply went, and why lyra thinks so.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub outcome: SkillOutcome,
    pub strength: Strength,
    /// Short, for the Activity panel ("they corrected the date").
    pub reason: String,
    /// It was wrong and they said how: their words are the evolver's evidence.
    #[serde(default)]
    pub correction: bool,
    /// What they said about it (their next message, or the note with a 👎).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
}

impl Verdict {
    pub fn new(outcome: SkillOutcome, strength: Strength, reason: impl Into<String>) -> Self {
        Self { outcome, strength, reason: reason.into(), correction: false, said: None }
    }

    /// Whether this one should replace `before` (a rating always may: they changed their mind).
    pub fn replaces(&self, before: Option<&Verdict>) -> bool {
        match before {
            None => true,
            Some(b) => self.strength == Strength::Rated || self.strength > b.strength,
        }
    }

    /// "good", "bad" or "partial", for the app's buttons.
    pub fn word(&self) -> &'static str {
        match self.outcome {
            SkillOutcome::Success => "good",
            SkillOutcome::Failure => "bad",
            _ => "partial",
        }
    }
}

/// A finished reply, waiting for how it went (kept with the reply in the
/// saved conversation, so a resumed one can still be judged or rated).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRef {
    /// The run its skills' and memories' use was recorded under.
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default)]
    pub memories: usize,
    /// Its evolution telemetry record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evo: Option<Uuid>,
    /// When the reply finished (asking the same again soon after is a bad sign).
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
}

impl RunRef {
    pub fn new(id: Uuid, skills: Vec<String>, memories: usize, evo: Option<Uuid>) -> Self {
        Self { id, skills, memories, evo, at: Utc::now(), verdict: None }
    }
}

/// What a `/outcome` word or an app button means.
pub fn rating(word: &str) -> Option<SkillOutcome> {
    Some(match word {
        "good" | "success" | "up" | "+" => SkillOutcome::Success,
        "bad" | "failure" | "down" | "-" => SkillOutcome::Failure,
        "partial" => SkillOutcome::Partial,
        _ => return None,
    })
}

/// The text the app's Continue / Keep going sends, and what people type for it.
const KEEP_GOING: &[&str] = &["continue where you left off", "continue", "keep going", "go on", "carry on", "please continue"];

/// The message asks to carry on from where the reply stopped.
pub fn keep_going(next: &str) -> bool {
    let t = words(next).join(" ");
    KEEP_GOING.contains(&t.as_str())
}

/// The person says they'd told lyra this before ("I told you…", "as I said").
pub fn told_before(message: &str) -> bool {
    const SAID: &[&str] = &["i told you", "i already told you", "i've told you", "i have told you", "told you already", "told you before", "as i said", "like i said", "i mentioned", "i already said", "i said before", "you should know this", "you should remember"];
    let t = words(message).join(" ");
    let t = format!(" {t} ");
    SAID.iter().any(|p| t.contains(&format!(" {} ", words(p).join(" "))))
}

/// How long after a reply asking the same again counts as "that didn't do it".
pub const ASKED_AGAIN: Duration = Duration::from_secs(120);

/// The same question again: most of its words are the earlier one's.
pub fn same_question(before: &str, now: &str) -> bool {
    let (a, b) = (words(before), words(now));
    if a.len() < 3 || b.len() < 3 {
        return false;
    }
    let shared = b.iter().filter(|w| a.contains(w)).count();
    let union = a.len() + b.len() - shared;
    shared as f32 / union.max(1) as f32 >= 0.7
}

/// Two short texts that say much the same (most of their words shared).
pub fn similar(a: &str, b: &str) -> bool {
    let (a, b) = (words(a), words(b));
    let shared = b.iter().filter(|w| a.contains(w)).count();
    let union = a.len() + b.len() - shared;
    union > 0 && shared as f32 / union as f32 >= 0.6
}

fn words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    out.dedup();
    out
}

/// What the person's next message says about the reply before it: something
/// they did first (asked the same again, Keep going), then the decision
/// model, then the fallback model (`light`: chat URL and model, for a light
/// job), and only last the word lists. `None`: no telling (a new topic).
pub fn judge(asked: &str, reply: &str, next: &str, since: Duration, light: Option<(&str, &str)>) -> Option<Verdict> {
    if keep_going(next) {
        return Some(Verdict::new(SkillOutcome::Partial, Strength::Signal, "they asked it to keep going"));
    }
    if since <= ASKED_AGAIN && same_question(asked, next) {
        let mut v = Verdict::new(SkillOutcome::Failure, Strength::Signal, "they asked the same again");
        v.said = Some(clip(next, 500));
        return Some(v);
    }
    let state = format!("The person asked: {}\n\nlyra replied: {}\n\nThen the person said: {}", clip(asked, 600), clip(reply, 1500), clip(next, 600));
    let class = by_decision_model(&state).or_else(|| light.and_then(|(url, model)| by_light_model(url, model, &state))).or_else(|| by_words(next).map(|c| (c, "from its words".to_string())));
    let (class, why) = class?;
    let (outcome, correction) = match class {
        Class::Success => (SkillOutcome::Success, false),
        Class::Partial => (SkillOutcome::Partial, false),
        Class::Correction => (SkillOutcome::Failure, true),
        Class::NewTopic | Class::Unclear => return None,
    };
    let mut v = Verdict::new(outcome, Strength::Judged, why);
    v.correction = correction;
    v.said = Some(clip(next, 500));
    Some(v)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Class {
    Success,
    Partial,
    Correction,
    NewTopic,
    Unclear,
}

const CLASSES: &[(&str, &str, Class)] = &[
    ("success", "satisfied: they thank it, accept it, or build on it", Class::Success),
    ("partial", "partly right: they ask for a part to be fixed, or for more of it", Class::Partial),
    ("correction", "wrong or not what they wanted: they correct it, complain, or ask again differently", Class::Correction),
    ("new_topic", "they moved on to something unrelated without saying anything about the reply", Class::NewTopic),
    ("unclear", "it can't be told from their message", Class::Unclear),
];

fn class_of(id: &str) -> Option<Class> {
    let id = id.trim().to_lowercase().replace(['-', ' '], "_");
    CLASSES.iter().find(|(name, ..)| *name == id).map(|(.., c)| *c)
}

fn describe(c: Class) -> &'static str {
    CLASSES.iter().find(|(.., x)| *x == c).map_or("", |(_, d, _)| d)
}

fn by_decision_model(state: &str) -> Option<(Class, String)> {
    use crate::decide::{Question, ask, confident};
    let options = CLASSES.iter().map(|(id, d, _)| (id.to_string(), d.to_string())).collect();
    let q = Question::Choice("What does the person's last message say about lyra's reply?".into(), options);
    let answers = ask("how did the reply go?", state, &[("verdict".into(), q)])?;
    let a = confident(&answers, "verdict")?;
    let c = class_of(&a.choice)?;
    Some((c, format!("{} (decision model, {:.0}%)", describe(c), a.confidence * 100.0)))
}

pub(crate) const JUDGE_PROMPT: &str = "\
You read what a person said right after an AI assistant's reply, and decide what it says about that reply. \
Answer with JSON only: {\"verdict\": \"success\" | \"partial\" | \"correction\" | \"new_topic\" | \"unclear\", \"reason\": \"a few words\"}.\n\
- success: they're satisfied: they thank it, accept it, or carry on building on it.\n\
- partial: it was partly right: they ask for one part fixed, or for more of it.\n\
- correction: it was wrong or not what they wanted: they correct it, complain, or ask again differently.\n\
- new_topic: they moved on to something unrelated and said nothing about the reply.\n\
- unclear: you can't tell.\n\
\"Thanks, now do Y instead\" is success (Y is a new request), not a correction. Judge the reply, not the new request.";

fn by_light_model(url: &str, model: &str, state: &str) -> Option<(Class, String)> {
    let (reply, _) = crate::learn::complete_light_short(url, model, JUDGE_PROMPT, state, 200).ok()?;
    let v = json_in(&reply)?;
    let c = class_of(v["verdict"].as_str()?)?;
    let why = v["reason"].as_str().map(|r| clip(r, 120)).filter(|r| !r.is_empty()).unwrap_or_else(|| describe(c).to_string());
    Some((c, format!("{why} (fallback model)")))
}

/// The first JSON object in a model's reply.
fn json_in(reply: &str) -> Option<Value> {
    let (start, end) = (reply.find('{')?, reply.rfind('}')?);
    serde_json::from_str(reply.get(start..=end)?).ok()
}

/// Last resort, whole words only: "no" isn't in "know", "great" isn't
/// "greater", and thanks before a new request is thanks.
fn by_words(next: &str) -> Option<Class> {
    const CORRECTION: &[&str] = &["that's wrong", "that is wrong", "not what i", "you should have", "doesn't work", "didn't work", "still broken", "still fails", "that's not right", "not right", "wrong", "nope", "no"];
    const PRAISE: &[&str] = &["thanks", "thank you", "perfect", "that worked", "it worked", "works now", "that works", "great", "awesome", "exactly", "nice", "spot on", "worked"];
    let w = words(next);
    let text = format!(" {} ", w.join(" "));
    // The phrases split the same way ("didn't" → "didn t").
    let has = |p: &&str| text.contains(&format!(" {} ", words(p).join(" ")));
    // How it starts says the most ("no, …" / "thanks, …").
    let first: String = w.iter().take(4).cloned().collect::<Vec<_>>().join(" ");
    let first = format!(" {first} ");
    let starts = |p: &&str| first.contains(&format!(" {} ", words(p).join(" ")));
    // A phrase that says it plainly ("didn't work") wins, even after thanks.
    if CORRECTION.iter().filter(|p| p.contains(' ')).any(has) {
        return Some(Class::Correction);
    }
    if PRAISE.iter().any(starts) {
        return Some(Class::Success);
    }
    if CORRECTION.iter().any(starts) {
        return Some(Class::Correction);
    }
    PRAISE.iter().any(has).then_some(Class::Success)
}

fn clip(text: &str, max: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max { t.to_string() } else { format!("{}…", t.chars().take(max).collect::<String>()) }
}

// ---- credit (which skills a reply followed)

/// Of the skills in a reply's prompt (`skills`: name and what it says), the
/// ones the reply actually followed: the decision model when it's sure of
/// each, else the fallback model (`light`). `None`: no telling, so every one
/// gets the credit, as before.
pub fn followed(reply: &str, tools: &[String], skills: &[(String, String)], light: Option<(&str, &str)>) -> Option<Vec<String>> {
    if skills.is_empty() {
        return Some(Vec::new());
    }
    let did = format!("lyra's reply: {}\n\nTools it used: {}", clip(reply, 1200), if tools.is_empty() { "none".to_string() } else { tools.join(", ") });
    by_decision_model_credit(&did, skills).or_else(|| light.and_then(|(url, model)| by_light_model_credit(url, model, &did, skills)))
}

fn by_decision_model_credit(did: &str, skills: &[(String, String)]) -> Option<Vec<String>> {
    use crate::decide::{Question, ask, confident};
    let questions: Vec<(String, Question)> = skills
        .iter()
        .enumerate()
        .map(|(i, (name, what))| (format!("s{i}"), Question::Yes(format!("Did the reply follow the skill \"{name}\": {}?", clip(what, 200)))))
        .collect();
    let answers = ask("which skills did it follow?", did, &questions)?;
    let mut out = Vec::new();
    for (i, (name, _)) in skills.iter().enumerate() {
        if confident(&answers, &format!("s{i}"))?.yes()? {
            out.push(name.clone());
        }
    }
    Some(out)
}

const CREDIT_PROMPT: &str = "\
You're given an AI assistant's reply, the tools it used, and the learned skills that were in its instructions. \
Say which skills the reply actually followed (it did what the skill says), not which were merely relevant. \
Answer with JSON only: {\"followed\": [\"skill name\", …]} (an empty list when it followed none).";

fn by_light_model_credit(url: &str, model: &str, did: &str, skills: &[(String, String)]) -> Option<Vec<String>> {
    let list: Vec<String> = skills.iter().map(|(n, w)| format!("- {n}: {}", clip(w, 300))).collect();
    let (reply, _) = crate::learn::complete_light_short(url, model, CREDIT_PROMPT, &format!("{did}\n\nSkills:\n{}", list.join("\n")), 200).ok()?;
    let v = json_in(&reply)?;
    let named: Vec<String> = v["followed"].as_array()?.iter().filter_map(Value::as_str).map(str::to_string).collect();
    // Only names it was given.
    Some(skills.iter().filter(|(n, _)| named.iter().any(|x| x.trim().eq_ignore_ascii_case(n))).map(|(n, _)| n.clone()).collect())
}

// ---- tool failures

/// What kind of failure a tool result is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    /// Not allowed: policy, a refusal, the person said no.
    Denied,
    NotFound,
    Timeout,
    /// The call itself was wrong: a missing or bad argument, an unknown tool.
    BadCall,
    /// The service or machine couldn't be reached.
    Unavailable,
    Failed,
}

impl FailKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FailKind::Denied => "denied",
            FailKind::NotFound => "not_found",
            FailKind::Timeout => "timeout",
            FailKind::BadCall => "bad_call",
            FailKind::Unavailable => "unavailable",
            FailKind::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "denied" => FailKind::Denied,
            "not_found" => FailKind::NotFound,
            "timeout" => FailKind::Timeout,
            "bad_call" => FailKind::BadCall,
            "unavailable" => FailKind::Unavailable,
            "failed" => FailKind::Failed,
            _ => return None,
        })
    }

    /// The kind of an error message.
    fn of(message: &str) -> Self {
        let m = message.to_lowercase();
        let any = |words: &[&str]| words.iter().any(|w| m.contains(w));
        if any(&["not allowed", "denied", "forbidden", "refused:", "said no", "needs the user's approval", "for admins", "skipped this step", "is off", "isn't set up"]) {
            FailKind::Denied
        } else if any(&["timed out", "timeout", "took too long"]) {
            FailKind::Timeout
        } else if any(&["not found", "404", "no such", "doesn't exist", "does not exist", "unknown id"]) {
            FailKind::NotFound
        } else if any(&["unknown tool", "missing", "required", "invalid", "expected", "bad argument", "not a tool", "couldn't parse", "can't parse"]) {
            FailKind::BadCall
        } else if any(&["connection", "unreachable", "refused", "offline", "unavailable", "couldn't reach", "is down", "502", "503"]) {
            FailKind::Unavailable
        } else {
            FailKind::Failed
        }
    }
}

/// A tool call that failed: its kind and message.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub kind: FailKind,
    pub message: String,
}

/// Whether a tool result is a failure, from its structure: `"ok": false`, or a
/// non-empty top-level `"error"`. A result that only mentions an error (a
/// mail about one, a log line) isn't.
pub fn failure(content: &str) -> Option<Failure> {
    let v: Value = serde_json::from_str(content.trim()).ok()?;
    let obj = v.as_object()?;
    let message = match obj.get("error") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
        Some(Value::Object(o)) => o.get("message").and_then(Value::as_str).unwrap_or("failed").to_string(),
        _ if obj.get("ok") == Some(&Value::Bool(false)) => obj.get("message").and_then(Value::as_str).unwrap_or("failed").to_string(),
        _ => return None,
    };
    let kind = obj.get("error_kind").and_then(Value::as_str).and_then(FailKind::parse).unwrap_or_else(|| FailKind::of(&message));
    Some(Failure { kind, message })
}

/// A tool's result as it goes back to the model: a failure says so in its
/// structure (`"ok": false, "error_kind": …`), for the detectors.
pub fn mark(content: String) -> String {
    let Some(f) = failure(&content) else { return content };
    let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(content.trim()) else { return content };
    if obj.contains_key("error_kind") && obj.get("ok") == Some(&Value::Bool(false)) {
        return content;
    }
    obj.insert("ok".into(), json!(false));
    obj.insert("error_kind".into(), json!(f.kind.as_str()));
    Value::Object(obj).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_are_whole_words_and_thanks_before_a_new_request_is_thanks() {
        assert_eq!(by_words("thanks, now do the same for Go instead"), Some(Class::Success));
        assert_eq!(by_words("no, the meeting is on Tuesday"), Some(Class::Correction));
        assert_eq!(by_words("that's wrong, it's port 8080"), Some(Class::Correction));
        assert_eq!(by_words("is 5 greater than 3?"), None, "greater isn't great");
        assert_eq!(by_words("I know the answer"), None, "know isn't no");
        assert_eq!(by_words("what should be the name of it?"), None);
        assert_eq!(by_words("use the other one instead"), None, "instead alone says nothing");
        assert_eq!(by_words("ok great"), Some(Class::Success));
        assert_eq!(by_words("Thanks but that didn't work"), Some(Class::Correction));
        assert_eq!(by_words("Now do the same for Go"), None);
    }

    #[test]
    fn i_told_you_already_is_a_memory_miss() {
        assert!(told_before("I told you, the server is in rack 3"));
        assert!(told_before("as I said, it's port 8080"));
        assert!(told_before("I've told you this before!"));
        assert!(!told_before("tell me about the server"));
        assert!(!told_before("she said it was fine"));
    }

    #[test]
    fn asking_the_same_again_and_keep_going_are_read_before_any_model() {
        let v = judge("what's the weather in Austin today", "It's sunny.", "what is the weather in Austin today?", Duration::from_secs(30), None).unwrap();
        assert_eq!((v.outcome, v.strength), (SkillOutcome::Failure, Strength::Signal));
        // Too late to count as "that didn't do it".
        let later = judge("what's the weather in Austin today", "It's sunny.", "what is the weather in Austin today?", Duration::from_secs(600), None);
        assert!(later.is_none_or(|v| v.strength != Strength::Signal));
        let v = judge("fix the build", "Fixed two of three errors.", "Continue where you left off.", Duration::from_secs(5), None).unwrap();
        assert_eq!((v.outcome, v.strength), (SkillOutcome::Partial, Strength::Signal));
        assert!(!same_question("send the report", "send the invoice to Sam instead"));
    }

    #[test]
    fn with_no_model_the_words_decide_and_a_new_topic_says_nothing() {
        let _guard = crate::decide::tests::LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::decide::configure(None);
        let v = judge("when is the meeting", "Tuesday at 3.", "no, it moved to Wednesday", Duration::from_secs(400), None).unwrap();
        assert_eq!(v.outcome, SkillOutcome::Failure);
        assert!(v.correction);
        assert_eq!(v.said.as_deref(), Some("no, it moved to Wednesday"));
        assert!(judge("when is the meeting", "Tuesday at 3.", "draft a note to the team about lunch", Duration::from_secs(400), None).is_none());
    }

    #[test]
    fn the_decision_model_reads_the_next_message_when_it_is_sure() {
        let _guard = crate::decide::tests::LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (url, server) = crate::decide::tests::fake(json!({ "answers": { "verdict": { "choice": "correction", "confidence": 0.9 } } }));
        crate::decide::configure(Some(crate::decide::tests::settings(&url)));
        let v = judge("book the room", "Booked room 4 for Monday.", "it was meant to be Tuesday", Duration::from_secs(400), None);
        crate::decide::configure(None);
        let sent = server.join().unwrap();
        let v = v.unwrap();
        assert_eq!((v.outcome, v.strength, v.correction), (SkillOutcome::Failure, Strength::Judged, true));
        assert!(v.reason.contains("decision model"), "{}", v.reason);
        assert!(sent.contains("Then the person said: it was meant to be Tuesday"));
    }

    #[test]
    fn only_the_skills_a_reply_followed_get_the_credit() {
        let _guard = crate::decide::tests::LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let skills = vec![("restart-printer".to_string(), "Power-cycle, then clear the queue".to_string()), ("vpn-reset".to_string(), "Re-import the profile".to_string())];
        // The decision model, sure of each.
        let (url, server) = crate::decide::tests::fake(json!({ "answers": { "s0": { "type": "noul", "noul": 0.97 }, "s1": { "type": "noul", "noul": 0.03 } } }));
        crate::decide::configure(Some(crate::decide::tests::settings(&url)));
        assert_eq!(followed("I power-cycled it and cleared the queue.", &["shell".into()], &skills, None), Some(vec!["restart-printer".to_string()]));
        assert!(server.join().unwrap().contains("restart-printer"));
        // Down: the fallback model names them (only names it was given count).
        crate::decide::configure(Some(crate::decide::tests::settings("http://127.0.0.1:9/v1")));
        let (chat, _req) = crate::decide::tests::fake(json!({ "choices": [{ "message": { "role": "assistant", "content": "{\"followed\": [\"VPN-reset\", \"made-up\"]}" } }] }));
        assert_eq!(followed("Re-import the VPN profile.", &[], &skills, Some((&format!("{chat}/chat/completions"), "m"))), Some(vec!["vpn-reset".to_string()]));
        // Nobody can tell: no answer (everyone gets the credit).
        assert_eq!(followed("x", &[], &skills, None), None);
        crate::decide::configure(None);
        assert_eq!(followed("x", &[], &[], None), Some(vec![]));
    }

    #[test]
    fn a_stronger_verdict_replaces_a_weaker_one_and_a_rating_always_does() {
        let mild = Verdict::new(SkillOutcome::Success, Strength::Mild, "copied");
        let judged = Verdict::new(SkillOutcome::Failure, Strength::Judged, "corrected");
        let rated = Verdict::new(SkillOutcome::Success, Strength::Rated, "👍");
        assert!(judged.replaces(Some(&mild)));
        assert!(!mild.replaces(Some(&judged)));
        assert!(!judged.replaces(Some(&judged.clone())));
        assert!(rated.replaces(Some(&rated.clone())), "they changed their mind");
        assert!(!judged.replaces(Some(&rated)));
    }

    #[test]
    fn failures_come_from_the_structure_not_the_word() {
        assert_eq!(failure(r#"{"error":"connection refused"}"#).unwrap().kind, FailKind::Unavailable);
        assert_eq!(failure(r#"{"error":"calendar_create is not allowed by policy"}"#).unwrap().kind, FailKind::Denied);
        assert_eq!(failure(r#"{"ok":false,"message":"no such file"}"#).unwrap().kind, FailKind::NotFound);
        assert!(failure(r#"{"subject":"Re: the error in the report","body":"\"error\" again"}"#).is_none());
        assert!(failure(r#"{"error":null,"result":"ok"}"#).is_none());
        assert!(failure("plain text mentioning an error").is_none());
        let marked = mark(r#"{"error":"unknown tool foo"}"#.to_string());
        let v: Value = serde_json::from_str(&marked).unwrap();
        assert_eq!((v["ok"].clone(), v["error_kind"].clone()), (json!(false), json!("bad_call")));
        assert_eq!(mark(r#"{"result":"fine"}"#.to_string()), r#"{"result":"fine"}"#);
        assert_eq!(failure(&marked).unwrap().kind, FailKind::BadCall);
    }
}

#[cfg(test)]
mod app_tests {
    use super::{Strength, Verdict};
    use crate::*;
    use std::time::{Duration, Instant};

    fn app(home: &std::path::Path, rt: &tokio::runtime::Runtime) -> (App, Arc<evolve::Evolution>) {
        let store = rt.block_on(lyra_evolution::EvolutionStore::in_memory()).unwrap();
        let manager = lyra_evolution::EvolutionManager::with_store(store, home, rt.handle().clone(), lyra_evolution::Settings::default()).unwrap();
        let evolution = Arc::new(evolve::Evolution::new(manager, rt.handle().clone(), None, 3));
        let off = || Err("off".to_string());
        let services = Services {
            tools: None, memory_status: off(), learning: None, learning_status: off(), engine: None, planning_status: off(),
            evolution: Some(evolution.clone()), evolution_status: Ok("test".into()), caps: None, goals: None, agents: None, agents_status: off(),
        };
        let mut app = App::new(config::Config::default(), context::Context::default(), services);
        app.session_id = sessions::new_id();
        (app, evolution)
    }

    /// One turn, answered: the person's message and lyra's reply.
    fn turn(app: &mut App, asked: &str, answer: &str) {
        app.messages.push(Message::new("user", asked.into()));
        app.run = Some(Uuid::new_v4());
        app.waiting = true;
        app.handle(StreamEvent::Token(answer.into()));
        app.handle(StreamEvent::Done(Stats { ttft: None, elapsed: Duration::from_millis(5), input: 10, cached: 0, output: 5, estimated: false }));
    }

    /// The run's record, once `ready` holds (it's written off the loop).
    fn run_when(ev: &evolve::Evolution, id: Uuid, ready: impl Fn(&lyra_evolution::RunRecord) -> bool) -> lyra_evolution::RunRecord {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(r) = ev.manager.runs(50).unwrap().into_iter().find(|r| r.id == id).filter(|r| ready(r)) {
                return r;
            }
            assert!(Instant::now() < until, "the run never got there");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_rating_reaches_evolution_survives_a_resume_and_outranks_a_guess() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        config::with_test_home(|home| {
            let (mut app, ev) = app(home, &rt);
            turn(&mut app, "what's the weather in Austin today", "It's sunny.");
            let run = app.messages.last().unwrap().run.clone().expect("the reply keeps its run");
            let evo = run.evo.expect("evolution recorded the turn");
            assert_eq!(run_when(&ev, evo, |_| true).task, "what's the weather in Austin today");

            // 👎 with what was wrong: everything learns from it.
            assert!(app.rate(None, "bad", "it was raining").unwrap().contains("bad"));
            let r = run_when(&ev, evo, |r| r.outcome == lyra_evolution::RunOutcome::Failure);
            assert!(r.corrected);
            assert_eq!(r.feedback.as_deref(), Some("it was raining"));
            assert!(app.activity.iter().any(|a| a.text.contains("reply rated bad")), "into Activity");

            // A guess afterwards doesn't undo what they said.
            app.apply_verdict(run.id, Verdict::new(SkillOutcome::Success, Strength::Judged, "they said thanks"));
            let kept = app.messages.last().unwrap().run.as_ref().unwrap().verdict.clone().unwrap();
            assert_eq!((kept.outcome, kept.strength), (SkillOutcome::Failure, Strength::Rated));

            // Resumed later: the rating is still there, and the reply can still be rated.
            let saved = sessions::find_for(&sessions::dir().unwrap(), &app.session_id, &app.owner).unwrap();
            let (mut again, _) = self::app(home, &rt);
            again.resume_session(saved);
            let last = again.last_run.clone().expect("the last reply can still be judged");
            assert_eq!(last.id, run.id);
            assert_eq!(last.verdict.map(|v| v.strength), Some(Strength::Rated));
            // They changed their mind.
            again.rate(None, "good", "").unwrap();
            let v = again.messages.iter().rev().find_map(|m| m.run.as_ref()).unwrap().verdict.clone().unwrap();
            assert_eq!(v.outcome, SkillOutcome::Success);
            // A reply the owner liked joins lyra's tests, with the answer they liked.
            let (tests, _) = lyra_evolution::golden::list(ev.manager.home());
            assert_eq!(tests.len(), 1);
            assert_eq!((tests[0].task.as_str(), tests[0].reference.as_deref(), tests[0].source.as_str()), ("what's the weather in Austin today", Some("It's sunny."), "rated"));
            // An admin adds another reply by hand; once is enough.
            turn(&mut again, "and tomorrow in Austin?", "Rain after noon.");
            assert!(again.rate(None, "test", "").unwrap().contains("added to lyra's tests"));
            assert!(again.rate(None, "test", "").unwrap().contains("already"));
            let (tests, _) = lyra_evolution::golden::list(ev.manager.home());
            assert_eq!(tests.len(), 2);
            assert!(tests.iter().any(|t| t.task == "and tomorrow in Austin?" && t.history.iter().any(|(_, x)| x == "what's the weather in Austin today")), "with the turns it follows");
            again.admin = false;
            assert!(again.rate(None, "test", "").unwrap_err().contains("for admins"));
        });
    }

    #[test]
    fn the_talk_page_switches_voice_replies_without_a_trace_in_the_chat() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        config::with_test_home(|home| {
            let (mut app, _) = app(home, &rt);
            for (line, on) in [("/talk on", true), ("/talk off", false)] {
                app.input = line.into();
                app.send();
                assert_eq!(app.voice, on);
            }
            assert!(app.messages.is_empty(), "nothing in the conversation");
            assert!(!app.waiting);
        });
    }

    #[test]
    fn reviews_wait_in_a_queue_one_per_turn() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        config::with_test_home(|home| {
            let (mut app, _) = app(home, &rt);
            let manager = rt.block_on(lyra_learning::SkillManager::open(&home.join("skills"), Default::default())).unwrap();
            app.learning = Some(Arc::new(learn::Learning::new(Arc::new(manager), rt.handle().clone(), "skills".into())));
            turn(&mut app, "remember how we did this: restart the printer queue", "Done: cleared and restarted.");
            app.reviewing = true; // one is running
            app.review(true);
            app.review(true);
            assert_eq!(app.review_queue.len(), 1, "one per turn");
            turn(&mut app, "and the scanner the same way", "Done.");
            app.review(true);
            assert_eq!(app.review_queue.len(), 2);
            assert!(app.activity.iter().any(|a| a.text.contains("a review is waiting")));
            // The running one finishes: the next starts (and leaves the queue).
            app.next_review();
            assert!(app.reviewing);
            assert_eq!(app.review_queue.len(), 1);
        });
    }

    #[test]
    fn signals_count_and_a_members_turn_is_counts_only() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        config::with_test_home(|home| {
            let (mut app, ev) = app(home, &rt);
            // The tool-call limit: part done, and the run says it hit the limit.
            turn(&mut app, "fix every failing test in the repo", "Fixed two so far.");
            app.handle(StreamEvent::Limit(8));
            let run = app.messages.iter().rev().find_map(|m| m.run.clone()).unwrap();
            assert_eq!(run.verdict.as_ref().map(|v| (v.outcome, v.strength)), Some((SkillOutcome::Partial, Strength::Signal)));
            let r = run_when(&ev, run.evo.unwrap(), |r| r.limited && r.outcome == lyra_evolution::RunOutcome::Partial);
            assert!(r.errors.iter().any(|e| e.contains("tool-call limit")));

            // Asked for again (/retry): that reply didn't do it.
            turn(&mut app, "summarize the report", "A summary.");
            let id = app.last_run.as_ref().unwrap().id;
            let evo = app.last_run.as_ref().unwrap().evo.unwrap();
            app.redo(None, "");
            run_when(&ev, evo, |r| r.outcome == lyra_evolution::RunOutcome::Failure && r.feedback.as_deref() == Some("they asked for the reply again"));
            assert!(app.run_ref(id).is_none() || app.run_ref(id).unwrap().verdict.is_some());

            // Someone else's turn: recorded, but only as counts.
            let (mut member, ev) = self::app(home, &rt);
            member.owner = "u-sam".into();
            turn(&mut member, "draft a note to my landlord about the leak", "Here's a draft.");
            let evo = member.last_run.as_ref().unwrap().evo.expect("members' turns count too");
            let r = run_when(&ev, evo, |_| true);
            assert_eq!((r.task.as_str(), r.person.as_deref()), ("", Some("u-sam")), "no words of theirs");
            member.rate(None, "bad", "wrong landlord's name").unwrap();
            let r = run_when(&ev, evo, |r| r.outcome == lyra_evolution::RunOutcome::Failure);
            assert_eq!(r.feedback, None, "their note stays theirs");
        });
    }
}
