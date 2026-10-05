
> **What can I do right now, which tool should I use, and am I allowed to use it?**

That becomes increasingly important now that the agent may have hundreds of OpenAPI operations plus native tools, MCP tools, workflows, subagents, and generated tools.

# Next Feature: Capability Intelligence

I would introduce a dedicated:

```text
CapabilityManager
```

Architecture:

```text
                     Agent
                       │
                       ▼
                    Planner
                       │
                       ▼
               CapabilityManager
                       │
      ┌────────────────┼────────────────┐
      ▼                ▼                ▼
 Native Tools      OpenAPI Tools      MCP Tools
      │                │                │
      └────────────────┼────────────────┘
                       ▼
                 Capability Index
                       │
          ┌────────────┼────────────┐
          ▼            ▼            ▼
       Search        Policy       History
```

## 1. Unified Capability Model

Every action the agent can perform should become a `Capability`.

```rust
pub struct Capability {
    pub id: String,
    pub name: String,
    pub description: String,

    pub kind: CapabilityKind,

    pub input_schema: serde_json::Value,
    pub output_schema: Option<serde_json::Value>,

    pub risk: RiskLevel,
    pub permissions: Vec<String>,

    pub tags: Vec<String>,

    pub enabled: bool,
}
```

Kinds:

```rust
pub enum CapabilityKind {
    NativeTool,
    OpenApi,
    Mcp,
    Workflow,
    Skill,
    Subagent,
}
```

Now the planner no longer cares whether something came from:

```text
Rust
OpenAPI
MCP
WASM
an internal workflow
```

It just asks:

```text
"Find capabilities that can create a project task."
```

---

# 2. Capability Discovery

Do not expose all tools to the model.

Instead:

```text
User Goal
   │
   ▼
Capability Search
   │
   ▼
Top relevant capabilities
   │
   ▼
Planner
```

Example:

```text
Goal:
Create a task and assign it to Garrett.
```

Discovery returns:

```text
pmi.search
pmi.tasks.create
pmi.tasks.assignees.set
```

instead of feeding 277 PMI operations into context.

Start with FTS.

Later use LanceDB embeddings:

```text
Capability descriptions
        │
        ▼
      LanceDB
        │
        ▼
 Semantic Tool Search
```

That gives your new LanceDB integration another valuable use.

---

# 3. Capability Metadata

Add operational information:

```rust
pub struct CapabilityMetadata {
    pub destructive: bool,
    pub idempotent: bool,
    pub requires_approval: bool,

    pub average_latency_ms: Option<u64>,
    pub success_rate: Option<f32>,

    pub cost: Option<f32>,

    pub verification_capability: Option<String>,
}
```

Now the planner can make smarter choices.

For example:

```text
Capability A
success rate: 98%
latency: 120ms

Capability B
success rate: 72%
latency: 4 seconds
```

Prefer A.

---

# 4. Tool Outcome Tracking

Track every capability invocation.

```rust
pub struct CapabilityUsage {
    pub capability_id: String,
    pub run_id: Uuid,

    pub success: bool,

    pub duration_ms: u64,

    pub retries: u32,

    pub error_code: Option<String>,
}
```

This starts creating actual intelligence around tools.

Eventually the agent learns:

```text
For this task:
Tool A usually works.

Tool B often fails with permissions.

Workflow C is more efficient.
```

---

# 5. Tool Selection Scoring

Once you have usage history:

```text
score =
    relevance
  + reliability
  + permission suitability
  + efficiency
  + historical success
```

Example:

```text
Tool selection score:

semantic relevance    0.92
success rate          0.98
permission fit        1.00
latency score         0.87
─────────────────────────
final                 0.95
```

This is far better than letting the LLM pick tools from names alone.

---

# 6. Capability Composition

This is where it becomes especially powerful.

Suppose the agent repeatedly performs:

```text
pmi.search
pmi.projects.get
pmi.tasks.create
pmi.tasks.assignees.set
```

The system can eventually create:

```text
workflow.create_and_assign_task
```

Then:

```text
Raw Tools
    │
    ▼
Repeated Pattern
    │
    ▼
Skill
    │
    ▼
Workflow Capability
```

Your Skills system learns **how** tools should be combined.

Your Capability system exposes that learned procedure as something reusable.

---

# 7. Capability Dependencies

Some tools need prerequisites.

Example:

```text
pmi.tasks.create
```

requires:

```text
projectId
```

So metadata could say:

```rust
pub struct CapabilityRequirement {
    pub entity: String,
    pub resolution_capability: Option<String>,
}
```

Then the planner understands:

```text
Need projectId
   │
   ▼
pmi.search
   │
   ▼
resolve project
   │
   ▼
pmi.tasks.create
```

This reduces unnecessary LLM reasoning.

---

# 8. Permissions and Policy

Make permissions first-class.

```rust
pub enum RiskLevel {
    ReadOnly,
    LowWrite,
    Write,
    Destructive,
    Privileged,
}
```

Policy example:

```toml
[capabilities.policy]
readonly = "auto"
low_write = "auto"
write = "auto"
destructive = "approval"
privileged = "deny"
```

That policy should be enforced by Rust, not prompting.

---

# 9. Capability Verification

Each mutating capability should optionally define verification.

Example:

```text
pmi.tasks.create
```

verification:

```text
pmi.tasks.get
```

Definition:

```rust
pub struct VerificationRule {
    pub capability: String,
    pub success_expression: String,
}
```

Execution becomes:

```text
Action
  │
  ▼
pmi.tasks.create
  │
  ▼
returned task ID
  │
  ▼
pmi.tasks.get
  │
  ▼
verify expected state
```

This fits directly into your Planning/Execution verifier.

---

# 10. Tool Health

Add capability availability checks.

Example:

```text
Capability          Status

pmi.tasks.create    healthy
browser.navigate    healthy
docker.inspect      unavailable
email.send          degraded
```

Then the planner avoids unavailable capabilities before it starts.

```rust
pub enum CapabilityHealth {
    Healthy,
    Degraded,
    Unavailable,
}
```

---

# 11. Dynamic Capability Loading

Eventually:

```text
Agent starts
   │
   ├── load native tools
   ├── parse OpenAPI
   ├── discover MCP servers
   ├── load workflows
   ├── load skill capabilities
   └── load WASM extensions
            │
            ▼
      Capability Registry
```

That gives you a plugin architecture almost automatically.

---

# 12. Suggested Development Phases

I would implement this as:

```text
C1
Unified Capability model

C2
Capability registry

C3
FTS capability discovery

C4
Risk + permissions policy

C5
Usage/outcome tracking

C6
Capability scoring

C7
Verification mappings

C8
Health/availability

C9
Semantic discovery with LanceDB

C10
Workflow composition

C11
Automatic capability generation

C12
Capability optimization through Evolution
```

# After Capability Intelligence

Then I would build **Goals + Autonomy**.

That would give the agent persistent objectives:

```text
Goal
  ↓
Observe
  ↓
Plan
  ↓
Discover capabilities
  ↓
Execute
  ↓
Verify
  ↓
Learn
  ↓
Remember
  ↓
Evolve
  ↓
Continue toward goal
```

At that point your architecture becomes:

```text
                   Agent Identity
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
          ┌─────────────┼─────────────┐
          ▼             ▼             ▼
       Memory         Skills       Evolution
```
