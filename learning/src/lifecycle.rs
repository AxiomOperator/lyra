//! Evidence-driven lifecycle (V6): which proposed skills have earned
//! promotion and which active ones should be deprecated. Pure policy; the
//! manager decides whether to apply a suggestion or propose it.

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::{Skill, SkillStatus};

/// Thresholds, configurable under `[learning.lifecycle]`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct Policy {
    /// A proposed skill needs at least this learned confidence to be promoted
    /// (and, in auto mode, to be tried out before approval).
    pub promote_confidence: f32,
    /// ...and this many successful uses...
    pub promote_successes: u64,
    /// ...and no failures.
    pub promote_max_failures: u64,
    /// An active skill below this reliability is deprecated...
    pub deprecate_reliability: f32,
    /// ...once it has at least this many known outcomes.
    pub deprecate_min_uses: u64,
    /// Unused this many days (and at least this old) counts as stale.
    pub stale_days: i64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            promote_confidence: 0.85,
            promote_successes: 2,
            promote_max_failures: 0,
            deprecate_reliability: 0.4,
            deprecate_min_uses: 5,
            stale_days: 90,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Promote,
    Deprecate,
}

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub skill: Uuid,
    pub name: String,
    pub transition: Transition,
    pub reason: String,
    pub evidence: String,
}

/// Lifecycle changes the evidence supports.
pub fn review(skills: &[Skill], p: &Policy) -> Vec<Suggestion> {
    let mut out = Vec::new();
    for s in skills {
        let u = &s.usage;
        let evidence = format!(
            "{} uses: {} succeeded, {} failed, {} partial; reliability {:.2}; confidence {:.2}",
            u.use_count, u.success_count, u.failure_count, u.partial_count, u.reliability(), s.confidence
        );
        let suggestion = |transition, reason: String| Suggestion {
            skill: s.id,
            name: s.name.clone(),
            transition,
            reason,
            evidence: evidence.clone(),
        };
        match s.status {
            SkillStatus::Proposed
                if s.confidence >= p.promote_confidence
                    && u.success_count >= p.promote_successes
                    && u.failure_count <= p.promote_max_failures =>
            {
                let reason = format!("validated by {} successful uses", u.success_count);
                out.push(suggestion(Transition::Promote, reason));
            }
            SkillStatus::Active
                if u.completed() >= p.deprecate_min_uses && u.reliability() < p.deprecate_reliability =>
            {
                let reason = format!("reliability {:.2} below {:.2}", u.reliability(), p.deprecate_reliability);
                out.push(suggestion(Transition::Deprecate, reason));
            }
            _ => {}
        }
    }
    out
}

/// Active skills nobody has used for `stale_days` (and older than that).
pub fn stale<'a>(skills: &'a [Skill], p: &Policy, now: DateTime<Utc>) -> Vec<&'a Skill> {
    let cutoff = now - Duration::days(p.stale_days);
    skills
        .iter()
        .filter(|s| s.status == SkillStatus::Active && s.created_at < cutoff)
        .filter(|s| s.usage.last_used_at.is_none_or(|used| used < cutoff))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::tests::skill;
    use crate::Usage;

    fn with(status: SkillStatus, confidence: f32, s: u64, f: u64) -> Skill {
        let mut sk = skill("x", "x", status);
        sk.confidence = confidence;
        sk.usage = Usage { use_count: s + f, success_count: s, failure_count: f, ..Usage::default() };
        sk
    }

    #[test]
    fn promotion_needs_confidence_successes_and_no_failures() {
        let p = Policy::default();
        let t = |s: &Skill| review(std::slice::from_ref(s), &p).first().map(|x| x.transition);
        assert_eq!(t(&with(SkillStatus::Proposed, 0.9, 2, 0)), Some(Transition::Promote));
        assert_eq!(t(&with(SkillStatus::Proposed, 0.8, 5, 0)), None, "low confidence");
        assert_eq!(t(&with(SkillStatus::Proposed, 0.9, 1, 0)), None, "not enough uses");
        assert_eq!(t(&with(SkillStatus::Proposed, 0.9, 3, 1)), None, "a failure");
    }

    #[test]
    fn deprecation_needs_enough_evidence() {
        let p = Policy::default();
        let t = |s: &Skill| review(std::slice::from_ref(s), &p).first().map(|x| x.transition);
        assert_eq!(t(&with(SkillStatus::Active, 0.9, 1, 6)), Some(Transition::Deprecate));
        assert_eq!(t(&with(SkillStatus::Active, 0.9, 0, 3)), None, "too few uses to judge");
        assert_eq!(t(&with(SkillStatus::Active, 0.9, 8, 2)), None, "reliable");
        assert_eq!(t(&with(SkillStatus::Deprecated, 0.9, 0, 9)), None, "already deprecated");
    }

    #[test]
    fn stale_skills() {
        let now = Utc::now();
        let p = Policy::default();
        let mut old = skill("old", "x", SkillStatus::Active);
        old.created_at = now - Duration::days(200);
        let mut used = old.clone();
        used.usage.last_used_at = Some(now - Duration::days(3));
        let young = skill("young", "x", SkillStatus::Active);
        let all = [old, used, young];
        let found: Vec<&str> = stale(&all, &p, now).iter().map(|s| s.name.as_str()).collect();
        assert_eq!(found, ["old"]);
    }
}
