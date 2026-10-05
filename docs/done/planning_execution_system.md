# Rust Agent Planning and Execution System — Development Plan

## Objective

Build a structured planning and execution subsystem that lets the agent:

- convert a user request into a goal,
- generate an executable plan,
- represent dependencies between steps,
- execute tools in a controlled order,
- verify outcomes,
- recover from failures,
- replan only when necessary,
- persist progress,
- resume interrupted work,
- delegate work to subagents later,
- enforce budgets and safety limits,
- and feed outcomes into Memory, Skills, and Self-Evolution.

The planning system should not simply produce prose. Plans must be represented as structured runtime state.

```text
Memory
└── what the agent knows

Skills
└── how the agent performs known procedures

Planning
└── what needs to happen

Execution
└── performs the plan

Verification
└── determines whether it actually worked

Evolution
└── improves these systems over time
```

---

# Core Architecture

```text
                    User Request
                         │
                         ▼
                    Goal Parser
                         │
                         ▼
                 Context Builder
                         │
          ┌──────────────┼──────────────┐
          ▼              ▼              ▼
       Memory          Skills      Runtime State
          │              │              │
          └──────────────┼──────────────┘
                         ▼
                       Planner
                         │
                         ▼
                     Task Graph
                         │
                         ▼
                     Scheduler
                         │
                         ▼
                      Executor
                         │
              ┌──────────┼──────────┐
              ▼          ▼          ▼
            Tools     Workflows   Subagents
              │          │          │
              └──────────┼──────────┘
                         ▼
                      Verifier
                         │
                 ┌───────┴────────┐
                 ▼                ▼
              Success           Failure
                 │                │
                 ▼                ▼
              Complete        Recovery/Replan
                 │
                 ▼
              Outcome
                 │
       ┌─────────┼─────────┐
       ▼         ▼         ▼
     Memory    Skills   Evolution
```

---

# Core Data Models

## Goal

```rust
pub struct Goal {
    pub id: Uuid,
    pub description: String,
    pub success_criteria: Vec<String>,
    pub constraints: Vec<String>,
    pub status: GoalStatus,
    pub created_at: DateTime<Utc>,
}
```

```rust
pub enum GoalStatus {
    Pending,
    Active,
    Completed,
    Failed,
    Cancelled,
}
```

The goal should define **what success means**, not just restate the user's request.

---

## Plan

```rust
pub struct Plan {
    pub id: Uuid,
    pub goal_id: Uuid,
    pub version: u32,
    pub status: PlanStatus,
    pub steps: Vec<PlanStep>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

```rust
pub enum PlanStatus {
    Draft,
    Ready,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}
```

---

## Plan Step

```rust
pub struct PlanStep {
    pub id: Uuid,
    pub title: String,
    pub description: String,

    pub status: StepStatus,
    pub dependencies: Vec<Uuid>,

    pub action: StepAction,

    pub expected_outcome: Option<String>,
    pub result: Option<StepResult>,

    pub retry_policy: RetryPolicy,

    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}
```

```rust
pub enum StepStatus {
    Pending,
    Ready,
    Running,
    Completed,
    Failed,
    Blocked,
    Skipped,
    Cancelled,
}
```

---

## Step Action

```rust
pub enum StepAction {
    Tool {
        tool: String,
        arguments: serde_json::Value,
    },

    Workflow {
        workflow: String,
        input: serde_json::Value,
    },

    Reasoning {
        instruction: String,
    },

    Subagent {
        agent: String,
        task: String,
    },
}
```

This keeps the planner independent from the executor.

---

# P1 — Goal Parsing

## Goal

Convert the user request into a normalized goal.

Input:

```text
"Deploy PostgreSQL and verify that it is working."
```

Output:

```json
{
  "description": "Deploy PostgreSQL and verify successful operation",
  "success_criteria": [
    "PostgreSQL is running",
    "Database accepts connections",
    "Health verification succeeds"
  ],
  "constraints": []
}
```

The parser should identify:

- primary objective,
- success criteria,
- explicit constraints,
- required output,
- potentially destructive operations,
- ambiguity that affects execution.

Do not make the planner infer success only after execution.

### Completion Criteria

P1 is complete when the system consistently produces an explicit, machine-readable goal.

---

# P2 — Structured Plan Generation

## Goal

Generate an ordered set of executable steps.

Example:

```text
1. Inspect environment
2. Check prerequisites
3. Install PostgreSQL
4. Configure service
5. Start service
6. Verify service
7. Test database connection
```

The model should return structured output rather than free-form Markdown.

Example:

```json
{
  "steps": [
    {
      "title": "Inspect environment",
      "dependencies": []
    },
    {
      "title": "Install PostgreSQL",
      "dependencies": ["step-1"]
    }
  ]
}
```

Each step must have:

- a specific objective,
- dependencies,
- intended action type,
- expected outcome.

### Rule

A step should be small enough to verify independently but large enough to avoid micro-planning every command.

### Completion Criteria

P2 is complete when generated plans can be parsed and validated without interpreting prose.

---

# P3 — Dependency Graph

## Goal

Turn the linear plan into a DAG where appropriate.

Example:

```text
                 Inspect Host
                      │
            ┌─────────┴─────────┐
            ▼                   ▼
      Check Storage        Check Network
            │                   │
            └─────────┬─────────┘
                      ▼
                 Install App
                      │
                      ▼
                    Verify
```

The scheduler should only mark a step `Ready` when all required dependencies are complete.

Validation must reject:

- missing dependencies,
- circular dependencies,
- self-dependencies.

Rust structure:

```rust
pub struct TaskGraph {
    pub steps: HashMap<Uuid, PlanStep>,
}
```

Provide:

```rust
fn ready_steps(&self) -> Vec<&PlanStep>;
fn blocked_steps(&self) -> Vec<&PlanStep>;
fn validate(&self) -> Result<()>;
```

### Completion Criteria

P3 is complete when the runtime can reliably determine which steps are executable.

---

# P4 — Execution Engine

## Goal

Execute plan steps deterministically.

Flow:

```text
Scheduler
   │
   ▼
Ready Step
   │
   ▼
Executor
   │
   ▼
Resolve Action
   │
   ├── Tool
   ├── Workflow
   ├── Reasoning
   └── Subagent
```

Core trait:

```rust
#[async_trait]
pub trait StepExecutor {
    async fn execute(
        &self,
        step: &PlanStep,
        context: &ExecutionContext,
    ) -> Result<StepResult>;
}
```

Result:

```rust
pub struct StepResult {
    pub success: bool,
    pub output: serde_json::Value,
    pub error: Option<String>,
    pub metadata: serde_json::Value,
}
```

Every execution should produce a durable result object.

### Completion Criteria

P4 is complete when a structured plan can execute sequentially through real tools.

---

# P5 — Verification

## Goal

Do not trust tool success alone.

Separate:

```text
Execution Result
```

from:

```text
Verified Outcome
```

Example:

```text
systemctl start postgresql
```

returning exit code `0` does not prove PostgreSQL works.

Verification might require:

```text
systemctl is-active
pg_isready
connection test
```

Add:

```rust
pub struct VerificationResult {
    pub verified: bool,
    pub evidence: Vec<String>,
    pub reason: Option<String>,
}
```

Verification types:

```rust
pub enum VerificationStrategy {
    ToolResult,
    FollowUpTool,
    StateCheck,
    ModelEvaluation,
    Custom(String),
}
```

Prefer deterministic verification over LLM judgment whenever possible.

### Completion Criteria

P5 is complete when important actions can be independently verified.

---

# P6 — Failure Handling and Retry

## Goal

Handle predictable transient failures without immediately replanning.

```rust
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff_ms: u64,
    pub retry_on: Vec<FailureClass>,
}
```

Failure classes:

```rust
pub enum FailureClass {
    Transient,
    Timeout,
    RateLimit,
    Permission,
    InvalidInput,
    Dependency,
    Unknown,
}
```

Example:

```text
Timeout
   ↓
retry

Authentication failure
   ↓
do not blindly retry
```

Track:

```text
attempt_count
failure_class
last_error
```

### Completion Criteria

P6 is complete when transient failures can recover without unnecessary replanning.

---

# P7 — Replanning

## Goal

Modify only the broken portion of a plan.

Do not regenerate the entire plan after every failure.

Example:

```text
A → B → C → D
        │
        X
```

Instead of replacing everything:

```text
A → B → C2 → C3 → D
```

Preserve successful steps.

The replanner should receive:

```text
original goal
current plan
completed steps
failed step
failure evidence
current runtime state
relevant skills
```

Output:

```rust
pub struct PlanRevision {
    pub removed_steps: Vec<Uuid>,
    pub added_steps: Vec<PlanStep>,
    pub modified_steps: Vec<PlanStep>,
    pub reason: String,
}
```

Increment plan version:

```text
Plan v1
   ↓
failure
   ↓
Plan v2
```

### Completion Criteria

P7 is complete when the agent can recover from failures without discarding completed work.

---

# P8 — Persistence and Resume

## Goal

Long-running execution must survive runtime restart.

Persist:

```text
goal
plan
steps
step results
verification results
retry state
plan revisions
```

Recommended tables:

```text
goals
plans
plan_steps
step_executions
verification_results
plan_revisions
```

On restart:

```text
Runtime starts
     │
     ▼
Find incomplete executions
     │
     ▼
Recover state
     │
     ▼
Check previously running step
     │
     ▼
Resume safely
```

Never blindly rerun a previously `Running` destructive step.

Its actual state must first be inspected.

### Completion Criteria

P8 is complete when execution can recover after process termination without starting over.

---

# P9 — Checkpoints

## Goal

Create known-good recovery points during long tasks.

Example:

```text
Step 1 ✓
Step 2 ✓
Step 3 ✓
──── checkpoint ────
Step 4
Step 5
```

Checkpoint:

```rust
pub struct Checkpoint {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub completed_steps: Vec<Uuid>,
    pub runtime_state: serde_json::Value,
    pub created_at: DateTime<Utc>,
}
```

Use checkpoints before:

- destructive operations,
- major workflow stages,
- external mutations,
- long-running branches.

---

# P10 — Context-Aware Planning

## Goal

Integrate Memory and Skills directly into planning.

Before creating a plan:

```text
Goal
 │
 ├── retrieve relevant memory
 ├── retrieve relevant skills
 ├── inspect runtime capabilities
 └── inspect available tools
        │
        ▼
      Planner
```

Planner context should include only relevant information.

Avoid dumping the entire memory or skill store.

---

# P11 — Budget Management

## Goal

Prevent runaway agents.

Define:

```rust
pub struct ExecutionBudget {
    pub max_model_calls: Option<u32>,
    pub max_tool_calls: Option<u32>,
    pub max_replans: Option<u32>,
    pub max_duration: Option<Duration>,
    pub max_tokens: Option<u64>,
}
```

Track usage continuously.

```text
Budget
├── model calls: 8 / 20
├── tool calls: 14 / 50
├── replans: 1 / 3
└── tokens: 31k / 100k
```

When approaching limits, the planner may simplify the remaining plan.

On exhaustion:

```text
pause
fail safely
or request operator intervention
```

Do not silently ignore budgets.

---

# P12 — Parallel Execution

## Goal

Execute independent steps concurrently.

Example:

```text
                 Gather State
                      │
           ┌──────────┼──────────┐
           ▼          ▼          ▼
         DNS        Network     Storage
           │          │          │
           └──────────┼──────────┘
                      ▼
                    Plan
```

Use Tokio tasks for independent safe operations.

But parallelism must respect:

- dependencies,
- mutable resources,
- rate limits,
- locking,
- destructive operations.

Introduce resource locks:

```text
host:server01
file:/etc/config
database:main
```

Two steps requiring the same exclusive resource should not run simultaneously.

---

# P13 — Subagents

## Goal

Delegate complex branches to specialized agents.

Example:

```text
Main Agent
    │
    ├── Network Agent
    ├── Database Agent
    └── Research Agent
```

A subagent receives:

```text
goal
scope
context
tools
budget
expected output
```

Not the entire parent context by default.

Return:

```rust
pub struct SubagentResult {
    pub status: RunOutcome,
    pub summary: String,
    pub evidence: Vec<serde_json::Value>,
}
```

The parent remains responsible for verification.

---

# P14 — Human Approval Gates

## Goal

Allow controlled execution of high-risk steps.

Step policy:

```rust
pub enum ApprovalPolicy {
    Automatic,
    RequireApproval,
    Forbidden,
}
```

Examples requiring approval:

```text
delete data
restart production systems
modify authentication
change firewall rules
deploy generated code
```

Approval should be attached to the specific action and arguments.

If parameters change after approval, approval must be requested again.

---

# P15 — Idempotency

## Goal

Make retries and resume safe.

Each mutable step should define whether it is:

```rust
pub enum Idempotency {
    Safe,
    Conditional,
    Unsafe,
}
```

Examples:

```text
read file
→ Safe

create user if absent
→ Conditional

send email
→ Unsafe
```

Unsafe actions should use operation IDs when supported.

Example:

```text
execution_id = 4a7...
```

Prevents duplicate actions after retry or restart.

---

# P16 — Execution Events

Everything should emit structured events.

```rust
pub enum ExecutionEvent {
    GoalCreated,
    PlanCreated,
    PlanStarted,

    StepReady,
    StepStarted,
    StepCompleted,
    StepFailed,

    RetryScheduled,

    VerificationStarted,
    VerificationCompleted,

    ReplanStarted,
    PlanRevised,

    CheckpointCreated,

    ExecutionCompleted,
    ExecutionFailed,
}
```

These events become valuable to:

```text
UI
logs
Memory
Skills
Evolution
observability
```

---

# P17 — Outcome Evaluation

At the end of execution, evaluate the **goal**, not merely the plan.

```text
All steps complete
      │
      ▼
Check success criteria
      │
  ┌───┴────┐
  ▼        ▼
success  incomplete
```

Example:

The plan completed but the final endpoint still returns `500`.

The execution should therefore be marked:

```text
Failed / Partial
```

not successful.

---

# P18 — Integration With Memory

Planning should consume memory:

```text
Previous environment state
Past decisions
Known constraints
Previous failures
```

Execution should create memory candidates:

```text
new configuration
important outcome
changed system state
durable discovery
```

But the planning subsystem should not write durable memory directly.

Send candidates to `MemoryManager`.

---

# P19 — Integration With Skills

Before planning:

```text
Goal
 ↓
Skill Search
 ↓
Relevant Procedures
 ↓
Planner
```

After execution:

```text
successful recovery
repeated discovery
user correction
      │
      ▼
Skill Learning Evaluator
```

A plan itself should not automatically become a skill.

Only reusable procedures should.

---

# P20 — Integration With Self-Evolution

The execution subsystem provides the primary evidence for evolution.

Record:

```text
model calls
tool calls
retries
replans
failed steps
successful recoveries
execution time
skills used
verification failures
```

Evolution can then detect:

```text
repeated planning inefficiency
bad retry policy
missing tool
poor workflow
unnecessary model calls
```

---

# Database Model

Recommended mature schema:

```text
goals
plans
plan_steps
step_dependencies
step_executions
step_attempts
verification_results
plan_revisions
checkpoints
execution_events
execution_budgets
resource_locks
```

Do not duplicate full tool outputs everywhere.

Store references to execution artifacts where appropriate.

---

# Rust Module Layout

```text
crates/
└── execution/
    ├── src/
    │   ├── lib.rs
    │   ├── goal.rs
    │   ├── plan.rs
    │   ├── step.rs
    │   │
    │   ├── planner/
    │   │   ├── mod.rs
    │   │   ├── goal_parser.rs
    │   │   ├── generator.rs
    │   │   └── replan.rs
    │   │
    │   ├── graph/
    │   │   ├── mod.rs
    │   │   └── dependency.rs
    │   │
    │   ├── scheduler/
    │   │   ├── mod.rs
    │   │   └── locks.rs
    │   │
    │   ├── executor/
    │   │   ├── mod.rs
    │   │   ├── tool.rs
    │   │   ├── workflow.rs
    │   │   └── reasoning.rs
    │   │
    │   ├── verification/
    │   │   ├── mod.rs
    │   │   └── strategies.rs
    │   │
    │   ├── retry/
    │   │   └── policy.rs
    │   │
    │   ├── checkpoint/
    │   │   └── manager.rs
    │   │
    │   ├── budget/
    │   │   └── manager.rs
    │   │
    │   ├── persistence/
    │   │   ├── store.rs
    │   │   └── sqlite.rs
    │   │
    │   └── events/
    │       └── bus.rs
    │
    └── migrations/
```

---

# Recommended Implementation Sequence

Build it incrementally.

```text
P1  Goal parsing
 ↓
P2  Structured planning
 ↓
P3  Dependency graph
 ↓
P4  Sequential execution
 ↓
P5  Verification
 ↓
P6  Retry handling
 ↓
P7  Replanning
 ↓
P8  Persistence/resume
 ↓
P9  Checkpoints
 ↓
P10 Memory/skill-aware planning
 ↓
P11 Budgets
 ↓
P12 Parallel execution
 ↓
P13 Subagents
 ↓
P14 Approval gates
 ↓
P15 Idempotency
 ↓
P16 Event system
 ↓
P17 Goal outcome evaluation
 ↓
P18–20 Memory/Skills/Evolution integration
```

---

# Practical Development Phases

## Phase 1 — Functional Executor

Implement:

```text
P1–P5
```

Capabilities:

- understand goal,
- create structured plan,
- determine dependencies,
- execute sequentially,
- verify results.

This should be the first usable milestone.

---

## Phase 2 — Resilient Execution

Implement:

```text
P6–P9
```

Capabilities:

- retries,
- failure classification,
- targeted replanning,
- persistence,
- restart recovery,
- checkpoints.

At this point the agent can handle substantial real work.

---

## Phase 3 — Intelligent Execution

Implement:

```text
P10–P13
```

Capabilities:

- memory-aware planning,
- skill-aware planning,
- execution budgets,
- parallel work,
- subagent delegation.

---

## Phase 4 — Production Controls

Implement:

```text
P14–P17
```

Capabilities:

- approval boundaries,
- idempotency,
- event auditing,
- goal-level verification.

---

## Phase 5 — Learning Loop

Implement:

```text
P18–P20
```

Complete the feedback loop:

```text
Plan
 ↓
Execute
 ↓
Verify
 ↓
Observe
 ↓
Memory
 ↓
Skills
 ↓
Evolution
 ↓
Better future planning
```

---

# Testing Strategy

## Unit Tests

Test:

- dependency resolution,
- state transitions,
- retry rules,
- budgets,
- locking,
- plan validation,
- persistence,
- idempotency.

## Scenario Tests

Example:

```text
Goal:
Deploy service.

Step 1 succeeds.
Step 2 fails transiently.
Retry succeeds.
Step 3 verification fails.
Planner generates corrective step.
Corrective step succeeds.
Final verification succeeds.
```

Expected result:

```text
Goal = Completed
Plan version = 2
Retry count = 1
Replan count = 1
```

## Crash Recovery Tests

Terminate the runtime during:

```text
tool execution
verification
replanning
checkpoint creation
```

Restart and ensure no unsafe action is duplicated.

## Negative Tests

Test:

```text
circular dependencies
invalid tools
missing permissions
budget exhaustion
repeated failure
unsafe retry
corrupt persisted state
```

---

# Core Design Rules

1. **Plans are structured state, not prose.**
2. **Goals define success; plans define how to reach it.**
3. **Execution success is not verification success.**
4. **Never replan work that already succeeded unless necessary.**
5. **Retries and replanning are separate mechanisms.**
6. **Persist before and after external mutations.**
7. **Destructive operations require stronger idempotency and approval rules.**
8. **Subagents never implicitly inherit every capability.**
9. **Budgets are runtime-enforced, not prompt suggestions.**
10. **The runtime controls execution; the LLM proposes actions.**
11. **Every material action should leave evidence.**
12. **A completed plan does not imply a completed goal.**

---

# Final Target Architecture

```text
                         USER
                           │
                           ▼
                         GOAL
                           │
                           ▼
                   CONTEXT BUILDER
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
           MEMORY        SKILLS       STATE
              │            │            │
              └────────────┼────────────┘
                           ▼
                        PLANNER
                           │
                           ▼
                       TASK GRAPH
                           │
                           ▼
                       SCHEDULER
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
            TOOL        WORKFLOW     SUBAGENT
              │            │            │
              └────────────┼────────────┘
                           ▼
                        VERIFIER
                           │
                     ┌─────┴─────┐
                     ▼           ▼
                  SUCCESS      FAILURE
                     │           │
                     │      Retry/Replan
                     │           │
                     └─────┬─────┘
                           ▼
                    GOAL EVALUATION
                           │
            ┌──────────────┼──────────────┐
            ▼              ▼              ▼
          MEMORY          SKILLS       EVOLUTION
```

The milestone I would optimize for first is simple: **the agent can take a goal, generate a structured plan, execute each step, verify the result, and recover from one failed step without throwing away successful work.** Everything else should build on that execution contract.
