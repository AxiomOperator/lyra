//! Ranking skills for a task by more than keyword match (V5): how reliable a
//! skill has proven, how sure we were when learning it, and how fresh it is.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::Skill;

/// How much each signal counts. Configurable under `[learning.scoring]`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct Weights {
    pub relevance: f32,
    pub reliability: f32,
    pub confidence: f32,
    pub freshness: f32,
    /// Days for freshness to halve without use.
    pub half_life_days: f32,
}

impl Default for Weights {
    fn default() -> Self {
        Self { relevance: 0.50, reliability: 0.25, confidence: 0.15, freshness: 0.10, half_life_days: 90.0 }
    }
}

/// Why a skill ranked where it did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkillScore {
    /// Keyword match to the task, 0–1 relative to the best match.
    pub relevance: f32,
    pub learned_confidence: f32,
    pub observed_reliability: f32,
    /// How much evidence there is, 0–1 (20+ known outcomes ≈ 1).
    pub usage_score: f32,
    pub freshness_score: f32,
    pub final_score: f32,
}

/// Score `skill` given its relevance (0–1) to the current task.
pub fn score(skill: &Skill, relevance: f32, w: &Weights, now: DateTime<Utc>) -> SkillScore {
    let reliability = skill.usage.reliability();
    let confidence = skill.confidence.clamp(0.0, 1.0);
    let freshness = freshness(skill, w.half_life_days, now);
    let completed = skill.usage.completed() as f32;
    SkillScore {
        relevance,
        learned_confidence: confidence,
        observed_reliability: reliability,
        usage_score: ((1.0 + completed).ln() / 21f32.ln()).min(1.0),
        freshness_score: freshness,
        final_score: relevance * w.relevance
            + reliability * w.reliability
            + confidence * w.confidence
            + freshness * w.freshness,
    }
}

/// 1.0 when just used or changed, halving every `half_life_days` after.
fn freshness(skill: &Skill, half_life_days: f32, now: DateTime<Utc>) -> f32 {
    let last = skill.usage.last_used_at.map_or(skill.updated_at, |used| used.max(skill.updated_at));
    let days = (now - last).num_seconds().max(0) as f32 / 86_400.0;
    if half_life_days <= 0.0 { 1.0 } else { 0.5f32.powf(days / half_life_days) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::tests::skill;
    use crate::{SkillStatus, Usage};
    use chrono::Duration;

    #[test]
    fn reliable_skills_outrank_failing_ones_at_equal_relevance() {
        let now = Utc::now();
        let w = Weights::default();
        let mut good = skill("good", "x", SkillStatus::Active);
        good.usage = Usage { use_count: 10, success_count: 9, failure_count: 1, ..Usage::default() };
        let mut bad = skill("bad", "x", SkillStatus::Active);
        bad.usage = Usage { use_count: 10, success_count: 1, failure_count: 9, ..Usage::default() };
        let new = skill("new", "x", SkillStatus::Active);

        let (g, b, n) = (score(&good, 1.0, &w, now), score(&bad, 1.0, &w, now), score(&new, 1.0, &w, now));
        assert!(g.final_score > n.final_score && n.final_score > b.final_score);
        assert_eq!(n.observed_reliability, 0.5, "new skills are treated cautiously");
    }

    #[test]
    fn freshness_halves_per_half_life() {
        let now = Utc::now();
        let mut s = skill("old", "x", SkillStatus::Active);
        s.updated_at = now - Duration::days(90);
        assert!((freshness(&s, 90.0, now) - 0.5).abs() < 0.01);
        s.usage.last_used_at = Some(now);
        assert!((freshness(&s, 90.0, now) - 1.0).abs() < 0.01, "recent use refreshes");
    }

    #[test]
    fn relevance_still_dominates_by_default() {
        let now = Utc::now();
        let w = Weights::default();
        let mut proven = skill("proven", "x", SkillStatus::Active);
        proven.usage = Usage { use_count: 30, success_count: 30, ..Usage::default() };
        let fresh = skill("fresh", "x", SkillStatus::Active);
        assert!(score(&fresh, 1.0, &w, now).final_score > score(&proven, 0.3, &w, now).final_score);
    }
}
