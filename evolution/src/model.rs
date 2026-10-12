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
    /// Whose turn it was: `None` for lyra's owner. Anyone else's run holds
    /// counts only (no task text, errors as their kind).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    /// It stopped at the tool-call limit, part done.
    #[serde(default)]
    pub limited: bool,
    /// During a trial: which side this run was on ("trial" or "control").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm: Option<String>,
    /// The conversation's turns just before it (role, text), owner's only, so
    /// a benchmark can replay a follow-up with what it follows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<(String, String)>,
    /// How its outcome came in: "rated", "signal", "judged" or "hint".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_source: Option<String>,
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
            person: None,
            limited: false,
            arm: None,
            history: Vec::new(),
            outcome_source: None,
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
    /// A subagent's instructions: always a person's call.
    Agent,
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
            Category::Agent => Level::Agent,
        }
    }

    pub fn policy(self) -> Policy {
        match self {
            Level::Skill | Level::Behavior => Policy::AutoEventually,
            Level::Workflow | Level::Tool | Level::Configuration | Level::Agent => Policy::Propose,
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
    /// A web service lyra can call, from its OpenAPI description (added to
    /// lyra's config when approved; its writes still ask each time).
    Api { api: ApiProvider },
}

/// A proposed OpenAPI provider: where its description is, how it signs in,
/// and what lyra found in it (for the person deciding).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiProvider {
    /// Its name in lyra (its capabilities are `name.operation`).
    pub name: String,
    /// The OpenAPI description's URL.
    pub spec: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// The secrets.toml entry its key goes in (`lyra secret <it>`), when it needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_scheme: Option<String>,
    /// The service's own title (its description's `info.title`).
    #[serde(default)]
    pub title: String,
    pub reads: usize,
    pub writes: usize,
    /// A few of its operations, for the person deciding.
    #[serde(default)]
    pub sample: Vec<String>,
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
            Change::Api { .. } => Category::Tool,
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
            Change::Api { api } => format!(
                "API {}{}: {} operations ({} read, {} write) from {}",
                api.name,
                if api.title.is_empty() { String::new() } else { format!(" ({})", api.title) },
                api.reads + api.writes,
                api.reads,
                api.writes,
                api.spec
            ),
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
    /// The candidate's answers judged against the current ones, pair by pair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison: Option<Comparison>,
}

impl Validation {
    /// Whether the benchmark says it's better: the pairwise comparison when
    /// there was one (the gain beat the noise), else the fitness totals.
    pub fn improves(&self) -> bool {
        match (&self.comparison, &self.fitness_before, &self.fitness_after) {
            (Some(c), ..) => c.passes,
            (None, Some(b), Some(a)) => crate::fitness::improves(b, a),
            _ => false,
        }
    }
}

/// Candidate vs current, judged blind pair by pair (E7).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    pub tasks: usize,
    /// Answers per task on each side.
    pub samples: usize,
    pub wins: usize,
    pub losses: usize,
    pub ties: usize,
    /// The candidate's mean preference per task (-1 worse … +1 better), with
    /// its 95% confidence interval.
    pub mean: f32,
    pub low: f32,
    pub high: f32,
    /// The model that judged.
    pub judge: String,
    pub passes: bool,
    /// Why it passes or not, in words.
    pub note: String,
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
    /// Deployed on trial: tried on part of the turns against the one before,
    /// kept only if it does at least as well.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial: Option<Trial>,
    /// A person's own change (the Settings page): who, and the behavior keys.
    /// A rollback past it keeps those values (3.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_by: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edited_keys: Vec<String>,
}

/// A generation on trial (E8): `share` of the turns use it, the rest the
/// generation before (the control), until `runs` of its turns have an outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trial {
    pub share: f32,
    pub runs: usize,
    pub started: DateTime<Utc>,
    pub status: TrialStatus,
    #[serde(default)]
    pub note: String,
}

/// How a trial stands.
#[derive(Debug, Clone, Serialize)]
pub struct TrialStanding {
    pub generation: u32,
    /// The generation it's compared with.
    pub parent: u32,
    pub trial: Arm,
    pub control: Arm,
    /// Turns with an outcome it needs.
    pub needed: usize,
    pub share: f32,
    pub started: DateTime<Utc>,
}

/// One side of a trial: its turns, those with an outcome, the good and corrected ones.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Arm {
    pub runs: usize,
    pub judged: usize,
    pub good: usize,
    pub corrected: usize,
}

impl Arm {
    pub fn of(runs: &[RunRecord]) -> Self {
        let judged: Vec<&RunRecord> = runs.iter().filter(|r| r.outcome != RunOutcome::Unknown).collect();
        Arm { runs: runs.len(), judged: judged.len(), good: judged.iter().filter(|r| r.outcome == RunOutcome::Success).count(), corrected: runs.iter().filter(|r| r.corrected).count() }
    }

    pub fn success_rate(&self) -> Option<f32> {
        (self.judged > 0).then(|| self.good as f32 / self.judged as f32)
    }

    pub fn correction_rate(&self) -> f32 {
        if self.runs == 0 { 0.0 } else { self.corrected as f32 / self.runs as f32 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrialStatus {
    Running,
    /// It did at least as well: it's the agent now.
    Kept,
    /// It did worse: rolled back.
    Lost,
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
        assert_eq!(Level::of(Category::Agent).policy(), Policy::Propose, "an agent's instructions are a person's call");
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

/// How lyra is doing (counts only).
#[derive(Debug, Clone, Serialize)]
pub struct Health {
    pub weeks: Vec<Week>,
    /// Outcomes by how they came in: rated, signal, judged, hint.
    pub sources: std::collections::BTreeMap<String, usize>,
    pub all: Arm,
    pub generations: Vec<GenerationHealth>,
    pub skills: Vec<SkillEffect>,
    pub trial: Option<TrialStanding>,
    pub owner_runs: usize,
    /// Everyone else's turns (counted, never shown).
    pub other_runs: usize,
}

/// One week's turns and how they went.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Week {
    pub start: chrono::NaiveDate,
    pub runs: usize,
    pub judged: usize,
    pub good: usize,
    pub partial: usize,
    pub bad: usize,
    pub corrected: usize,
    pub limited: usize,
    pub rated: usize,
}

impl Week {
    pub fn add(&mut self, r: &RunRecord) {
        self.runs += 1;
        match r.outcome {
            RunOutcome::Success => self.good += 1,
            RunOutcome::Partial => self.partial += 1,
            RunOutcome::Failure => self.bad += 1,
            RunOutcome::Unknown => {}
        }
        if r.outcome != RunOutcome::Unknown {
            self.judged += 1;
        }
        self.corrected += r.corrected as usize;
        self.limited += r.limited as usize;
        self.rated += (r.outcome_source.as_deref() == Some("rated")) as usize;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GenerationHealth {
    pub number: u32,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub trial: Option<Trial>,
    /// Its own turns (a trial's, not the control's).
    pub record: Arm,
}

/// How replies that used a skill went, against those that didn't.
#[derive(Debug, Clone, Serialize)]
pub struct SkillEffect {
    pub name: String,
    pub with: Arm,
    pub without: Arm,
}
