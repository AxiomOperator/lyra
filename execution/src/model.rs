//! Plans as structured runtime state: goals, plans, steps, results,
//! verification, retry, approval, budgets, checkpoints and events.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// What the user wants, and what success means (P1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub id: Uuid,
    /// The user's words.
    pub request: String,
    pub description: String,
    pub success_criteria: Vec<String>,
    pub constraints: Vec<String>,
    /// What the user expects to get back.
    pub outputs: Vec<String>,
    /// The goal involves destructive or external changes.
    pub destructive: bool,
    /// Open questions that affect how to carry it out.
    pub ambiguities: Vec<String>,
    pub status: GoalStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Pending,
    Active,
    Completed,
    /// The plan finished but not every success criterion was met (P17).
    Partial,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: Uuid,
    pub goal_id: Uuid,
    pub version: u32,
    pub status: PlanStatus,
    pub steps: Vec<PlanStep>,
    pub budget: Budget,
    pub usage: BudgetUsage,
    /// Why it's paused or failed, for the user.
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Plan {
    pub fn step(&self, id: Uuid) -> Option<&PlanStep> {
        self.steps.iter().find(|s| s.id == id)
    }

    pub fn step_mut(&mut self, id: Uuid) -> Option<&mut PlanStep> {
        self.steps.iter_mut().find(|s| s.id == id)
    }

    /// A step by its key (`s3`), id prefix or 1-based position.
    pub fn find_step(&self, key: &str) -> Option<&PlanStep> {
        let key = key.trim();
        self.steps
            .iter()
            .find(|s| s.key == key || (key.len() >= 4 && s.id.to_string().starts_with(key)))
            .or_else(|| key.parse::<usize>().ok().and_then(|n| self.steps.get(n.wrapping_sub(1))))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Draft,
    Ready,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl PlanStatus {
    pub fn is_finished(self) -> bool {
        matches!(self, PlanStatus::Completed | PlanStatus::Failed | PlanStatus::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: Uuid,
    /// Short label used by the planner and commands: `s1`, `s2`, ...
    pub key: String,
    pub title: String,
    pub description: String,
    pub status: StepStatus,
    pub dependencies: Vec<Uuid>,
    pub action: StepAction,
    pub expected_outcome: Option<String>,
    pub verification: Verification,
    pub retry_policy: RetryPolicy,
    pub approval: ApprovalPolicy,
    /// Fingerprint of the action that was approved; a changed action needs
    /// approval again (P14).
    pub approved: Option<String>,
    pub idempotency: Idempotency,
    /// Exclusive resources it uses (`host:db1`, `file:/etc/x`), for parallel safety.
    pub resources: Vec<String>,
    pub result: Option<StepResult>,
    pub verification_result: Option<VerificationResult>,
    pub attempts: u32,
    pub failure_class: Option<FailureClass>,
    pub last_error: Option<String>,
    /// Operation id for unsafe actions, reused across retries (P15).
    pub operation_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl PlanStep {
    /// What approval is bound to: the action and its arguments.
    pub fn fingerprint(&self) -> String {
        serde_json::to_string(&self.action).unwrap_or_default()
    }

    pub fn needs_approval(&self) -> bool {
        self.approval == ApprovalPolicy::RequireApproval && self.approved.as_deref() != Some(&self.fingerprint())
    }

    /// Done, one way or another, as far as dependents are concerned.
    pub fn is_settled(&self) -> bool {
        matches!(self.status, StepStatus::Completed | StepStatus::Skipped)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Ready,
    Running,
    Completed,
    Failed,
    /// Waiting on something outside the plan: approval, or inspection after a crash.
    Blocked,
    Skipped,
    Cancelled,
}

/// What a step does. The planner chooses; the executor carries it out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StepAction {
    /// Call one tool with these arguments.
    Tool { tool: String, arguments: Value },
    /// Follow a named procedure (a learned skill).
    Workflow { workflow: String, input: Value },
    /// Have the model work on it, using tools as needed.
    Reasoning { instruction: String },
    /// Hand it to a scoped helper agent.
    Subagent { agent: String, task: String },
}

impl StepAction {
    pub fn kind(&self) -> &'static str {
        match self {
            StepAction::Tool { .. } => "tool",
            StepAction::Workflow { .. } => "workflow",
            StepAction::Reasoning { .. } => "reasoning",
            StepAction::Subagent { .. } => "subagent",
        }
    }
}

/// The durable result of executing a step (P4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub success: bool,
    pub output: Value,
    pub error: Option<String>,
    pub metadata: Value,
}

/// How to check a step really worked (P5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Verification {
    pub strategy: VerificationStrategy,
    /// For `follow_up_tool`: the tool to call and its arguments.
    pub tool: Option<String>,
    pub arguments: Option<Value>,
    /// What the check should show, in words (for `model_evaluation` and
    /// `follow_up_tool`), or text the output must contain (`state_check`).
    pub check: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStrategy {
    /// Trust the execution result.
    #[default]
    ToolResult,
    /// Run another tool and check its output.
    FollowUpTool,
    /// The output must contain the expected text (deterministic).
    StateCheck,
    /// The model judges the evidence against the expected outcome.
    ModelEvaluation,
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    pub verified: bool,
    pub evidence: Vec<String>,
    pub reason: Option<String>,
}

/// When to retry a failed step (P6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff_ms: u64,
    pub retry_on: Vec<FailureClass>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff_ms: 1000,
            retry_on: vec![FailureClass::Transient, FailureClass::Timeout, FailureClass::RateLimit],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    Transient,
    Timeout,
    RateLimit,
    Permission,
    InvalidInput,
    Dependency,
    Verification,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    #[default]
    Automatic,
    RequireApproval,
    Forbidden,
}

/// Whether repeating a step is harmless (P15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Idempotency {
    /// Reading, checking: repeat freely.
    #[default]
    Safe,
    /// Changes something, but only if not already done.
    Conditional,
    /// Repeating it repeats its effect (sending, deleting, paying).
    Unsafe,
}

/// Limits enforced by the runtime, not suggested to the model (P11).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Budget {
    pub max_model_calls: Option<u32>,
    pub max_tool_calls: Option<u32>,
    pub max_replans: Option<u32>,
    pub max_minutes: Option<u32>,
    pub max_tokens: Option<u64>,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_model_calls: Some(40),
            max_tool_calls: Some(100),
            max_replans: Some(3),
            max_minutes: Some(30),
            max_tokens: Some(300_000),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetUsage {
    pub model_calls: u32,
    pub tool_calls: u32,
    pub replans: u32,
    pub retries: u32,
    pub tokens: u64,
    /// Seconds spent running (summed over runs).
    pub seconds: u64,
}

/// A known-good recovery point (P9).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub plan_version: u32,
    pub completed_steps: Vec<Uuid>,
    pub runtime_state: Value,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

/// What changed when a plan was revised (P7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRevision {
    pub removed_steps: Vec<Uuid>,
    pub added_steps: Vec<PlanStep>,
    pub modified_steps: Vec<PlanStep>,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    GoalCreated,
    PlanCreated,
    PlanStarted,
    PlanPaused,
    StepReady,
    StepStarted,
    StepCompleted,
    StepFailed,
    StepBlocked,
    RetryScheduled,
    VerificationStarted,
    VerificationCompleted,
    ReplanStarted,
    PlanRevised,
    CheckpointCreated,
    ApprovalRequested,
    ApprovalGranted,
    BudgetExhausted,
    ExecutionCompleted,
    ExecutionFailed,
    ExecutionCancelled,
}

/// A structured record of something that happened (P16).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEvent {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub step_id: Option<Uuid>,
    pub kind: EventKind,
    pub message: String,
    pub data: Value,
    pub created_at: DateTime<Utc>,
}

impl ExecutionEvent {
    pub fn new(plan_id: Uuid, step_id: Option<Uuid>, kind: EventKind, message: impl Into<String>) -> Self {
        Self { id: Uuid::new_v4(), plan_id, step_id, kind, message: message.into(), data: Value::Null, created_at: Utc::now() }
    }

    pub fn data(mut self, data: Value) -> Self {
        self.data = data;
        self
    }
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

serde_names!(GoalStatus, PlanStatus, StepStatus, FailureClass, ApprovalPolicy, Idempotency, EventKind);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        assert_eq!(PlanStatus::Running.as_str(), "running");
        assert!("waiting".parse::<StepStatus>().is_err());
        assert_eq!("rate_limit".parse::<FailureClass>().unwrap(), FailureClass::RateLimit);
        assert_eq!(EventKind::PlanRevised.to_string(), "plan_revised");
    }

    #[test]
    fn approval_is_bound_to_the_action() {
        let now = Utc::now();
        let mut step = PlanStep {
            id: Uuid::new_v4(),
            key: "s1".into(),
            title: "t".into(),
            description: String::new(),
            status: StepStatus::Pending,
            dependencies: vec![],
            action: StepAction::Tool { tool: "memory_forget".into(), arguments: serde_json::json!({"id": "a"}) },
            expected_outcome: None,
            verification: Verification::default(),
            retry_policy: RetryPolicy::default(),
            approval: ApprovalPolicy::RequireApproval,
            approved: None,
            idempotency: Idempotency::Unsafe,
            resources: vec![],
            result: None,
            verification_result: None,
            attempts: 0,
            failure_class: None,
            last_error: None,
            operation_id: None,
            created_at: now,
            started_at: None,
            completed_at: None,
        };
        assert!(step.needs_approval());
        step.approved = Some(step.fingerprint());
        assert!(!step.needs_approval());
        step.action = StepAction::Tool { tool: "memory_forget".into(), arguments: serde_json::json!({"id": "b"}) };
        assert!(step.needs_approval(), "changed arguments need approval again");
    }
}
