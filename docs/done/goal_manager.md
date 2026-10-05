
What is missing is a durable layer that answers:

> What am I trying to accomplish over time, what should I work on next, and when should I resume?

That is what turns the system from a reactive assistant into an actual autonomous agent.

## Add a Goal Manager

```text
User / System
     │
     ▼
 GoalManager
     │
     ├── active goals
     ├── priorities
     ├── deadlines
     ├── dependencies
     ├── progress
     └── success criteria
            │
            ▼
          Planner
            │
            ▼
         Executor
```

A goal should be a first-class object:

```rust
pub struct Goal {
    pub id: Uuid,
    pub title: String,
    pub description: String,

    pub status: GoalStatus,
    pub priority: u8,

    pub parent_goal_id: Option<Uuid>,
    pub dependencies: Vec<Uuid>,

    pub success_criteria: Vec<String>,

    pub progress: f32,

    pub created_at: DateTime<Utc>,
    pub due_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}
```

Statuses:

```rust
pub enum GoalStatus {
    Proposed,
    Active,
    Blocked,
    Paused,
    Completed,
    Failed,
    Cancelled,
}
```

## Then add goal decomposition

Example:

```text
Goal:
Integrate PMI into the agent
```

The system could break that into:

```text
Integrate PMI
├── Parse OpenAPI
├── Build capability registry
├── Add authentication
├── Add tool discovery
├── Add planner integration
├── Add verification
└── Test end-to-end
```

This is different from a one-time execution plan.

A **goal** can live for days or weeks.

A **plan** is one attempt or work session toward that goal.

That distinction is important:

```text
Goal
  ├── Plan v1
  ├── Plan v2
  └── Plan v3
```

## Add an autonomy loop

Eventually:

```text
Observe State
     │
     ▼
Check Active Goals
     │
     ▼
Select Highest Priority Goal
     │
     ▼
Can Progress?
   │       │
  No      Yes
   │       │
 Block    Plan
           │
           ▼
        Execute
           │
           ▼
        Verify
           │
           ▼
     Update Progress
           │
           ▼
      Continue / Stop
```

The runtime should decide when to stop. Do not let the LLM run indefinitely.

## Add goal priority scoring

Something like:

```text
priority_score =
    explicit_priority
  + deadline_urgency
  + dependency_value
  + user_importance
  + progress_opportunity
  - estimated_cost
```

For example:

```text
Goal A
priority 8
deadline tomorrow
blocks 3 other goals

Goal B
priority 10
no deadline
blocks nothing
```

Goal A may deserve work first.

## Add blockers

This is critical for autonomy.

```rust
pub struct GoalBlocker {
    pub goal_id: Uuid,
    pub reason: String,
    pub blocker_type: BlockerType,
    pub created_at: DateTime<Utc>,
}
```

Types:

```rust
pub enum BlockerType {
    MissingInformation,
    MissingPermission,
    ExternalDependency,
    FailedDependency,
    ApprovalRequired,
    CapabilityUnavailable,
}
```

The agent should not repeatedly retry blocked goals.

Instead:

```text
Goal
 ↓
blocked
 ↓
record reason
 ↓
wait for state change
```

## Add persistent progress

Track more than a percentage.

```rust
pub struct GoalProgress {
    pub completed_items: u32,
    pub total_items: Option<u32>,
    pub summary: String,
    pub updated_at: DateTime<Utc>,
}
```

That lets the agent answer:

```text
"How far are we on the PMI integration?"
```

without reconstructing everything from conversation history.

## Add goal events

```rust
pub enum GoalEvent {
    Created,
    Activated,
    PlanStarted,
    ProgressUpdated,
    Blocked,
    Unblocked,
    Paused,
    Completed,
    Failed,
}
```

These events can feed Memory and Evolution.

## Then add scheduling

Once goals exist, add a scheduler:

```text
Scheduler
   │
   ├── at a specific time
   ├── periodically
   ├── after another goal
   └── when a condition changes
```

Example:

```text
Goal:
Finish PMI integration

Blocked because:
API token unavailable

Trigger:
credentials become available

→ resume goal
```

That gives you real event-driven autonomy.

## Add autonomy policies

This should be strongly runtime-controlled.

```rust
pub enum AutonomyMode {
    Reactive,
    Assisted,
    Autonomous,
}
```

### Reactive
Only works when explicitly asked.

### Assisted
May continue existing goals, but asks before meaningful writes.

### Autonomous
May select and execute work within policy limits.

Then separately define:

```text
max continuous runtime
max tool calls
max model calls
max cost
max replans
allowed risk levels
```

## I would build it in this order

```text
G1  Goal model
G2  Goal persistence
G3  Goal decomposition
G4  Goal ↔ plan relationship
G5  Progress tracking
G6  Blockers/dependencies
G7  Priority selection
G8  Resume logic
G9  Scheduler
G10 Event triggers
G11 Autonomy policy
G12 Goal review/consolidation
```

After that, the next subsystem should be **event-driven scheduling and triggers**, then **multi-agent coordination**.

At that point your agent starts to look like this:

```text
                    Identity
                       │
                       ▼
                     Goals
                       │
                       ▼
                    Planner
                       │
                       ▼
              Capability Manager
                       │
                       ▼
                    Executor
                       │
                       ▼
                    Verifier
                       │
          ┌────────────┼────────────┐
          ▼            ▼            ▼
        Memory       Skills      Evolution
          │            │            │
          └────────────┼────────────┘
                       ▼
                 Goal Progress
                       │
                       ▼
                 Scheduler/Event Loop
```
