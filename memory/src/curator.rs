//! Keeping the memory collection useful (M9, M15, M18): duplicate and stale
//! detection, the model's consolidation and contradiction review, and
//! collection statistics.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::rank::cosine;
use crate::{Memory, MemoryKind, MemoryStatus};

/// Content words of a memory, for comparing wording.
pub fn words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(str::to_lowercase)
        .collect()
}

/// Share of words two texts have in common (Jaccard).
pub fn overlap(a: &str, b: &str) -> f32 {
    let (wa, wb) = (words(a), words(b));
    let union = wa.union(&wb).count();
    if union == 0 { 0.0 } else { wa.intersection(&wb).count() as f32 / union as f32 }
}

/// Pairs of active memories in the same scope that say nearly the same thing,
/// by wording or by meaning (vectors), most similar first.
pub fn duplicates(
    memories: &[Memory],
    vectors: &HashMap<Uuid, Vec<f32>>,
    word_threshold: f32,
    vector_threshold: f32,
) -> Vec<(Uuid, Uuid, f32)> {
    let live: Vec<&Memory> = memories
        .iter()
        .filter(|m| m.status == MemoryStatus::Active && m.kind != MemoryKind::Episodic)
        .collect();
    let mut out = Vec::new();
    for (i, a) in live.iter().enumerate() {
        for b in &live[i + 1..] {
            if a.scope != b.scope {
                continue;
            }
            let by_words = overlap(&a.content, &b.content);
            let by_meaning = match (vectors.get(&a.id), vectors.get(&b.id)) {
                (Some(va), Some(vb)) => cosine(va, vb),
                _ => 0.0,
            };
            if by_words >= word_threshold || by_meaning >= vector_threshold {
                out.push((a.id, b.id, by_words.max(by_meaning)));
            }
        }
    }
    out.sort_by(|x, y| y.2.total_cmp(&x.2));
    out
}

/// Active memories that haven't been used in `days`, are older than that, and
/// aren't marked important: candidates for archiving.
pub fn stale(memories: &[Memory], days: i64, now: DateTime<Utc>) -> Vec<Uuid> {
    let cutoff = now - Duration::days(days);
    memories
        .iter()
        .filter(|m| m.status == MemoryStatus::Active && m.importance < 0.7 && m.created_at < cutoff)
        .filter(|m| m.last_accessed_at.is_none_or(|t| t < cutoff))
        .map(|m| m.id)
        .collect()
}

/// The memory collection at a glance (M15).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub total: usize,
    pub active: usize,
    pub by_kind: Vec<(MemoryKind, usize)>,
    pub by_status: Vec<(MemoryStatus, usize)>,
    pub by_scope: Vec<(String, u64)>,
    /// Active memories never put in front of the model.
    pub unused: usize,
    /// Active memories past their expiry (archived at the next curation).
    pub expired: usize,
    pub contradictions: usize,
    pub duplicate_candidates: usize,
    pub average_confidence: Option<f32>,
    /// Active memories with a vector from the current embedding model.
    pub embedded: usize,
    pub episodes: usize,
    pub pending_proposals: usize,
}

pub const SYSTEM_PROMPT: &str = "\
You maintain an AI assistant's long-term memory. Review the memories below (each \
has an id) and find:
- consolidations: several memories that state the same thing or repeat one \
observation; give one clearer memory that keeps every useful detail
- contradictions: memories that can't both be true (flag them; don't resolve them)

Only suggest changes that clearly help; an empty answer is fine. Never merge \
memories from different scopes.

Respond with only a JSON object, no other text:
{\"consolidations\": [{\"memories\": [\"id\", \"id\"], \"content\": \"the consolidated memory\", \
\"reason\": \"why\"}],
 \"contradictions\": [{\"memories\": [\"id\", \"id\"], \"reason\": \"what conflicts\"}]}";

/// The facts for review, with the duplicate pairs already spotted. Episodes
/// and working notes record events and short-term state, so they're left out.
pub fn prompt(memories: &[Memory], duplicate_pairs: &[(String, String, f32)]) -> String {
    let mut out = String::from("Memories:\n");
    let facts = memories.iter().filter(|m| m.status == MemoryStatus::Active && m.kind == MemoryKind::Semantic);
    for m in facts.take(150) {
        let content: String = m.content.chars().take(400).collect();
        out += &format!("- [{}] ({}, {}, confidence {:.1}) {content}\n", m.short_id(), m.scope, m.kind, m.confidence);
    }
    if !duplicate_pairs.is_empty() {
        out += "\nPairs that look like duplicates:\n";
        for (a, b, sim) in duplicate_pairs {
            out += &format!("- {a} / {b} ({:.0}% similar)\n", sim * 100.0);
        }
    }
    out
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Plan {
    pub consolidations: Vec<Consolidation>,
    pub contradictions: Vec<Contradiction>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Consolidation {
    pub memories: Vec<String>,
    pub content: String,
    pub reason: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Contradiction {
    pub memories: Vec<String>,
    pub reason: String,
}

pub fn parse(reply: &str) -> Result<Plan, String> {
    let reply = reply.rsplit_once("</think>").map_or(reply, |(_, after)| after);
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else {
        return Err(format!("no JSON in curator reply: {}", reply.trim()));
    };
    if end < start {
        return Err("malformed JSON in curator reply".into());
    }
    serde_json::from_str(&reply[start..=end]).map_err(|e| format!("bad curator JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::tests::memory;

    #[test]
    fn duplicates_by_words_or_vectors_within_a_scope() {
        let a = memory("project:x", "The agent runtime is written in Rust");
        let b = memory("project:x", "Agent runtime written in Rust");
        let c = memory("project:x", "Deploys happen on Fridays");
        let d = memory("project:y", "The agent runtime is written in Rust");
        let mut vectors = HashMap::new();
        vectors.insert(c.id, vec![1.0, 0.0]);
        let mut e = memory("project:x", "Releases go out at the end of each week");
        vectors.insert(e.id, vec![0.99, 0.05]);
        e.status = MemoryStatus::Active;

        let found = duplicates(&[a.clone(), b.clone(), c.clone(), d, e.clone()], &vectors, 0.6, 0.95);
        let pairs: Vec<(Uuid, Uuid)> = found.iter().map(|(x, y, _)| (*x, *y)).collect();
        assert!(pairs.contains(&(a.id, b.id)), "same words");
        assert!(pairs.contains(&(c.id, e.id)), "same meaning");
        assert_eq!(pairs.len(), 2, "never across scopes");
    }

    #[test]
    fn stale_memories() {
        let now = Utc::now();
        let mut old = memory("user", "old");
        old.created_at = now - Duration::days(400);
        old.importance = 0.3;
        let mut old_but_important = old.clone();
        old_but_important.importance = 0.9;
        let mut old_but_used = old.clone();
        old_but_used.last_accessed_at = Some(now);
        let found = stale(&[old.clone(), old_but_important, old_but_used, memory("user", "new")], 180, now);
        assert_eq!(found, [old.id]);
    }

    #[test]
    fn parses_plans() {
        let plan = parse(r#"{"consolidations":[{"memories":["a","b"],"content":"c","reason":"r"}]}"#).unwrap();
        assert_eq!(plan.consolidations[0].memories, ["a", "b"]);
        assert!(plan.contradictions.is_empty());
    }
}
