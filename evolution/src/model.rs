//! What evolution works with: run telemetry (E1), improvement opportunities
//! (E2), candidate changes and their policy, generations and fitness.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::composite::CompositeTool;
use crate::workflow::WorkflowDef;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    /// One chat turn.
    Chat,
    /// One plan run.
    Plan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Unknown,
    Success,
    Partial,
    Failure,
}

/// Telemetry for one run (E1): the evidence evolution works from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: Uuid,
    pub kind: RunKind,
    /// What was asked (first part of the user's message or the plan's goal).
    pub task: String,
    pub model_calls: u32,
    pub tool_calls: u32,
    /// Retried or failed-then-redone work (tool errors in chat; retries in plans).
    pub retries: u32,
    pub replans: u32,
    /// Plans: attempts that failed, steps that failed and then recovered,
    /// and results that didn't pass verification.
    #[serde(default)]
    pub failed_attempts: u32,
    #[serde(default)]
    pub recoveries: u32,
    #[serde(default)]
    pub verification_failures: u32,
    pub tokens: u64,
    pub duration_ms: u64,
    pub outcome: RunOutcome,
    /// The user's next message corrected this run.
    pub corrected: bool,
    /// What the user said about it (the correction), when they reacted.
    #[serde(default)]
    pub feedback: Option<String>,
    pub skills_used: Vec<String>,
    /// Tool names in call order.
    pub tools_used: Vec<String>,
    pub errors: Vec<String>,
    /// The agent generation that did the work.
    pub generation: u32,
    pub created_at: DateTime<Utc>,
}

impl RunRecord {
    pub fn new(kind: RunKind, task: &str, generation: u32) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind,
            task: task.chars().take(300).collect(),
            model_calls: 0,
            tool_calls: 0,
            retries: 0,
            replans: 0,
            failed_attempts: 0,
            recoveries: 0,
            verification_failures: 0,
            tokens: 0,
            duration_ms: 0,
            outcome: RunOutcome::Unknown,
            corrected: false,
            feedback: None,
            skills_used: Vec::new(),
            tools_used: Vec::new(),
            errors: Vec::new(),
            generation,
            created_at: Utc::now(),
        }
    }
}

/// What kind of change a candidate makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Response guidelines added to the system prompt.
    Prompt,
    /// A behavior setting (a whitelisted knob).
    Configuration,
    /// A workflow definition used in planning.
    Workflow,
    /// A refinement of a learned skill (through the skills system).
    Skill,
    /// A composite tool built from existing tools.
    Tool,
    /// A source-code patch (sandboxed, never deployed automatically).
    Code,
    /// Revised instructions for a specialist subagent (a new agent version).
    Agent,
}

/// How dangerous a mutation is, and so what approval it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Skill,
    Behavior,
    Workflow,
    Tool,
    Configuration,
    Code,
    Architecture,
}

/// Who may deploy a change at a level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// May deploy on its own in auto mode, once it beats the baseline.
    AutoEventually,
    /// Always waits for approval.
    Propose,
    /// Approval, and never applied to the running agent (code: a branch only).
    Manual,
    /// Never generated or deployed.
    Never,
}

impl Level {
    pub fn of(category: Category) -> Level {
        match category {
            Category::Prompt => Level::Behavior,
            Category::Configuration => Level::Configuration,
            Category::Workflow => Level::Workflow,
            Category::Skill => Level::Skill,
            Category::Tool => Level::Tool,
            Category::Code => Level::Code,
            Category::Agent => Level::Behavior,
        }
    }

    pub fn policy(self) -> Policy {
        match self {
            Level::Skill | Level::Behavior => Policy::AutoEventually,
            Level::Workflow | Level::Tool | Level::Configuration => Policy::Propose,
            Level::Code => Policy::Manual,
            Level::Architecture => Policy::Never,
        }
    }
}

/// An improvement opportunity found in the telemetry (E2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opportunity {
    /// Which detector found it, e.g. `inefficient_runs`.
    pub kind: String,
    pub problem: String,
    /// Categories worth considering.
    pub categories: Vec<Category>,
    /// The runs that show it.
    pub evidence: Vec<Uuid>,
    /// Extra facts for the evolver (the repeated tool sequence, the error…).
    pub details: Value,
}

/// The concrete change a candidate would make.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Change {
    Prompt { add: Vec<String>, remove: Vec<String> },
    Configuration { key: String, from: Value, to: Value },
    Workflow { workflow: WorkflowDef },
    Skill { skill: String, instructions: String },
    Tool { tool: CompositeTool },
    Code { base_commit: String, diff: String },
    Agent { agent: String, instructions: String },
}

impl Change {
    pub fn category(&self) -> Category {
        match self {
            Change::Prompt { .. } => Category::Prompt,
            Change::Configuration { .. } => Category::Configuration,
            Change::Workflow { .. } => Category::Workflow,
            Change::Skill { .. } => Category::Skill,
            Change::Tool { .. } => Category::Tool,
            Change::Code { .. } => Category::Code,
            Change::Agent { .. } => Category::Agent,
        }
    }

    /// One line for lists.
    pub fn summary(&self) -> String {
        match self {
            Change::Prompt { add, remove } => {
                let mut parts: Vec<String> = add.iter().map(|g| format!("+ \"{g}\"")).collect();
                parts.extend(remove.iter().map(|g| format!("- \"{g}\"")));
                format!("guidelines {}", parts.join(" "))
            }
            Change::Configuration { key, from, to } => format!("{key}: {from} → {to}"),
            Change::Workflow { workflow } => {
                format!("workflow {}: {}", workflow.name, workflow.phases.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(" → "))
            }
            Change::Skill { skill, .. } => format!("refine skill {skill}"),
            Change::Agent { agent, .. } => format!("refine agent {agent}'s instructions"),
            Change::Tool { tool } => format!(
                "tool {}({}) = {}",
                tool.name,
                tool.inputs.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", "),
                tool.steps.iter().map(|s| s.tool.as_str()).collect::<Vec<_>>().join(" + ")
            ),
            Change::Code { diff, .. } => format!("code patch ({} lines)", diff.lines().count()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    Proposed,
    Testing,
    Rejected,
    Approved,
    Deployed,
    RolledBack,
}

/// What the validation lab found.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Validation {
    /// Static checks passed (structure, policy, safety).
    pub valid: bool,
    pub notes: Vec<String>,
    pub fitness_before: Option<Fitness>,
    pub fitness_after: Option<Fitness>,
    /// For code: the checks run in the sandbox, and their output tails.
    pub checks: Vec<(String, bool, String)>,
}

/// A proposed improvement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub id: Uuid,
    /// Candidates competing on the same problem share a group (E7).
    pub group: Uuid,
    pub level: Level,
    pub problem: String,
    pub rationale: String,
    pub change: Change,
    pub evidence: Vec<Uuid>,
    pub confidence: f32,
    pub status: CandidateStatus,
    pub validation: Option<Validation>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Candidate {
    pub fn short(&self) -> String {
        self.id.to_string()[..8].to_string()
    }
}

/// The evolvable state of the agent at one point (files as text).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub behavior: String,
    pub workflows: BTreeMap<String, String>,
    pub tools: BTreeMap<String, String>,
}

/// A behavioral generation: what the agent was, and where it came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Generation {
    pub id: Uuid,
    pub number: u32,
    pub parent: Option<Uuid>,
    pub snapshot: Snapshot,
    /// The candidate that produced it (or why it exists: initial, rollback).
    pub candidate: Option<Uuid>,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    /// A skill revision this generation made. Skills live in the skill
    /// system (files and ledger), so the snapshot doesn't hold them; rolling
    /// back past this generation restores `from_version`.
    #[serde(default)]
    pub skill: Option<SkillRevision>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillRevision {
    pub name: String,
    pub from_version: i64,
    pub to_version: i64,
    /// A subagent's version rather than a skill's.
    #[serde(default)]
    pub agent: bool,
}

/// How good a version of the agent is on a benchmark (E7). All 0–1.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Fitness {
    pub success_rate: f32,
    pub accuracy: f32,
    pub efficiency: f32,
    pub reliability: f32,
    pub safety: f32,
    pub total: f32,
    /// Raw averages, for display.
    pub tool_calls: f32,
    pub model_calls: f32,
    pub tokens: f32,
    pub seconds: f32,
}

/// One benchmark task's result.
#[derive(Debug, Clone, Default)]
pub struct BenchResult {
    pub success: bool,
    /// 0–1, from the judge.
    pub accuracy: f32,
    pub model_calls: u32,
    pub tool_calls: u32,
    pub tokens: u64,
    pub seconds: f32,
    pub errors: u32,
    /// Attempts to change or destroy something during a benchmark.
    pub safety_violations: u32,
}

/// The weights of the fitness function. `[evolution.fitness]`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct FitnessWeights {
    pub success: f32,
    pub accuracy: f32,
    pub efficiency: f32,
    pub reliability: f32,
    pub safety: f32,
}

impl Default for FitnessWeights {
    fn default() -> Self {
        Self { success: 0.40, accuracy: 0.25, efficiency: 0.15, reliability: 0.10, safety: 0.10 }
    }
}

/// One immutable entry in the evolution history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionEvent {
    pub id: Uuid,
    pub candidate: Option<Uuid>,
    pub kind: String,
    pub description: String,
    pub old_generation: Option<u32>,
    pub new_generation: Option<u32>,
    pub fitness_before: Option<f32>,
    pub fitness_after: Option<f32>,
    /// The kind of change the candidate makes, for candidate events.
    #[serde(default)]
    pub category: Option<Category>,
    /// The runs that were the candidate's evidence.
    #[serde(default)]
    pub evidence: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// `as_str`, `Display` and `FromStr` via the serde names.
macro_rules! serde_names {
    ($($ty:ty),+) => {$(
        impl $ty {
            pub fn as_str(&self) -> String {
                serde_json::to_value(self).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.as_str())
            }
        }

        impl FromStr for $ty {
            type Err = anyhow::Error;

            fn from_str(s: &str) -> anyhow::Result<Self> {
                serde_json::from_value(Value::String(s.to_string())).map_err(|_| anyhow::anyhow!("unknown value {s:?}"))
            }
        }
    )+};
}

serde_names!(RunKind, RunOutcome, Category, Level, CandidateStatus);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies_follow_the_danger_of_the_level() {
        assert_eq!(Level::of(Category::Prompt).policy(), Policy::AutoEventually);
        assert_eq!(Level::of(Category::Skill).policy(), Policy::AutoEventually);
        assert_eq!(Level::of(Category::Workflow).policy(), Policy::Propose);
        assert_eq!(Level::of(Category::Tool).policy(), Policy::Propose);
        assert_eq!(Level::of(Category::Configuration).policy(), Policy::Propose);
        assert_eq!(Level::of(Category::Code).policy(), Policy::Manual);
        assert_eq!(Level::Architecture.policy(), Policy::Never);
    }

    #[test]
    fn names_round_trip() {
        assert_eq!("rolled_back".parse::<CandidateStatus>().unwrap(), CandidateStatus::RolledBack);
        assert_eq!(Category::Configuration.to_string(), "configuration");
    }
}
