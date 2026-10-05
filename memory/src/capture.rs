//! Automatic memory capture (M2, M3, M8): after a turn that looks like it
//! contained something durable, the model reviews it and proposes memories
//! (new, updates or superseding ones) and, for significant runs, an episode.
//! The runtime validates and applies them; this module has no network code.

use serde::Deserialize;

use crate::Memory;

/// What happened in the turn just finished.
pub struct Turn<'a> {
    pub user: &'a str,
    /// Tool calls other than the memory tools themselves.
    pub task_tool_calls: usize,
    /// The model already saved, corrected or superseded memories this turn.
    pub model_saved: bool,
}

/// Phrases that usually introduce something worth keeping.
const DURABLE: &[&str] = &[
    "i use", "we use", "i'm using", "we're using", "i am using", "we are using", "my ", "our ",
    "i prefer", "i like", "i don't like", "i hate", "i work", "i'm a", "i am a", "we decided",
    "decided to", "we chose", "we switched", "switched to", "moved to", "migrated to",
    "no longer", "from now on", "always ", "never ", "remember", "is located", "lives at",
    "runs on", "is running", "deadline", "due on", "my name", "call me", "port ", "version ",
    "is hosted", "we have", "i have",
];

/// Why this turn is worth reviewing for memories, or `None`.
pub fn trigger(turn: &Turn) -> Option<&'static str> {
    if turn.task_tool_calls >= 2 {
        return Some("a multi-step task ran (possible episode)");
    }
    if turn.model_saved {
        return None;
    }
    let text = format!(" {} ", turn.user.to_lowercase());
    DURABLE.iter().any(|p| text.contains(p)).then_some("the user shared something that may be durable")
}

pub const SYSTEM_PROMPT: &str = "\
You maintain an AI assistant's long-term memory. Read the conversation and decide \
what, if anything, is worth remembering for future conversations.

Remember: project decisions, stable configuration, lasting user preferences and \
instructions, important people/systems/entities, durable technical facts, outcomes \
of significant tasks.

Never remember: small talk, temporary state, credentials, passwords, tokens or keys, \
transient errors, guesses or unsupported assumptions, procedures (those are skills, \
not memories), or anything already stored unchanged.

Compare with the existing memories shown (each has an id):
- if it's new, create it
- if it refines or corrects the same fact (typo, more detail), update that memory
- if the fact has changed over time (we used X, now we use Y), supersede the old one
- if it's already stored, ignore it

Each memory is one self-contained statement that makes sense on its own later. \
Confidence: 1.0 if the user said it outright, 0.9 if it's clear from the conversation, \
0.5 if you're inferring it. Importance: 0.1 minor detail, 0.5 useful, 0.9 critical. \
Scopes: \"user\" (about the user), \"agent\" (how the assistant should work), \
\"project:<name>\" (a specific project).

If a multi-step task ran, you may also summarize it as an episode: what was done, \
what went wrong, how it ended.

Respond with only a JSON object, no other text:
{\"memories\": [{\"action\": \"create|update|supersede|ignore\", \"target\": \"id of the \
existing memory (update/supersede only)\", \"content\": \"...\", \"kind\": \"semantic\", \
\"scope\": \"user\", \"importance\": 0.5, \"confidence\": 0.9, \"tags\": [\"...\"], \
\"expires_in_days\": null, \"reason\": \"why\"}],
 \"episode\": null or {\"summary\": \"...\", \"outcome\": \"...\", \"entities\": [\"...\"]}}";

/// The user message for the capture request.
pub fn prompt(reason: &str, transcript: &str, similar: &[Memory], default_scope: &str) -> String {
    let mut existing = String::new();
    for m in similar {
        existing += &format!("- [{}] ({}, {}) {}\n", m.short_id(), m.scope, m.kind, m.content);
    }
    if existing.is_empty() {
        existing = "(none)\n".into();
    }
    format!(
        "Why this was flagged: {reason}\nDefault scope: {default_scope}\n\n\
         Existing memories that may be related:\n{existing}\nConversation:\n{transcript}"
    )
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Plan {
    pub memories: Vec<Item>,
    pub episode: Option<EpisodeItem>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Item {
    pub action: String,
    pub target: String,
    pub content: String,
    pub kind: String,
    pub scope: String,
    pub importance: Option<f32>,
    pub confidence: Option<f32>,
    pub tags: Vec<String>,
    pub expires_in_days: Option<f32>,
    pub reason: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct EpisodeItem {
    pub summary: String,
    pub outcome: String,
    pub entities: Vec<String>,
}

/// Read the plan out of the model's reply, tolerating thinking tags and text around it.
pub fn parse(reply: &str) -> Result<Plan, String> {
    let reply = reply.rsplit_once("</think>").map_or(reply, |(_, after)| after);
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else {
        return Err(format!("no JSON in capture reply: {}", reply.trim()));
    };
    if end < start {
        return Err("malformed JSON in capture reply".into());
    }
    serde_json::from_str(&reply[start..=end]).map_err(|e| format!("bad capture JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(user: &str) -> Turn<'_> {
        Turn { user, task_tool_calls: 0, model_saved: false }
    }

    #[test]
    fn triggers_on_durable_statements_only() {
        assert!(trigger(&turn("We switched the API to port 8080 last week")).is_some());
        assert!(trigger(&turn("I prefer tabs over spaces")).is_some());
        assert_eq!(trigger(&turn("What's 2 + 2?")), None);
        assert_eq!(trigger(&turn("thanks!")), None);
        let saved = Turn { model_saved: true, ..turn("I prefer tabs") };
        assert_eq!(trigger(&saved), None, "the model already saved it");
        let task = Turn { task_tool_calls: 3, ..turn("deploy it") };
        assert!(trigger(&task).unwrap().contains("episode"));
    }

    #[test]
    fn parses_plans() {
        let plan = parse(
            r#"<think>{x}</think> {"memories":[{"action":"supersede","target":"abcd1234","content":"The API uses port 8080.","scope":"project:api","confidence":1.0}],
               "episode":{"summary":"Moved the API","outcome":"done","entities":["api"]}}"#,
        )
        .unwrap();
        assert_eq!(plan.memories[0].action, "supersede");
        assert_eq!(plan.memories[0].importance, None);
        assert_eq!(plan.episode.unwrap().entities, ["api"]);
        assert!(parse(r#"{"memories": []}"#).unwrap().episode.is_none());
        assert!(parse("nothing").is_err());
    }
}
