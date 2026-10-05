//! Changes to the skill collection that wait for approval.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A skill to be created by a merge or split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewSkill {
    pub name: String,
    pub description: String,
    pub instructions: String,
}

/// What a proposal would do when approved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    /// Refine a skill's text (a version snapshot is taken first).
    Update { skill: Uuid, description: String, instructions: String },
    /// Replace several skills with one; the originals are deprecated and superseded.
    Merge { skills: Vec<Uuid>, into: NewSkill },
    /// Replace a too-broad skill with narrower ones; the original is deprecated.
    Split { skill: Uuid, parts: Vec<NewSkill> },
    /// Proposed → active, on evidence.
    Promote { skill: Uuid },
    /// Active → deprecated, on evidence.
    Deprecate { skill: Uuid },
}

impl Change {
    pub fn kind(&self) -> &'static str {
        match self {
            Change::Update { .. } => "update",
            Change::Merge { .. } => "merge",
            Change::Split { .. } => "split",
            Change::Promote { .. } => "promote",
            Change::Deprecate { .. } => "deprecate",
        }
    }

    /// The skill this is mainly about (the first one, for merges).
    pub fn subject(&self) -> Option<Uuid> {
        match self {
            Change::Update { skill, .. }
            | Change::Split { skill, .. }
            | Change::Promote { skill }
            | Change::Deprecate { skill } => Some(*skill),
            Change::Merge { skills, .. } => skills.first().copied(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalStatus {
    Pending,
    Applied,
    Rejected,
}

impl ProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProposalStatus::Pending => "pending",
            ProposalStatus::Applied => "applied",
            ProposalStatus::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "pending" => Ok(ProposalStatus::Pending),
            "applied" => Ok(ProposalStatus::Applied),
            "rejected" => Ok(ProposalStatus::Rejected),
            _ => anyhow::bail!("unknown proposal status {s:?}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Proposal {
    pub id: Uuid,
    pub change: Change,
    pub reason: String,
    /// What it's based on, in a sentence or two.
    pub evidence: String,
    pub confidence: f32,
    pub run_id: Option<Uuid>,
    pub status: ProposalStatus,
    pub created_at: DateTime<Utc>,
}

impl Proposal {
    pub fn new(change: Change, reason: String, evidence: String, confidence: f32, run_id: Option<Uuid>) -> Self {
        Self {
            id: Uuid::new_v4(),
            change,
            reason,
            evidence,
            confidence,
            run_id,
            status: ProposalStatus::Pending,
            created_at: Utc::now(),
        }
    }
}
