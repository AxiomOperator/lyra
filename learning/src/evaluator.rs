//! Deciding whether an interaction taught something reusable.
//!
//! Two stages: a cheap [`trigger`] check on every turn, and, only when it fires,
//! an LLM review ([`SYSTEM_PROMPT`] + [`prompt`], answer read by [`parse`]). The
//! caller makes the LLM request, so this module has no network code.

use serde::Deserialize;

/// What happened in the turn just finished.
pub struct Turn<'a> {
    /// The user's message that started the turn.
    pub user: &'a str,
    /// Whether the assistant had replied before this message (so it can be a correction).
    pub follows_reply: bool,
    pub tool_calls: usize,
    /// Tool results that reported an error.
    pub tool_errors: usize,
}

/// Why this turn is worth reviewing for a lesson, or `None` for ordinary turns.
pub fn trigger(turn: &Turn) -> Option<&'static str> {
    let text = format!(" {} ", turn.user.to_lowercase());
    const EXPLICIT: &[&str] = &[
        "remember how", "how we did this", "next time", "from now on", "in the future",
        "going forward", "learn this", "make this a skill",
    ];
    const CORRECTION: &[&str] = &[
        " no,", " no.", " nope", "that's wrong", "that is wrong", "that's not", "not what i",
        "actually,", "instead", "you should have", "should be", "wrong", "doesn't work",
        "didn't work", "don't do", "stop doing",
    ];
    if EXPLICIT.iter().any(|p| text.contains(p)) {
        return Some("user asked to remember a procedure");
    }
    if turn.follows_reply && CORRECTION.iter().any(|p| text.contains(p)) {
        return Some("user corrected the assistant");
    }
    if turn.tool_errors > 0 && turn.tool_calls > turn.tool_errors {
        return Some("a failed step was replaced by a working one");
    }
    if turn.tool_calls >= 3 && turn.tool_errors == 0 {
        return Some("a multi-step procedure succeeded");
    }
    None
}

pub const SYSTEM_PROMPT: &str = "\
You review a conversation between a user and an AI assistant and decide whether it \
taught the assistant a reusable skill: a procedure or rule it should follow in \
future, similar tasks.

Learn only from:
- an explicit correction of the assistant by the user
- a failed approach that was replaced by one that worked
- a multi-step procedure that worked
- a recurring mistake that was fixed
- the user asking to remember how something was done

Never learn from: ordinary conversation, one-off requests, facts about the user or \
the world (those belong in memory, not skills), temporary errors, unverified claims, \
procedures that failed, or anything containing secrets or credentials.

Respond with only a JSON object, no other text:
{\"should_learn\": true, \"name\": \"short-kebab-case-name\", \
\"description\": \"one line: when this skill applies\", \
\"instructions\": \"the procedure or rule, in the imperative, self-contained\", \
\"confidence\": 0.0 to 1.0, \"reason\": \"why this is worth learning\"}
or, when nothing qualifies:
{\"should_learn\": false, \"reason\": \"why not\"}";

/// The user message for the review request.
pub fn prompt(trigger: &str, transcript: &str, existing: &[String]) -> String {
    let existing = if existing.is_empty() { "(none)".to_string() } else { existing.join(", ") };
    format!(
        "Why this was flagged: {trigger}\n\
         Skills that already exist (don't duplicate them): {existing}\n\n\
         Conversation:\n{transcript}"
    )
}

/// The reviewer's answer.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Verdict {
    pub should_learn: bool,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub confidence: f32,
    pub reason: String,
}

/// A lesson that passed validation and can be saved as a skill.
#[derive(Debug)]
pub struct Candidate {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub confidence: f32,
}

/// Read the JSON object out of the reviewer's reply, tolerating thinking tags
/// and text around it.
pub fn parse(reply: &str) -> Result<Verdict, String> {
    let reply = reply.rsplit_once("</think>").map_or(reply, |(_, after)| after);
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else {
        return Err(format!("no JSON in reviewer reply: {}", reply.trim()));
    };
    if end < start {
        return Err("malformed JSON in reviewer reply".into());
    }
    serde_json::from_str(&reply[start..=end]).map_err(|e| format!("bad reviewer JSON: {e}"))
}

impl Verdict {
    /// Turn a positive verdict into a candidate, or say why it doesn't qualify.
    pub fn into_candidate(self, min_confidence: f32) -> Result<Candidate, String> {
        if !self.should_learn {
            return Err(format!("nothing to learn: {}", self.reason));
        }
        let name = kebab(&self.name);
        let instructions = self.instructions.trim().to_string();
        let description = self.description.trim().to_string();
        if name.is_empty() || instructions.is_empty() || description.is_empty() {
            return Err("reviewer left out the name, description or instructions".into());
        }
        if self.confidence < min_confidence {
            return Err(format!(
                "confidence {:.2} below {min_confidence:.2}: {name}",
                self.confidence
            ));
        }
        if looks_secret(&instructions) || looks_secret(&description) {
            return Err(format!("{name} looks like it contains a credential"));
        }
        Ok(Candidate { name, description, instructions, confidence: self.confidence.min(1.0) })
    }
}

/// `"Provider Fallback_Order!"` -> `"provider-fallback-order"`, at most 60 chars.
fn kebab(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.chars().take(60).collect();
    out.trim_end_matches('-').to_string()
}

/// Conservative check for credentials: telltale words, or long random-looking tokens.
fn looks_secret(text: &str) -> bool {
    let lower = text.to_lowercase();
    const WORDS: &[&str] = &[
        "password", "passwd", "api_key", "api key", "apikey", "secret key", "private key",
        "bearer ", "access key",
    ];
    WORDS.iter().any(|w| lower.contains(w))
        || text.split_whitespace().any(|w| {
            w.len() >= 32
                && w.chars().any(|c| c.is_ascii_digit())
                && w.chars().any(|c| c.is_ascii_alphabetic())
                && !w.contains('/')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(user: &str) -> Turn<'_> {
        Turn { user, follows_reply: true, tool_calls: 0, tool_errors: 0 }
    }

    #[test]
    fn ordinary_turns_do_not_trigger() {
        assert_eq!(trigger(&turn("What's the capital of France?")), None);
        assert_eq!(trigger(&turn("Thanks, that's great")), None);
        let first = Turn { follows_reply: false, ..turn("No, I want tea") };
        assert_eq!(trigger(&first), None, "nothing to correct yet");
    }

    #[test]
    fn corrections_and_explicit_requests_trigger() {
        assert!(trigger(&turn("No, you should run the tests before committing.")).is_some());
        assert!(trigger(&turn("Actually, use cargo nextest instead")).is_some());
        assert!(trigger(&turn("Remember how we did this for next time")).is_some());
    }

    #[test]
    fn tool_outcomes_trigger() {
        let recovered = Turn { tool_calls: 2, tool_errors: 1, ..turn("do it") };
        assert!(trigger(&recovered).is_some());
        let all_failed = Turn { tool_calls: 1, tool_errors: 1, ..turn("do it") };
        assert_eq!(trigger(&all_failed), None);
        let multi = Turn { tool_calls: 3, ..turn("do it") };
        assert!(trigger(&multi).is_some());
    }

    #[test]
    fn parse_tolerates_wrapping_text() {
        let reply = "<think>hmm {not this}</think>Sure! ```json\n{\"should_learn\": true, \"name\": \"Run Tests First\", \"description\": \"before commits\", \"instructions\": \"Run cargo test before committing.\", \"confidence\": 0.9, \"reason\": \"user correction\"}\n```";
        let c = parse(reply).unwrap().into_candidate(0.6).unwrap();
        assert_eq!(c.name, "run-tests-first");
        assert_eq!(c.instructions, "Run cargo test before committing.");
    }

    #[test]
    fn negative_low_confidence_and_secret_verdicts_are_rejected() {
        let no = parse(r#"{"should_learn": false, "reason": "small talk"}"#).unwrap();
        assert!(no.into_candidate(0.6).unwrap_err().contains("small talk"));

        let low = parse(r#"{"should_learn": true, "name": "x", "description": "d", "instructions": "i", "confidence": 0.3}"#).unwrap();
        assert!(low.into_candidate(0.6).unwrap_err().contains("confidence"));

        let secret = parse(r#"{"should_learn": true, "name": "login", "description": "d", "instructions": "Use password hunter2 for the db", "confidence": 0.9}"#).unwrap();
        assert!(secret.into_candidate(0.6).unwrap_err().contains("credential"));

        let token = parse(r#"{"should_learn": true, "name": "x", "description": "d", "instructions": "Set TOKEN=a8f3k29dk3l2k4j5h6g7f8d9s0a1q2w3e4r5", "confidence": 0.9}"#).unwrap();
        assert!(token.into_candidate(0.6).is_err());

        assert!(parse("I don't think so.").is_err());
    }

    #[test]
    fn kebab_names() {
        assert_eq!(kebab("Provider Fallback_Order!"), "provider-fallback-order");
        assert_eq!(kebab("  --x--  "), "x");
        assert_eq!(kebab(&"é".repeat(80)).chars().count(), 60);
    }
}
