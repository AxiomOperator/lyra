# Rust Agent Skills System — V1–V7 Implementation Plan

## Objective

Build a self-learning skills subsystem for a Rust-based AI agent that can:

- capture reusable procedures,
- retrieve relevant skills,
- update existing skills instead of creating duplicates,
- track whether skills actually help,
- automatically detect worthwhile learning opportunities,
- score skill reliability,
- promote or reject skills based on evidence,
- and eventually maintain the entire skill collection autonomously.

The system should remain separate from ordinary memory.

```text
Memory
└── facts, context, preferences, observations

Skills
└── reusable procedures, workflows, recovery methods, learned techniques
```

---

# Core Architecture

```text
                    Agent Runtime
                         │
          ┌──────────────┴──────────────┐
          │                             │
    MemoryManager                 SkillManager
                                        │
                    ┌───────────────────┼───────────────────┐
                    │                   │                   │
                 Search              Learning            Usage
                    │                Evaluator           Tracker
                    │                   │                   │
                    └──────────┬────────┴──────────┬────────┘
                               │                   │
                               ▼                   ▼
                          Skill Store         Skill History
                               │
                               ▼
                             SQLite
```

Initially, keep all skill data in SQLite.

---

# Core Skill Model

```rust
pub struct Skill {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub instructions: String,

    pub status: SkillStatus,

    pub confidence: f32,

    pub use_count: u64,
    pub success_count: u64,
    pub failure_count: u64,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,

    pub source: String,
}
```

Status:

```rust
pub enum SkillStatus {
    Proposed,
    Active,
    Rejected,
    Deprecated,
}
```

Public API:

```text
skill.learn
skill.search
skill.get
skill.update
skill.forget
skill.approve
skill.reject
```

---

# V1 — Basic Skill Storage and Retrieval

## Goal

Allow the agent to save a reusable skill and retrieve it during future work.

## Features

- create skill
- search skills
- list skills
- retrieve by ID
- delete skill
- proposed/active status
- SQLite persistence

## Skill Store

```rust
#[async_trait]
pub trait SkillStore {
    async fn create(&self, skill: Skill) -> Result<Skill>;
    async fn get(&self, id: Uuid) -> Result<Option<Skill>>;
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Skill>>;
    async fn list(&self, status: Option<SkillStatus>) -> Result<Vec<Skill>>;
    async fn delete(&self, id: Uuid) -> Result<()>;
}
```

## Search

Use SQLite FTS5.

Search:

- name
- description
- instructions

Do not introduce embeddings yet.

## Learning Flow

```text
Agent discovers reusable procedure
        │
        ▼
skill.learn
        │
        ▼
Proposed Skill
        │
        ▼
Operator Approval
        │
        ▼
Active Skill
```

## Completion Criteria

V1 is complete when:

- skills persist across restarts,
- the agent can search them,
- active skills can be injected into agent context,
- proposed skills can be approved or rejected.

---

# V2 — Skill Matching and Refinement

## Goal

Prevent duplicate skills.

Before creating a skill, search for an existing skill describing the same procedure.

## Learning Decision

```rust
pub enum LearningAction {
    Ignore,
    Create,
    Update,
}
```

```rust
pub struct LearningDecision {
    pub action: LearningAction,
    pub skill_id: Option<Uuid>,
    pub confidence: f32,
    pub reason: String,
    pub proposed_content: Option<String>,
}
```

## Flow

```text
New Learning Candidate
        │
        ▼
 Search Existing Skills
        │
        ▼
 Learning Evaluator
        │
 ┌──────┼──────┐
 ▼      ▼      ▼
Ignore Create Update
```

## Update Rules

An update should:

- preserve useful existing instructions,
- modify only relevant sections,
- avoid replacing a broader skill with a narrower lesson,
- avoid duplicate procedure steps.

## Version History

Introduce:

```text
skill_versions
```

Fields:

```text
id
skill_id
version
instructions
description
change_reason
created_at
```

Every update creates a version snapshot first.

## Completion Criteria

V2 is complete when:

- repeated lessons refine existing skills,
- duplicate skills are uncommon,
- every update has rollback history.

---

# V3 — Usage and Outcome Tracking

## Goal

Determine whether learned skills are actually useful.

Every time a skill is included in agent execution, record its usage.

## New Structure

```rust
pub struct SkillUsage {
    pub id: Uuid,
    pub skill_id: Uuid,
    pub run_id: Uuid,
    pub used_at: DateTime<Utc>,
    pub outcome: SkillOutcome,
}
```

```rust
pub enum SkillOutcome {
    Unknown,
    Success,
    Failure,
    Partial,
}
```

## Metrics

Maintain:

```text
use_count
success_count
failure_count
success_rate
last_used_at
```

Success rate:

```text
success_count / completed_uses
```

Do not immediately infer failure just because a task failed. A task may fail for unrelated reasons.

## Runtime Receipt

The agent runtime should record exactly which skills were injected or explicitly used during a run.

```text
Run
├── skill A
├── skill C
└── skill F
```

This becomes evidence for later learning.

## Completion Criteria

V3 is complete when:

- every used skill can be traced to a run,
- outcomes are recorded,
- the system can identify frequently successful and frequently unsuccessful skills.

---

# V4 — Automatic Learning Candidate Detection

## Goal

Stop requiring the agent or user to explicitly call `skill.learn`.

Introduce a post-task learning evaluator.

## Candidate Sources

Evaluate learning when:

- the user corrected the agent,
- an initial method failed and a replacement succeeded,
- a non-obvious multi-step procedure succeeded,
- the agent discovered a repeatable recovery method,
- a skill used during the task was incomplete,
- the same problem required several rounds of discovery.

Do not learn from:

- ordinary successful tasks,
- temporary service failures,
- secrets,
- credentials,
- random conversation,
- unsupported assumptions,
- one-off facts.

## Flow

```text
Task Completed
      │
      ▼
Evidence Collector
      │
      ▼
Learning Evaluator
      │
      ▼
Should Learn?
   │       │
  No      Yes
           │
           ▼
     Candidate Skill
           │
           ▼
      V2 Matching
```

## Evidence

The evaluator should receive:

```text
user request
relevant conversation
tools called
tool results
skills used
corrections
task outcome
```

It should not receive unlimited historical context.

## Completion Criteria

V4 is complete when:

- useful candidates are generated without explicit user commands,
- routine tasks rarely create skills,
- candidates still default to `Proposed`.

---

# V5 — Confidence and Reliability Scoring

## Goal

Stop treating all skills as equally trustworthy.

Introduce separate confidence concepts.

```rust
pub struct SkillScore {
    pub learned_confidence: f32,
    pub observed_reliability: f32,
    pub usage_score: f32,
    pub freshness_score: f32,
    pub final_score: f32,
}
```

### Learned Confidence

How confident the evaluator was when creating the skill.

### Observed Reliability

Based on real outcomes.

Example:

```text
successful uses: 18
failed uses:      2
reliability:      0.90
```

Use smoothing so new skills do not immediately receive 100%.

For example:

```text
(successes + 1) / (successes + failures + 2)
```

### Freshness

Older procedures can slowly lose priority without being deleted.

### Final Score

Example:

```text
final_score =
    relevance   * 0.50
  + reliability * 0.25
  + confidence  * 0.15
  + freshness   * 0.10
```

Do not hard-code these permanently. Make them configurable.

## Retrieval

Skill retrieval now becomes:

```text
search relevance
      +
skill reliability
      +
confidence
      +
freshness
```

## Completion Criteria

V5 is complete when:

- reliable skills rank higher,
- newly created skills are treated cautiously,
- frequently failing skills lose priority.

---

# V6 — Automatic Promotion, Rejection, and Deprecation

## Goal

Allow evidence to affect skill lifecycle automatically.

Introduce lifecycle transitions.

```text
Proposed
   │
   ├──► Active
   │
   └──► Rejected

Active
   │
   ├──► Deprecated
   │
   └──► Active revision
```

## Example Promotion Policy

Automatically promote a proposed skill when:

```text
confidence >= 0.85
AND
validated by >= 2 successful uses
AND
0 observed failures
```

These thresholds should be configurable.

## Automatic Deprecation

Do not automatically delete skills.

A skill can move to `Deprecated` when:

- reliability falls below threshold,
- a newer skill supersedes it,
- repeated evidence contradicts it.

## Relationships

Add:

```rust
pub enum SkillRelationship {
    Supersedes,
    Extends,
    ConflictsWith,
    RelatedTo,
}
```

This prevents old knowledge from simply disappearing.

Example:

```text
skill-v2
   │
   └── supersedes ──► skill-v1
```

## Safety

Any automatic state transition must generate an audit entry.

```text
what changed
why
evidence
previous state
new state
timestamp
```

## Completion Criteria

V6 is complete when:

- trusted skills can become active automatically,
- weak skills are downgraded rather than deleted,
- all automated lifecycle changes are auditable and reversible.

---

# V7 — Autonomous Skill Maintenance

## Goal

Allow the system to periodically clean, consolidate, and improve its skill collection.

This is where true autonomous skill maintenance begins.

## Background Maintenance

Create a `SkillCurator`.

```text
Skill Store
    │
    ▼
Skill Curator
    │
    ├── duplicate detection
    ├── contradiction detection
    ├── stale-skill review
    ├── skill merging
    ├── skill splitting
    ├── reliability review
    └── cleanup
```

## Maintenance Tasks

### Duplicate Detection

Find skills covering substantially identical procedures.

```text
Skill A
Skill B
Skill C
   │
   ▼
Merge Candidate
```

### Contradiction Detection

Detect conflicting instructions.

```text
Skill A:
Always retry 5 times.

Skill B:
Never retry more than twice.
```

Flag for resolution instead of choosing silently.

### Skill Consolidation

Merge repeated lessons into stronger procedures.

### Skill Splitting

If a skill becomes too broad:

```text
Docker Deployment
```

may become:

```text
Docker Deployment
Docker Recovery
Docker Networking
Docker Backup
```

### Stale Skill Review

Review skills that:

- have not been used recently,
- have poor reliability,
- reference obsolete tools or APIs,
- have been superseded.

### Collection Health Metrics

Track:

```text
total skills
active skills
proposed skills
deprecated skills
duplicate candidates
conflicts
average reliability
skills never used
skills failing frequently
```

## Scheduling

Do not run maintenance continuously.

Use:

```text
manual
idle-time
daily
weekly
```

Weekly is a reasonable default once the system is mature.

## Autonomous Modes

By V7, support:

```rust
pub enum LearningMode {
    Off,
    Propose,
    Auto,
}
```

### Off

No autonomous learning.

### Propose

Candidates and maintenance changes require approval.

### Auto

Safe changes can apply automatically.

High-risk changes should still be reviewable.

## Rollback

By V7, every mutation should be reversible:

```text
create
update
merge
deprecate
promote
```

Each must retain:

```text
previous version
change reason
evidence
timestamp
initiating run
```

---

# Recommended Database Tables

By V7:

```text
skills
skill_versions
skill_usage
skill_relationships
skill_proposals
skill_events
```

### `skills`

Current canonical state.

### `skill_versions`

Immutable historical versions.

### `skill_usage`

Records which runs used each skill.

### `skill_relationships`

Tracks superseding, conflicting, and related skills.

### `skill_proposals`

Stores proposed creates and updates.

### `skill_events`

Audit log.

---

# Recommended Rust Module Layout

```text
crates/
└── skills/
    ├── src/
    │   ├── lib.rs
    │   ├── model.rs
    │   ├── manager.rs
    │   ├── store.rs
    │   ├── sqlite.rs
    │   │
    │   ├── search/
    │   │   ├── mod.rs
    │   │   └── fts.rs
    │   │
    │   ├── learning/
    │   │   ├── evaluator.rs
    │   │   ├── candidate.rs
    │   │   └── matcher.rs
    │   │
    │   ├── scoring/
    │   │   ├── confidence.rs
    │   │   └── reliability.rs
    │   │
    │   ├── lifecycle/
    │   │   ├── promotion.rs
    │   │   ├── rejection.rs
    │   │   └── deprecation.rs
    │   │
    │   ├── usage/
    │   │   └── tracker.rs
    │   │
    │   └── curator/
    │       ├── duplicates.rs
    │       ├── conflicts.rs
    │       ├── merge.rs
    │       └── maintenance.rs
    │
    └── migrations/
        ├── 001_skills.sql
        ├── 002_versions.sql
        ├── 003_usage.sql
        ├── 004_proposals.sql
        └── 005_relationships.sql
```

---

# Development Sequence

Implement each version as a usable milestone.

```text
V1
Save + retrieve skills
        │
        ▼
V2
Match + refine
        │
        ▼
V3
Track usage + outcomes
        │
        ▼
V4
Detect learning automatically
        │
        ▼
V5
Score reliability
        │
        ▼
V6
Manage lifecycle automatically
        │
        ▼
V7
Maintain the collection autonomously
```

Do not begin the next version until the prior one produces trustworthy data.

---

# Testing Strategy

Every version should include:

## Unit Tests

Test:

- CRUD behavior
- matching
- scoring
- lifecycle transitions
- version creation
- rollback

## Scenario Tests

Create realistic agent runs:

```text
incorrect procedure
      ↓
user correction
      ↓
successful retry
      ↓
learning candidate
      ↓
skill created
      ↓
future run retrieves skill
      ↓
successful execution
```

## Negative Tests

Ensure the system does not learn from:

- failed commands,
- credentials,
- hallucinations,
- temporary outages,
- irrelevant conversation.

## Regression Tests

Once a learned skill fixes a recurring failure, preserve that interaction as a test case.

This creates an important feedback loop:

```text
Agent failure
     ↓
Correction
     ↓
Skill
     ↓
Regression test
```

---

# Key Design Rules

1. **Never confuse memory with skills.**
2. **Never silently overwrite a skill without version history.**
3. **New evidence should refine existing skills whenever possible.**
4. **Observed outcomes matter more than model confidence.**
5. **Failed tasks are not automatically failed skills.**
6. **Do not delete weak skills; deprecate them.**
7. **Every autonomous change must be auditable.**
8. **Use proposal mode before enabling automatic learning.**
9. **Avoid embeddings until FTS-based retrieval becomes a measurable limitation.**
10. **The model proposes learning; the runtime owns policy and persistence.**

---

# Final Target

By V7, the runtime should behave like this:

```text
                    USER TASK
                        │
                        ▼
                 Search Skills
                        │
                        ▼
                     Agent
                        │
                  Execute Task
                        │
                        ▼
                 Record Outcome
                        │
                        ▼
              Learning Evaluator
                        │
             ┌──────────┼───────────┐
             │          │           │
           Ignore     Create      Refine
                         │           │
                         └─────┬─────┘
                               ▼
                         Skill Store
                               │
                               ▼
                       Reliability Data
                               │
                               ▼
                         Skill Curator
                               │
              ┌────────────────┼────────────────┐
              ▼                ▼                ▼
            Merge           Deprecate        Promote
```

At that point the agent is not simply storing notes. It is gradually building, testing, refining, and maintaining its own library of reusable procedures based on actual execution evidence.
