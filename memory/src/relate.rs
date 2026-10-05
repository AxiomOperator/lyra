//! Relating a new memory to what's already known (M3, M4): when a memory is
//! saved, similar ones are shown to the model, which says how they relate.
//! An update supersedes the old memory, a contradiction is flagged, and
//! support and relatedness become links that ranking uses. No network code.

use serde::Deserialize;

use crate::{Memory, Relationship};

pub const SYSTEM_PROMPT: &str = "\
You maintain an AI assistant's long-term memory. A new memory was just saved. For each \
existing memory listed, say how the new one relates to it:
- \"updates\": the new memory replaces it (the old one is no longer true, e.g. a changed \
preference, a new version, a moved server)
- \"contradicts\": they can't both be true, and it isn't clear which is current
- \"supports\": the new one confirms or backs it up
- \"related\": same subject, different facts
- \"unrelated\": nothing to do with each other

Respond with only a JSON object: {\"links\": [{\"id\": \"8-char id\", \"relation\": \"updates\", \"reason\": \"few words\"}]}";

pub fn prompt(new: &Memory, existing: &[Memory]) -> String {
    let mut out = format!("New memory [{}] ({}): {}\n\nExisting memories:\n", new.short_id(), new.created_at.format("%Y-%m-%d"), new.content);
    for m in existing {
        out += &format!("- [{}] ({}, {}): {}\n", m.short_id(), m.created_at.format("%Y-%m-%d"), m.source, m.content);
    }
    out
}

/// What to do about one existing memory.
#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    /// The new memory supersedes this one.
    Replaces,
    Relate(Relationship),
}

#[derive(Deserialize)]
struct Draft {
    #[serde(default)]
    links: Vec<DraftLink>,
}

#[derive(Deserialize)]
struct DraftLink {
    id: String,
    relation: String,
    #[serde(default)]
    reason: String,
}

/// The links to make, as `(existing memory, link, reason)`; ids must be ones
/// that were shown.
pub fn parse(reply: &str, existing: &[Memory]) -> Result<Vec<(Memory, Link, String)>, String> {
    let reply = reply.rsplit_once("</think>").map_or(reply, |(_, after)| after);
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else {
        return Err("no JSON in the reply".into());
    };
    let draft: Draft = serde_json::from_str(&reply[start..=end.max(start)]).map_err(|e| format!("bad JSON: {e}"))?;
    let mut out = Vec::new();
    for l in draft.links {
        let id = l.id.trim().trim_start_matches('[').trim_end_matches(']');
        let Some(m) = existing.iter().find(|m| !id.is_empty() && m.id.to_string().starts_with(id)) else { continue };
        let link = match l.relation.trim() {
            "updates" => Link::Replaces,
            "contradicts" => Link::Relate(Relationship::Contradicts),
            "supports" => Link::Relate(Relationship::Supports),
            "related" => Link::Relate(Relationship::RelatedTo),
            _ => continue,
        };
        let reason = if l.reason.trim().is_empty() { l.relation.trim().to_string() } else { l.reason.trim().to_string() };
        out.push((m.clone(), link, reason));
    }
    Ok(out)
}
