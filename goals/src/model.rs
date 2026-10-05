//! The goal model (G1): long-lived objectives, their progress, blockers,
//! triggers, events and the autonomy policy that governs working on them.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// Suggested (by the agent) and not yet accepted.
    Proposed,
    Active,
    /// Can't progress until something changes (see its blockers).
    Blocked,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl GoalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Active => "active",
            Self::Blocked => "blocked",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_open(self) -> bool {
        matches!(self, Self::Proposed | Self::Active | Self::Blocked | Self::Paused)
    }
}

impl fmt::Display for GoalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for GoalStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        serde_json::from_value(serde_json::json!(s)).map_err(|_| format!("unknown goal status {s:?}"))
    }
}

/// How far along a goal is (G5): more than a percentage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GoalProgress {
    pub completed_items: u32,
    pub total_items: Option<u32>,
    /// Where things stand, in words.
    pub summary: String,
    pub updated_at: Option<DateTime<Utc>>,
}

/// Who asked for a goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    #[default]
    User,
    /// The agent proposed it (from a conversation, a decomposition, a review).
    Agent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub status: GoalStatus,
    /// 0–10, as given.
    pub priority: u8,
    /// 0–1: how much the user cares (separate from urgency).
    #[serde(default = "half")]
    pub importance: f32,
    pub parent_goal_id: Option<Uuid>,
    /// Goals that must be completed first.
    pub dependencies: Vec<Uuid>,
    pub success_criteria: Vec<String>,
    /// 0–1.
    pub progress: f32,
    #[serde(default)]
    pub progress_detail: GoalProgress,
    #[serde(default)]
    pub origin: Origin,
    pub created_at: DateTime<Utc>,
    pub due_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

fn half() -> f32 {
    0.5
}

impl Goal {
    pub fn new(title: &str, description: &str) -> Self {
        Self {
            id: Uuid::new_v4(),
            title: title.trim().to_string(),
            description: description.trim().to_string(),
            status: GoalStatus::Active,
            priority: 5,
            importance: 0.5,
            parent_goal_id: None,
            dependencies: Vec::new(),
            success_criteria: Vec::new(),
            progress: 0.0,
            progress_detail: GoalProgress::default(),
            origin: Origin::User,
            created_at: Utc::now(),
            due_at: None,
            completed_at: None,
        }
    }

    pub fn short(&self) -> String {
        self.id.to_string()[..8].to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerType {
    MissingInformation,
    MissingPermission,
    ExternalDependency,
    FailedDependency,
    ApprovalRequired,
    CapabilityUnavailable,
}

impl BlockerType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingInformation => "missing_information",
            Self::MissingPermission => "missing_permission",
            Self::ExternalDependency => "external_dependency",
            Self::FailedDependency => "failed_dependency",
            Self::ApprovalRequired => "approval_required",
            Self::CapabilityUnavailable => "capability_unavailable",
        }
    }
}

impl std::str::FromStr for BlockerType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        serde_json::from_value(serde_json::json!(s.replace('-', "_"))).map_err(|_| {
            format!(
                "unknown blocker type {s:?}: missing_information, missing_permission, external_dependency, failed_dependency, approval_required or capability_unavailable"
            )
        })
    }
}

/// Why a goal is stuck (G6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalBlocker {
    pub id: Uuid,
    pub goal_id: Uuid,
    pub reason: String,
    pub blocker_type: BlockerType,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// What happened to a goal; these feed Memory and Evolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalEventKind {
    Created,
    Activated,
    PlanStarted,
    PlanFinished,
    ProgressUpdated,
    Blocked,
    Unblocked,
    Paused,
    Completed,
    Failed,
    Cancelled,
    Decomposed,
    Triggered,
    Reviewed,
    Changed,
}

impl GoalEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Activated => "activated",
            Self::PlanStarted => "plan_started",
            Self::PlanFinished => "plan_finished",
            Self::ProgressUpdated => "progress_updated",
            Self::Blocked => "blocked",
            Self::Unblocked => "unblocked",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Decomposed => "decomposed",
            Self::Triggered => "triggered",
            Self::Reviewed => "reviewed",
            Self::Changed => "changed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalEvent {
    pub id: Uuid,
    pub goal_id: Uuid,
    pub kind: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

/// What wakes a goal (G9, G10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Trigger {
    /// Once, at a time.
    At { at: DateTime<Utc> },
    /// Again and again: a recurring goal reopens each time.
    Every { minutes: u64 },
    /// When another goal completes.
    After { goal: Uuid },
    /// When a condition becomes true: `env:VAR`, `file:/path`,
    /// `capability:<id or source.*>`, `goal:<id>` (completed).
    When { condition: String },
}

impl Trigger {
    pub fn describe(&self) -> String {
        match self {
            Trigger::At { at } => format!("at {}", at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")),
            Trigger::Every { minutes } => match minutes {
                m if m % 1440 == 0 => format!("every {}d", m / 1440),
                m if m % 60 == 0 => format!("every {}h", m / 60),
                m => format!("every {m}m"),
            },
            Trigger::After { goal } => format!("after goal {}", &goal.to_string()[..8]),
            Trigger::When { condition } => format!("when {condition}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalTrigger {
    pub id: Uuid,
    pub goal_id: Uuid,
    pub trigger: Trigger,
    pub last_fired: Option<DateTime<Utc>>,
    pub active: bool,
}

/// One plan's part in a goal (G4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalPlan {
    pub goal_id: Uuid,
    pub plan_id: Uuid,
    pub attempt: u32,
    /// The plan's outcome (`completed`, `partial`, `failed`, …) once it ended.
    pub outcome: Option<String>,
    pub summary: Option<String>,
    pub tokens: u64,
    pub autonomous: bool,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// How much the agent may do on its own (G11). Enforced by the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutonomyMode {
    /// Only works when explicitly asked.
    #[default]
    Reactive,
    /// May continue goals that were already worked on, asking before
    /// meaningful writes.
    Assisted,
    /// May select and execute work within the policy limits.
    Autonomous,
}

impl AutonomyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reactive => "reactive",
            Self::Assisted => "assisted",
            Self::Autonomous => "autonomous",
        }
    }
}

/// `[goals.autonomy]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AutonomyPolicy {
    pub mode: AutonomyMode,
    /// One autonomous session's limits; when one is reached the session stops.
    pub max_runtime_minutes: u64,
    pub max_tool_calls: u32,
    pub max_model_calls: u32,
    /// Money per session (0 = no limit; local models cost nothing).
    pub max_cost: f64,
    pub max_replans: u32,
    pub max_plans: u32,
    /// The riskiest kind of action autonomous work may take: `read_only`,
    /// `low_write`, `write`, `destructive`. Riskier capabilities aren't offered.
    pub max_risk: String,
    /// Minutes to wait after a session stops before another may start.
    pub cooldown_minutes: u64,
    /// Plans that failed for a goal in a row before it's blocked.
    pub max_failures: u32,
}

impl Default for AutonomyPolicy {
    fn default() -> Self {
        Self {
            mode: AutonomyMode::Reactive,
            max_runtime_minutes: 30,
            max_tool_calls: 60,
            max_model_calls: 40,
            max_cost: 0.0,
            max_replans: 2,
            max_plans: 3,
            max_risk: "write".into(),
            cooldown_minutes: 60,
            max_failures: 3,
        }
    }
}

/// How goals are ranked (G7). `[goals.priority]`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct PriorityWeights {
    pub explicit: f32,
    pub deadline: f32,
    pub dependency: f32,
    pub importance: f32,
    pub progress: f32,
    pub cost: f32,
}

impl Default for PriorityWeights {
    fn default() -> Self {
        Self { explicit: 0.35, deadline: 0.25, dependency: 0.15, importance: 0.1, progress: 0.1, cost: 0.05 }
    }
}

/// Why a goal ranks where it does.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PriorityScore {
    pub explicit: f32,
    pub deadline: f32,
    pub dependency: f32,
    pub importance: f32,
    pub progress: f32,
    pub cost: f32,
    pub total: f32,
}
