//! Deciding whether an interaction taught something reusable, and whether that
//! is a new skill or a refinement of an existing one.
//!
//! Two stages: a cheap [`trigger`] check on every turn, and, only when it fires,
//! an LLM review ([`SYSTEM_PROMPT`] + [`prompt`], answer read by [`parse`] and
//! [`Verdict::decide`]). The caller makes the LLM request, so this module has
//! no network code. [`outcome_signal`] reads the user's next message as
//! feedback on the skills the last answer used.

use serde::Deserialize;

use crate::{Skill, SkillOutcome};

/// What happened in the turn just finished.
pub struct Turn<'a> {
    /// The user's message that started the turn.
    pub user: &'a str,
    /// Whether the assistant had replied before this message (so it can be a correction).
    pub follows_reply: bool,
    pub tool_calls: usize,
    /// Tool results that reported an error.
    pub tool_errors: usize,
    /// Skills that were in the prompt for the reply being corrected, if any.
    pub skills_used: usize,
}

const EXPLICIT: &[&str] = &[
    "remember how", "how we did this", "next time", "from now on", "in the future",
    "going forward", "learn this", "make this a skill",
];
const CORRECTION: &[&str] = &[
    " no,", " no.", " nope", "that's wrong", "that is wrong", "that's not", "not what i",
    "actually,", "instead", "you should have", "should be", "wrong", "doesn't work",
    "didn't work", "don't do", "stop doing", "still broken", "still fails",
];
const PRAISE: &[&str] = &[
    "thanks", "thank you", "perfect", "that worked", "it worked", "works now", "that works",
    "great", "awesome", "exactly", "nice", "spot on", "worked",
];

/// Why this turn is worth reviewing for a lesson, or `None` for ordinary turns.
pub fn trigger(turn: &Turn) -> Option<&'static str> {
    let text = format!(" {} ", turn.user.to_lowercase());
    if EXPLICIT.iter().any(|p| text.contains(p)) {
        return Some("user asked to remember a procedure");
    }
    if turn.follows_reply && CORRECTION.iter().any(|p| text.contains(p)) {
        return Some(if turn.skills_used > 0 {
            "user corrected an answer that followed a learned skill, so the skill may be incomplete"
        } else {
            "user corrected the assistant"
        });
    }
    if turn.tool_errors > 0 && turn.tool_calls > turn.tool_errors {
        return Some("a failed step was replaced by a working one");
    }
    if turn.tool_calls >= 3 && turn.tool_errors == 0 {
        return Some("a multi-step procedure succeeded");
    }
    None
}

/// What the user's next message says about the previous answer: a correction
/// is a failure, thanks or "that worked" a success, anything else no signal.
/// (A failed task isn't a failed skill, so only the user's reaction counts.)
pub fn outcome_signal(next_message: &str) -> Option<SkillOutcome> {
    let text = format!(" {} ", next_message.to_lowercase());
    if CORRECTION.iter().any(|p| text.contains(p)) {
        Some(SkillOutcome::Failure)
    } else if PRAISE.iter().any(|p| text.contains(p)) {
        Some(SkillOutcome::Success)
    } else {
        None
    }
}

pub const SYSTEM_PROMPT: &str = "\
You review a conversation between a user and an AI assistant and decide whether it \
taught the assistant a reusable skill: a procedure or rule it should follow in \
future, similar tasks. If an existing skill already covers it, refine that skill \
instead of creating a duplicate.

Learn only from:
- an explicit correction of the assistant by the user
- a failed approach that was replaced by one that worked
- a multi-step procedure that worked
- a recurring mistake that was fixed
- the user asking to remember how something was done
- a skill that was used but turned out incomplete

Never learn from: ordinary conversation, one-off requests, facts about the user or \
the world (those belong in memory, not skills), temporary errors, unverified claims, \
procedures that failed, or anything containing secrets or credentials.

When updating an existing skill:
- keep its useful existing instructions, and change only what the lesson is about
- don't replace a broad skill with a narrower lesson; add the lesson to it
- don't repeat steps that are already there
- give the complete revised instructions, not just the change

Respond with only a JSON object, no other text. One of:
{\"action\": \"create\", \"name\": \"short-kebab-case-name\", \
\"description\": \"one line: when this skill applies\", \
\"instructions\": \"the procedure or rule, in the imperative, self-contained\", \
\"confidence\": 0.0 to 1.0, \"reason\": \"why this is worth learning\"}
{\"action\": \"update\", \"skill\": \"name-of-existing-skill\", \
\"description\": \"one line: when it applies\", \
\"instructions\": \"the complete revised instructions\", \
\"confidence\": 0.0 to 1.0, \"reason\": \"what changed and why\"}
{\"action\": \"ignore\", \"reason\": \"why nothing qualifies\"}";

/// The user message for the review request. `related` are the existing skills
/// most similar to this conversation (shown in full, so they can be refined);
/// `others` are the names of the rest.
pub fn prompt(trigger: &str, transcript: &str, related: &[Skill], others: &[String]) -> String {
    let mut related_text = String::new();
    for s in related {
        related_text += &format!(
            "\n### {} ({})\nWhen: {}\n{}\n",
            s.name, s.status, s.description, s.instructions
        );
    }
    if related_text.is_empty() {
        related_text = "(none)\n".into();
    }
    let others = if others.is_empty() { "(none)".to_string() } else { others.join(", ") };
    format!(
        "Why this was flagged: {trigger}\n\n\
         Existing skills that may already cover this:\n{related_text}\n\
         Other skill names (don't duplicate them): {others}\n\n\
         Conversation:\n{transcript}"
    )
}

/// The reviewer's answer.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Verdict {
    /// `create`, `update` or `ignore`.
    pub action: String,
    /// Older answer shape: `should_learn: true` means `create`.
    pub should_learn: bool,
    /// For `update`: the existing skill's name.
    pub skill: String,
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
    pub reason: String,
}

/// What to do with a review.
#[derive(Debug)]
pub enum Decision {
    Ignore(String),
    Create(Candidate),
    /// Refine the existing skill named `skill`; `instructions` is the full revised text.
    Update { skill: String, description: String, instructions: String, confidence: f32, reason: String },
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
    /// Validate the verdict against the skills that exist (`existing` names).
    /// Anything that doesn't qualify becomes `Ignore` with the reason.
    pub fn decide(self, min_confidence: f32, existing: &[String]) -> Decision {
        let action = match self.action.as_str() {
            "" if self.should_learn => "create",
            "" => "ignore",
            other => other,
        };
        if action == "ignore" {
            return Decision::Ignore(format!("nothing to learn: {}", self.reason));
        }
        let instructions = self.instructions.trim().to_string();
        let description = self.description.trim().to_string();
        let target = kebab(if action == "update" { &self.skill } else { &self.name });
        if target.is_empty() || instructions.is_empty() {
            return Decision::Ignore("reviewer left out the skill name or instructions".into());
        }
        if self.confidence < min_confidence {
            return Decision::Ignore(format!(
                "confidence {:.2} below {min_confidence:.2}: {target}",
                self.confidence
            ));
        }
        if looks_secret(&instructions) || looks_secret(&description) {
            return Decision::Ignore(format!("{target} looks like it contains a credential"));
        }
        let confidence = self.confidence.min(1.0);
        let reason = self.reason.trim().to_string();
        // Creating a skill that already exists is really an update of it.
        if action == "update" || existing.contains(&target) {
            if !existing.contains(&target) {
                return Decision::Ignore(format!("asked to update {target}, which doesn't exist"));
            }
            return Decision::Update { skill: target, description, instructions, confidence, reason };
        }
        if action != "create" {
            return Decision::Ignore(format!("unknown action {action:?}"));
        }
        if description.is_empty() {
            return Decision::Ignore("reviewer left out the description".into());
        }
        Decision::Create(Candidate { name: target, description, instructions, confidence, reason })
    }
}

/// `"Provider Fallback_Order!"` -> `"provider-fallback-order"`, at most 60 chars.
pub fn kebab(name: &str) -> String {
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
pub fn looks_secret(text: &str) -> bool {
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
        Turn { user, follows_reply: true, tool_calls: 0, tool_errors: 0, skills_used: 0 }
    }

    fn decide(json: &str, existing: &[&str]) -> Decision {
        let existing: Vec<String> = existing.iter().map(|s| s.to_string()).collect();
        parse(json).unwrap().decide(0.6, &existing)
    }

    fn ignored(d: Decision) -> String {
        match d {
            Decision::Ignore(why) => why,
            other => panic!("expected ignore, got {other:?}"),
        }
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
        let with_skill = Turn { skills_used: 1, ..turn("No, that's wrong") };
        assert!(trigger(&with_skill).unwrap().contains("skill may be incomplete"));
    }

    #[test]
    fn tool_outcomes_trigger() {
        let recovered = Turn { tool_calls: 2, tool_errors: 1, ..turn("do it") };
        assert!(trigger(&recovered).is_some());
        let all_failed = Turn { tool_calls: 1, tool_errors: 1, ..turn("do it") };
        assert_eq!(trigger(&all_failed), None, "failed procedures aren't lessons");
        let multi = Turn { tool_calls: 3, ..turn("do it") };
        assert!(trigger(&multi).is_some());
    }

    #[test]
    fn outcome_signals() {
        assert_eq!(outcome_signal("Thanks, that worked!"), Some(SkillOutcome::Success));
        assert_eq!(outcome_signal("No, that's wrong"), Some(SkillOutcome::Failure));
        assert_eq!(outcome_signal("Thanks but that didn't work"), Some(SkillOutcome::Failure));
        assert_eq!(outcome_signal("Now do the same for Go"), None);
    }

    #[test]
    fn parse_tolerates_wrapping_text() {
        let reply = "<think>hmm {not this}</think>Sure! ```json\n{\"action\": \"create\", \"name\": \"Run Tests First\", \"description\": \"before commits\", \"instructions\": \"Run cargo test before committing.\", \"confidence\": 0.9, \"reason\": \"user correction\"}\n```";
        let Decision::Create(c) = parse(reply).unwrap().decide(0.6, &[]) else {
            panic!("expected create")
        };
        assert_eq!(c.name, "run-tests-first");
        assert_eq!(c.instructions, "Run cargo test before committing.");
    }

    #[test]
    fn updates_target_existing_skills() {
        let d = decide(
            r#"{"action":"update","skill":"rust-commit","description":"d","instructions":"1. fmt 2. clippy 3. test 4. doc","confidence":0.8,"reason":"added doc step"}"#,
            &["rust-commit"],
        );
        assert!(matches!(d, Decision::Update { ref skill, .. } if skill == "rust-commit"), "{d:?}");

        let missing = r#"{"action":"update","skill":"nope","instructions":"x","confidence":0.8}"#;
        assert!(ignored(decide(missing, &["rust-commit"])).contains("doesn't exist"));

        // A create for a name that exists becomes an update.
        let dup = r#"{"action":"create","name":"rust-commit","description":"d","instructions":"x","confidence":0.8}"#;
        assert!(matches!(decide(dup, &["rust-commit"]), Decision::Update { .. }));
    }

    #[test]
    fn old_answer_shape_still_works() {
        let old = r#"{"should_learn": true, "name": "x", "description": "d", "instructions": "i", "confidence": 0.9}"#;
        assert!(matches!(decide(old, &[]), Decision::Create(_)));
    }

    #[test]
    fn ignore_low_confidence_and_secrets_are_rejected() {
        assert!(ignored(decide(r#"{"action": "ignore", "reason": "small talk"}"#, &[])).contains("small talk"));
        let low = r#"{"action":"create","name":"x","description":"d","instructions":"i","confidence":0.3}"#;
        assert!(ignored(decide(low, &[])).contains("confidence"));
        let password = r#"{"action":"create","name":"login","description":"d","instructions":"Use password hunter2 for the db","confidence":0.9}"#;
        assert!(ignored(decide(password, &[])).contains("credential"));
        let token = r#"{"action":"create","name":"x","description":"d","instructions":"Set TOKEN=a8f3k29dk3l2k4j5h6g7f8d9s0a1q2w3e4r5","confidence":0.9}"#;
        assert!(ignored(decide(token, &[])).contains("credential"));
        assert!(parse("I don't think so.").is_err());
    }

    #[test]
    fn kebab_names() {
        assert_eq!(kebab("Provider Fallback_Order!"), "provider-fallback-order");
        assert_eq!(kebab("  --x--  "), "x");
        assert_eq!(kebab(&"é".repeat(80)).chars().count(), 60);
    }
}
