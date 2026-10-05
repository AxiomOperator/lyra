//! Keeping the collection healthy (V7): find near-duplicates, conflicts,
//! skills that should be merged or split, and stale or failing ones.
//!
//! Duplicate detection and health metrics are computed here; contradictions,
//! merges and splits need judgment, so [`SYSTEM_PROMPT`] + [`prompt`] ask the
//! model and [`parse`] reads its plan. The manager turns the plan into
//! proposals (merges and splits always need approval) and relationships.

use std::collections::HashSet;

use serde::Deserialize;
use uuid::Uuid;

use crate::files::words;
use crate::proposal::NewSkill;
use crate::{Skill, SkillStatus};

/// Pairs of in-use skills whose wording overlaps at least `threshold`
/// (Jaccard similarity of their content words), most similar first.
pub fn duplicates(skills: &[Skill], threshold: f32) -> Vec<(Uuid, Uuid, f32)> {
    let live: Vec<(&Skill, HashSet<String>)> = skills
        .iter()
        .filter(|s| matches!(s.status, SkillStatus::Active | SkillStatus::Proposed))
        .map(|s| {
            let text = format!("{} {} {}", s.name.replace('-', " "), s.description, s.instructions);
            (s, words(&text).into_iter().collect())
        })
        .collect();
    let mut out = Vec::new();
    for (i, (a, wa)) in live.iter().enumerate() {
        for (b, wb) in &live[i + 1..] {
            let union = wa.union(wb).count();
            if union == 0 {
                continue;
            }
            let similarity = wa.intersection(wb).count() as f32 / union as f32;
            if similarity >= threshold {
                out.push((a.id, b.id, similarity));
            }
        }
    }
    out.sort_by(|x, y| y.2.total_cmp(&x.2));
    out
}

/// Collection health at a glance.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Health {
    pub total: usize,
    pub active: usize,
    pub proposed: usize,
    pub deprecated: usize,
    pub rejected: usize,
    pub pending_proposals: usize,
    pub duplicate_candidates: usize,
    pub conflicts: usize,
    /// Mean reliability of active skills with known outcomes.
    pub average_reliability: Option<f32>,
    /// Active skills never used.
    pub never_used: usize,
    /// Active skills with more failures than successes (and some evidence).
    pub failing: usize,
    pub stale: usize,
}

pub const SYSTEM_PROMPT: &str = "\
You maintain a library of skills (reusable procedures) for an AI assistant. Review \
the library and find:
- merges: skills that cover substantially the same procedure; give the merged skill
- splits: a skill that has grown to cover several distinct procedures; give the parts
- conflicts: skills whose instructions contradict each other (flag them, don't resolve)

Only suggest changes that clearly improve the library; an empty plan is fine. \
Keep every useful instruction when merging or splitting. Names are short kebab-case.

Respond with only a JSON object, no other text:
{\"merges\": [{\"skills\": [\"name-a\", \"name-b\"], \"name\": \"merged-name\", \
\"description\": \"when it applies\", \"instructions\": \"complete merged instructions\", \
\"reason\": \"why\"}],
 \"splits\": [{\"skill\": \"name\", \"parts\": [{\"name\": \"part-name\", \
\"description\": \"when it applies\", \"instructions\": \"...\"}], \"reason\": \"why\"}],
 \"conflicts\": [{\"skills\": [\"name-a\", \"name-b\"], \"reason\": \"what contradicts\"}]}";

/// The library for review, with the duplicate pairs already spotted.
pub fn prompt(skills: &[Skill], duplicate_names: &[(String, String, f32)]) -> String {
    let mut out = String::from("Skills:\n");
    for s in skills.iter().filter(|s| matches!(s.status, SkillStatus::Active | SkillStatus::Proposed)) {
        let instructions: String = s.instructions.chars().take(800).collect();
        out += &format!(
            "\n### {} ({}, reliability {:.2})\nWhen: {}\n{}\n",
            s.name,
            s.status,
            s.usage.reliability(),
            s.description,
            instructions
        );
    }
    if !duplicate_names.is_empty() {
        out += "\nPairs with very similar wording (possible duplicates):\n";
        for (a, b, sim) in duplicate_names {
            out += &format!("- {a} / {b} ({:.0}% overlap)\n", sim * 100.0);
        }
    }
    out
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Plan {
    pub merges: Vec<MergePlan>,
    pub splits: Vec<SplitPlan>,
    pub conflicts: Vec<ConflictPlan>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct MergePlan {
    pub skills: Vec<String>,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub reason: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SplitPlan {
    pub skill: String,
    pub parts: Vec<NewSkill>,
    pub reason: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ConflictPlan {
    pub skills: Vec<String>,
    pub reason: String,
}

/// Read the curator's plan out of the model's reply.
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
    use crate::files::tests::skill;

    #[test]
    fn finds_near_duplicates_only_among_live_skills() {
        let a = skill("rust-commit-checks", "Run cargo fmt, cargo clippy and cargo test before committing", SkillStatus::Active);
        let b = skill("rust-precommit", "Before committing run cargo fmt, cargo clippy, cargo test", SkillStatus::Proposed);
        let c = skill("green-tea", "Brew green tea at 80 degrees for two minutes", SkillStatus::Active);
        let d = skill("old-commit", "Run cargo fmt, cargo clippy and cargo test before committing", SkillStatus::Deprecated);
        let found = duplicates(&[a.clone(), b.clone(), c, d], 0.5);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].0, found[0].1), (a.id, b.id));
    }

    #[test]
    fn parses_plans() {
        let plan = parse(
            r#"ok {"merges":[{"skills":["a","b"],"name":"ab","description":"d","instructions":"i","reason":"same"}],
                   "conflicts":[{"skills":["x","y"],"reason":"retry counts differ"}]}"#,
        )
        .unwrap();
        assert_eq!(plan.merges[0].skills, ["a", "b"]);
        assert!(plan.splits.is_empty());
        assert_eq!(plan.conflicts[0].reason, "retry counts differ");
        assert!(parse(r#"{}"#).unwrap().merges.is_empty());
        assert!(parse("nothing to do").is_err());
    }
}
