use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A reusable procedure the agent learned. (Facts belong in memory, not here.)
///
/// Content and lifecycle live in the skill's Markdown file; the usage numbers
/// come from the ledger and are filled in by the manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: Uuid,
    /// Short kebab-case name, unique across all statuses.
    pub name: String,
    /// When this skill applies.
    pub description: String,
    /// The procedure or rule itself.
    pub instructions: String,
    /// What it was learned from, e.g. `conversation`.
    pub source: String,
    /// How sure the reviewer was when it learned the skill.
    pub confidence: f32,
    pub status: SkillStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip)]
    pub usage: Usage,
}

/// How a skill has fared in the runs that used it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub use_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub partial_count: u64,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl Usage {
    /// Uses whose outcome is known.
    pub fn completed(&self) -> u64 {
        self.success_count + self.failure_count + self.partial_count
    }

    /// `success / completed`, or `None` before any outcome is known.
    pub fn success_rate(&self) -> Option<f32> {
        let done = self.completed();
        (done > 0).then(|| self.success_count as f32 / done as f32)
    }

    /// Observed reliability, smoothed so a new skill starts at 0.5 rather
    /// than 0 or 1: `(successes + 1) / (completed + 2)`.
    pub fn reliability(&self) -> f32 {
        (self.success_count as f32 + 1.0) / (self.completed() as f32 + 2.0)
    }
}

/// Proposed skills wait for approval (or, in auto mode, earn it on trial);
/// active ones are used; rejected and deprecated ones are kept but unused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillStatus {
    Proposed,
    Active,
    Rejected,
    Deprecated,
}

/// How a run that used a skill turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillOutcome {
    Unknown,
    Success,
    Failure,
    Partial,
}

/// How two skills relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relationship {
    Supersedes,
    Extends,
    ConflictsWith,
    RelatedTo,
}

/// `as_str`, `Display` and `FromStr` for the lowercase names these enums are stored under.
macro_rules! names {
    ($ty:ident { $($variant:ident = $name:literal),+ $(,)? }) => {
        impl $ty {
            pub fn as_str(self) -> &'static str {
                match self { $($ty::$variant => $name),+ }
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $ty {
            type Err = anyhow::Error;

            fn from_str(s: &str) -> anyhow::Result<Self> {
                match s {
                    $($name => Ok($ty::$variant),)+
                    _ => anyhow::bail!(concat!("unknown ", stringify!($ty), " {:?}"), s),
                }
            }
        }
    };
}

names!(SkillStatus {
    Proposed = "proposed",
    Active = "active",
    Rejected = "rejected",
    Deprecated = "deprecated",
});
names!(SkillOutcome { Unknown = "unknown", Success = "success", Failure = "failure", Partial = "partial" });
names!(Relationship {
    Supersedes = "supersedes",
    Extends = "extends",
    ConflictsWith = "conflicts_with",
    RelatedTo = "related_to",
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reliability_is_smoothed() {
        let new = Usage::default();
        assert_eq!(new.reliability(), 0.5);
        assert_eq!(new.success_rate(), None);

        let good = Usage { use_count: 20, success_count: 18, failure_count: 2, ..Usage::default() };
        assert!((good.reliability() - 19.0 / 22.0).abs() < 1e-6);
        assert_eq!(good.success_rate(), Some(0.9));

        let one_win = Usage { use_count: 1, success_count: 1, ..Usage::default() };
        assert!(one_win.reliability() < 0.7, "one success isn't proof");
    }

    #[test]
    fn names_round_trip() {
        for s in [SkillStatus::Proposed, SkillStatus::Active, SkillStatus::Rejected, SkillStatus::Deprecated] {
            assert_eq!(s.as_str().parse::<SkillStatus>().unwrap(), s);
        }
        assert_eq!("conflicts_with".parse::<Relationship>().unwrap(), Relationship::ConflictsWith);
        assert!("maybe".parse::<SkillOutcome>().is_err());
    }
}
