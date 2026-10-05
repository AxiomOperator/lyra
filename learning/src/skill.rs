use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A reusable procedure the agent learned. (Facts belong in memory, not here.)
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
    pub confidence: f32,
    pub status: SkillStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Proposed skills wait for approval; only active ones are used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillStatus {
    Proposed,
    Active,
    Rejected,
}

impl SkillStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SkillStatus::Proposed => "proposed",
            SkillStatus::Active => "active",
            SkillStatus::Rejected => "rejected",
        }
    }
}

impl fmt::Display for SkillStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SkillStatus {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "proposed" => Ok(SkillStatus::Proposed),
            "active" => Ok(SkillStatus::Active),
            "rejected" => Ok(SkillStatus::Rejected),
            _ => anyhow::bail!("unknown skill status {s:?}"),
        }
    }
}
