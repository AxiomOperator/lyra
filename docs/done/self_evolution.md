The key distinction from the skills system is:

```text
Memory     = what the agent knows
Skills     = how the agent does things
Evolution  = how the agent changes itself
```

A good architecture is to make evolution progressive instead of giving the model unrestricted access to rewrite itself.

## Self-Evolution Architecture

```text
                         Agent Runtime
                              │
                              ▼
                       Observe Performance
                              │
                              ▼
                     Evolution Evaluator
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
          Prompt Change   Workflow Change   Tool Change
              │               │               │
              └───────────────┼───────────────┘
                              ▼
                      Change Proposal
                              │
                              ▼
                       Validation Lab
                              │
                ┌─────────────┼─────────────┐
                ▼             ▼             ▼
              Tests        Security      Benchmark
                │             │             │
                └─────────────┼─────────────┘
                              ▼
                         Promotion
                              │
                      ┌───────┴───────┐
                      ▼               ▼
                   Reject           Deploy
                                        │
                                        ▼
                                   Monitor
                                        │
                                        ▼
                                    Rollback
```

I would implement it in **seven stages**, similar to the skills roadmap.

---

# E1 — Self-Observation

Before the agent can improve itself, it needs structured telemetry about its own behavior.

Record each run:

```rust
pub struct RunRecord {
    pub id: Uuid,
    pub task: String,

    pub model_calls: u32,
    pub tool_calls: u32,
    pub retries: u32,

    pub duration_ms: u64,

    pub outcome: RunOutcome,

    pub skills_used: Vec<Uuid>,
    pub tools_used: Vec<String>,
    pub errors: Vec<String>,
}
```

Track things such as:

```text
success
failure
partial success

tool calls
model iterations
retries
token consumption
latency
corrections
skills used
errors encountered
```

This creates the evidence required for evolution.

Without this, self-improvement becomes guesswork.

---

# E2 — Improvement Detection

Add an `EvolutionEvaluator`.

Its job is to identify:

> Could this task have been completed better?

Examples:

```text
12 tool calls needed
↓
probably inefficient workflow
```

```text
same error encountered 4 times
↓
missing recovery behavior
```

```text
user repeatedly corrects response style
↓
prompt/configuration improvement
```

```text
tool frequently requires multiple calls
↓
better tool could be created
```

Possible output:

```rust
pub struct EvolutionCandidate {
    pub category: EvolutionCategory,
    pub problem: String,
    pub proposed_change: String,
    pub evidence: Vec<Uuid>,
    pub confidence: f32,
}
```

Categories:

```rust
pub enum EvolutionCategory {
    Prompt,
    Workflow,
    Skill,
    Tool,
    Configuration,
    Code,
}
```

Initially, only allow:

```text
Prompt
Workflow
Skill
Configuration
```

Do not allow autonomous code modification yet.

---

# E3 — Prompt and Behavior Evolution

This is the safest form of actual self-evolution.

Allow the agent to propose modifications to its behavioral configuration.

For example:

```text
Current behavior:
Search skills after planning.

Observed:
Planning repeatedly duplicates procedures already stored as skills.

Proposed:
Search skills before planning.
```

Instead of rewriting one giant system prompt, use structured configuration.

```toml
[behavior]
search_skills_before_planning = true
max_tool_retries = 3
verify_destructive_actions = true
```

The agent proposes:

```text
search_skills_before_planning:
false → true
```

Not:

```text
rewrite entire system prompt
```

That makes changes understandable and reversible.

---

# E4 — Workflow Evolution

Now allow the agent to modify execution workflows.

For example:

```text
OLD

Task
 ↓
Plan
 ↓
Execute
 ↓
Verify
```

The agent discovers that infrastructure changes work better with:

```text
NEW

Task
 ↓
Gather State
 ↓
Search Skills
 ↓
Plan
 ↓
Preflight
 ↓
Execute
 ↓
Verify
 ↓
Record Outcome
```

Represent workflows as data instead of hardcoded Rust logic.

Example:

```yaml
name: infrastructure-change

steps:
  - inspect_environment
  - search_skills
  - create_plan
  - preflight
  - execute
  - verify
  - record_result
```

Then evolution can safely modify workflow definitions.

This is substantially safer than modifying Rust code.

---

# E5 — Tool Evolution

This is where things become much more interesting.

The agent detects repeated inefficient patterns.

Example:

```text
docker.ps
docker.inspect
docker.logs
docker.inspect
docker.stats
```

Every time it diagnoses a container.

The evaluator concludes:

```text
These five calls should become one diagnostic tool.
```

It proposes:

```text
docker.diagnose
```

with:

```text
Input:
container_id

Output:
status
health
recent_logs
resources
network
mounts
```

The agent has effectively created a **new capability**.

Initially, I would generate tools from declarative compositions:

```yaml
tool: docker.diagnose

steps:
  - docker.inspect
  - docker.stats
  - docker.logs

output:
  combine: true
```

That allows tool creation without generating arbitrary executable code.

---

# E6 — Controlled Code Evolution

Only after the prior layers work reliably would I permit code changes.

The agent should never modify the currently running binary directly.

Use:

```text
Current Runtime
      │
      ▼
Evolution Candidate
      │
      ▼
Generate Patch
      │
      ▼
Sandbox Repository
      │
      ▼
cargo fmt
cargo clippy
cargo test
security checks
integration tests
benchmarks
      │
      ▼
Candidate Build
```

Then:

```text
candidate
   │
   ├── worse → reject
   │
   └── better → eligible for promotion
```

The agent should produce **patches**, not overwrite source files blindly.

Example:

```rust
pub struct CodeEvolution {
    pub base_commit: String,
    pub diff: String,
    pub reason: String,
    pub evidence: Vec<Uuid>,
}
```

Every change should be tied to a known source revision.

---

# E7 — Evolution Selection

This is the point where the system starts to resemble actual evolutionary optimization.

Instead of creating one candidate, produce several.

```text
Problem:
Agent uses too many calls for dependency debugging.
```

Generate:

```text
Candidate A
Modify workflow.

Candidate B
Create diagnostic tool.

Candidate C
Improve skill retrieval.

Candidate D
Modify planning prompt.
```

Run the same benchmark suite against each:

```text
              Baseline
                 │
       ┌─────────┼─────────┐
       ▼         ▼         ▼
       A         B         C
       │         │         │
       ▼         ▼         ▼
     tests     tests     tests
       │         │         │
       └──────┬──┴──────┬──┘
              ▼
           scoring
              │
              ▼
        best candidate
```

Score candidates using:

```text
success rate
tool calls
model calls
latency
token use
error rate
user corrections
security violations
```

Example fitness function:

```text
fitness =
    success_rate      * 0.40
  + accuracy          * 0.25
  + efficiency        * 0.15
  + reliability       * 0.10
  + safety            * 0.10
```

Now changes are selected based on evidence instead of model preference.

---

# Evolution Levels

I would explicitly categorize how dangerous a mutation is.

```rust
pub enum EvolutionLevel {
    Skill,
    Behavior,
    Workflow,
    Tool,
    Configuration,
    Code,
    Architecture,
}
```

And assign approval requirements.

| Level | Example | Initial policy |
|---|---|---|
| Skill | Better recovery procedure | Auto eventually |
| Behavior | Search before planning | Auto eventually |
| Workflow | Add preflight step | Propose |
| Tool | Compose new diagnostic tool | Propose |
| Config | Change retry limits | Propose |
| Code | Modify Rust implementation | Manual approval |
| Architecture | Replace storage engine | Manual only |

I would **never** let architecture-level changes automatically deploy.

---

# Evolution History

This needs to be immutable.

```text
evolution_events

id
candidate_id
category
description
evidence
old_version
new_version
fitness_before
fitness_after
status
created_at
```

Status:

```text
proposed
testing
rejected
approved
deployed
rolled_back
```

That gives you:

```text
Agent v1.43
   │
   ├── skill update
   ├── workflow update
   └── prompt update
         │
         ▼
Agent v1.44
```

Not necessarily binary versions—more like **behavioral generations**.

---

# Generation Model

I would actually introduce a concept of an agent generation.

```rust
pub struct AgentGeneration {
    pub id: Uuid,
    pub parent: Option<Uuid>,

    pub prompt_version: String,
    pub workflow_version: String,
    pub skill_snapshot: String,
    pub tool_snapshot: String,
    pub config_snapshot: String,

    pub created_at: DateTime<Utc>,
}
```

Then:

```text
Generation 1
      │
      ▼
Generation 2
      │
      ├───────────┐
      ▼           ▼
Generation 3A   Generation 3B
      │           │
      └─────┬─────┘
            ▼
         Evaluate
            │
            ▼
Generation 4
```

Now you've created genuine versioned agent evolution.

---

# The Most Important Boundary

The agent should have three identities:

```text
RUNNER
Does the work.

REVIEWER
Analyzes how the work went.

EVOLVER
Proposes improvements.
```

I would not let the same execution loop do all three.

```text
Runner
  ↓
Run Evidence
  ↓
Reviewer
  ↓
Improvement Opportunity
  ↓
Evolver
  ↓
Candidate
  ↓
Validator
```

This separation dramatically reduces the chance that:

```text
"I made a mistake"
```

turns immediately into:

```text
"I'll rewrite myself."
```

---

# Relationship to Your Skills System

The systems fit together nicely:

```text
                    Agent
                      │
        ┌─────────────┼─────────────┐
        ▼             ▼             ▼
      Memory        Skills       Evolution
        │             │             │
    remembers      learns       improves
      facts       procedures      itself
```

And evolution should consume your skills evidence:

```text
Skills
  │
  ├── usage
  ├── failures
  ├── success rate
  └── corrections
       │
       ▼
Evolution Engine
```

For example:

```text
Skill keeps failing
       │
       ▼
Is skill wrong?
Is retrieval wrong?
Is workflow wrong?
Is tool inadequate?
       │
       ▼
Propose appropriate change
```

That is much more powerful than simply editing the skill.

---

# Recommended Build Sequence

I would build self-evolution in this order:

```text
E1  Run telemetry
 ↓
E2  Detect improvement opportunities
 ↓
E3  Prompt/config behavior proposals
 ↓
E4  Workflow mutation
 ↓
E5  Declarative tool creation
 ↓
E6  Sandboxed source-code patches
 ↓
E7  Competing candidates + fitness selection
```
