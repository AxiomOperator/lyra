# Rust Agent Memory System — Complete Implementation Plan

## Objective

Build a memory subsystem that allows the agent to:

- remember facts and context,
- recall relevant information later,
- distinguish short-term from long-term information,
- track where memories came from,
- update or supersede old memories,
- avoid duplicates and contradictions,
- rank memories by relevance and usefulness,
- consolidate repeated information,
- forget or expire information when appropriate,
- and provide only the most useful memories to the LLM.

Memory should remain distinct from skills and evolution.

```text
Memory
└── what the agent knows

Skills
└── how the agent performs tasks

Evolution
└── how the agent changes itself
```

---

# Core Architecture

```text
                       Agent Runtime
                            │
                  ┌─────────┴─────────┐
                  │                   │
             Memory Recall        Memory Capture
                  │                   │
                  ▼                   ▼
          Retrieval Engine      Memory Evaluator
                  │                   │
                  └─────────┬─────────┘
                            ▼
                      MemoryManager
                            │
         ┌──────────────────┼──────────────────┐
         │                  │                  │
         ▼                  ▼                  ▼
    Working Memory    Persistent Memory   Memory History
         │                  │
         ▼                  ▼
      Runtime             SQLite
       State         / PostgreSQL later
```

The agent itself should interact only with `MemoryManager`.

Storage technology should remain replaceable.

---

# Memory Types

Eventually support four primary classes.

## Working Memory

Temporary state for the current task or conversation.

Examples:

```text
current goal
current plan
recent tool outputs
temporary variables
active entities
unfinished tasks
```

Usually not persisted permanently.

---

## Semantic Memory

Durable facts.

Examples:

```text
The project uses Rust.

The agent's database is SQLite.

The API listens on port 8080.
```

---

## Episodic Memory

Summaries of things that happened.

Example:

```text
On October 4, the agent attempted provider configuration,
encountered an authentication error, corrected the token
configuration, and successfully connected.
```

---

## Procedural Memory

Do not use this for your primary skill system.

Procedures should normally live in **Skills**.

Procedural memory should only exist for lightweight behavior that is not substantial enough to become a skill.

---

# Core Memory Object

Start with a structure that can expand later.

```rust
pub struct Memory {
    pub id: Uuid,

    pub scope: String,
    pub kind: MemoryKind,

    pub content: String,

    pub source: MemorySource,

    pub importance: f32,
    pub confidence: f32,

    pub status: MemoryStatus,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_accessed_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,

    pub tags: Vec<String>,
}
```

Types:

```rust
pub enum MemoryKind {
    Working,
    Semantic,
    Episodic,
}
```

Status:

```rust
pub enum MemoryStatus {
    Active,
    Superseded,
    Archived,
    Deleted,
}
```

---

# Scope

Every memory should belong to a scope.

Examples:

```text
user
agent
project:arcella
conversation:abc123
workspace:default
```

Scopes prevent unrelated memories from contaminating retrieval.

Example:

```text
project:arcella
```

should not automatically return memories from:

```text
project:other-project
```

unless explicitly requested.

---

# Source and Provenance

Every persistent memory should record where it came from.

```rust
pub enum MemorySource {
    User,
    Conversation,
    Tool,
    Document,
    Agent,
    Derived,
}
```

Eventually expand this into structured provenance:

```rust
pub struct MemorySourceInfo {
    pub source_type: MemorySource,
    pub conversation_id: Option<Uuid>,
    pub message_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub tool_call_id: Option<Uuid>,
}
```

This allows the agent to answer:

```text
Why do I believe this?
```

---

# M1 — Basic Persistent Memory

## Goal

Implement simple memory that survives restarts.

Use SQLite.

Operations:

```text
remember
recall
get
list
forget
```

Rust trait:

```rust
#[async_trait]
pub trait MemoryStore {
    async fn create(&self, memory: Memory) -> Result<Memory>;

    async fn get(&self, id: Uuid) -> Result<Option<Memory>>;

    async fn search(
        &self,
        scope: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Memory>>;

    async fn list(
        &self,
        scope: &str,
        limit: usize,
    ) -> Result<Vec<Memory>>;

    async fn delete(&self, id: Uuid) -> Result<()>;
}
```

Use SQLite FTS5 for initial retrieval.

No embeddings yet.

### Completion Criteria

- memory survives restart,
- memory can be searched,
- scope isolation works,
- agent can explicitly remember and recall.

---

# M2 — Automatic Memory Capture

## Goal

Stop requiring explicit `memory.remember`.

After relevant interactions, run a memory evaluator.

Flow:

```text
Conversation / Tool Result
          │
          ▼
    Memory Evaluator
          │
     Should Remember?
       │        │
      No       Yes
                │
                ▼
         Memory Candidate
```

The evaluator should ask:

```text
Is this information durable?
Will it likely matter later?
Is it explicitly stated or inferred?
Is it already stored?
Is it sensitive?
```

Good candidates:

```text
project decisions
stable configuration
persistent user instructions
important entities
long-lived technical facts
important task outcomes
```

Do not automatically store:

```text
small talk
temporary states
credentials
API keys
transient errors
unsupported assumptions
random observations
```

---

# M3 — Deduplication and Updating

## Goal

Prevent the memory store from filling with repeated facts.

Before writing:

```text
Memory Candidate
       │
       ▼
Search Similar Memories
       │
       ▼
 ┌─────┼───────────────┐
 ▼     ▼               ▼
new   same         contradiction
 │     │               │
 ▼     ▼               ▼
save  merge         supersede
```

Introduce:

```rust
pub enum MemoryDecision {
    Ignore,
    Create,
    Update,
    Supersede,
}
```

Example:

Existing:

```text
Agent runtime language is Go.
```

New:

```text
Agent runtime language is Rust.
```

Do not delete the original.

Mark:

```text
Rust memory
    │
    └── supersedes
            │
            ▼
       Go memory
```

This preserves history.

---

# M4 — Memory Relationships

Introduce relationships between memories.

```rust
pub enum MemoryRelationship {
    Supports,
    Contradicts,
    Supersedes,
    DerivedFrom,
    RelatedTo,
}
```

Database:

```text
memory_relationships

id
from_memory_id
to_memory_id
relationship
created_at
```

Examples:

```text
Memory A ──supports────► Memory B

Memory C ──contradicts─► Memory D

Memory E ──supersedes──► Memory F
```

This creates a lightweight knowledge graph without requiring Neo4j.

---

# M5 — Hybrid Retrieval

Once FTS becomes limiting, introduce embeddings.

Do not replace lexical search.

Use both.

```text
Query
 │
 ├── FTS search
 │
 ├── vector search
 │
 └── metadata filtering
        │
        ▼
      Ranking
```

Possible ranking:

```text
score =
    semantic_similarity * 0.40
  + lexical_similarity  * 0.20
  + importance          * 0.15
  + confidence          * 0.10
  + recency             * 0.10
  + relationship_score  * 0.05
```

Keep weights configurable.

Possible storage upgrade:

```text
SQLite
   ↓
PostgreSQL + pgvector
```

Do not require Qdrant unless scale eventually justifies it.

---

# M6 — Importance and Confidence

Every memory should have two distinct values.

## Importance

How useful the memory is likely to be.

```text
0.1 = minor detail
0.5 = useful context
0.9 = critical persistent fact
```

## Confidence

How certain the agent is that the memory is correct.

Example:

```text
User explicitly said it:
confidence = 1.0

Tool output reported it:
confidence = 0.95

Agent inferred it:
confidence = 0.50
```

These should affect retrieval independently.

---

# M7 — Working Memory

Introduce a dedicated short-term state layer.

```rust
pub struct WorkingMemory {
    pub current_goal: Option<String>,
    pub active_plan: Vec<String>,
    pub recent_messages: VecDeque<Message>,
    pub active_entities: HashSet<String>,
    pub temporary_values: HashMap<String, Value>,
}
```

Working memory should not automatically become persistent memory.

Example:

```text
current server IP
temporary command output
partial plan
current retry count
```

Working memory can initially live in-process.

Later, use Dragonfly or Valkey if agents become distributed.

---

# M8 — Episodic Memory

Instead of storing every interaction forever, summarize significant runs.

```rust
pub struct Episode {
    pub id: Uuid,
    pub scope: String,

    pub summary: String,
    pub outcome: String,

    pub entities: Vec<String>,

    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,

    pub source_run_id: Uuid,
}
```

Example:

```text
Configured PostgreSQL HA.

The first PgBouncer configuration failed because of
authentication-file permissions. Permissions were corrected
and the service started successfully.
```

Episodes should reference the underlying run evidence.

---

# M9 — Memory Consolidation

Introduce periodic maintenance.

```text
Memory Collection
       │
       ▼
 Memory Curator
       │
 ┌─────┼──────────────────────┐
 ▼     ▼          ▼           ▼
merge dedupe  contradictions summarize
```

Consolidation should:

- merge duplicates,
- summarize repeated observations,
- identify contradictions,
- mark obsolete memories,
- strengthen repeatedly confirmed memories.

Example:

```text
Memory 1:
Project uses Rust.

Memory 2:
Agent runtime is implemented in Rust.

Memory 3:
Rust was selected for the runtime.
```

Could consolidate to:

```text
The agent runtime uses Rust.
```

Original evidence remains available.

---

# M10 — Memory Decay

Not everything should remain equally relevant forever.

Use decay for ranking, not immediate deletion.

Example:

```text
freshness_score =
e^(-age / half_life)
```

Different memory types can use different half-lives.

Example:

```text
configuration       90 days
temporary context    1 day
user preference    365 days
project decision   730 days
```

Important or repeatedly used memories can decay more slowly.

---

# M11 — Forgetting and Archiving

Support several forms of forgetting.

```text
soft forget
archive
expire
hard delete
```

### Soft Forget

Exclude memory from normal retrieval.

### Archive

Keep for historical reference.

### Expire

Automatically become inactive after a date.

### Hard Delete

Physically remove.

Deletion should cascade appropriately through relationships and indexes.

---

# M12 — Context Compiler

This is one of the most important components.

Memory retrieval should not equal prompt injection.

```text
User Request
      │
      ▼
Memory Search
      │
      ▼
Candidate Memories
      │
      ▼
Context Compiler
      │
 ┌────┼───────────┐
 ▼    ▼           ▼
rank dedupe   token budget
      │
      ▼
Selected Memories
      │
      ▼
LLM Context
```

Define:

```rust
pub struct ContextCandidate {
    pub memory: Memory,
    pub relevance: f32,
    pub token_cost: usize,
}
```

The compiler should optimize for:

```text
maximum useful context
within token budget
```

Do not inject 50 memories simply because they matched.

Usually:

```text
3–10 strong memories
```

will outperform a large dump.

---

# M13 — Memory Access Tracking

Track when memories are actually used.

```text
memory_usage

memory_id
run_id
retrieved_at
injected
helpful
```

This provides evidence for later ranking.

Useful memories should become easier to retrieve.

Unused memories should slowly lose priority.

---

# M14 — Memory Correction

The user and agent should be able to correct memory explicitly.

Tools:

```text
memory.correct
memory.supersede
memory.archive
memory.forget
```

Example:

```text
User:
We no longer use SQLite. We moved to PostgreSQL.
```

Result:

```text
PostgreSQL memory
     │
     └── supersedes
             │
             ▼
       SQLite memory
```

Do not silently erase historical information.

---

# M15 — Memory Introspection

Allow the agent or operator to inspect memory.

Commands/API:

```text
memory.list
memory.search
memory.inspect
memory.history
memory.relationships
memory.stats
```

Useful diagnostics:

```text
total memories
memories by type
memories by scope
unused memories
expired memories
contradictions
duplicate candidates
average confidence
```

---

# M16 — Memory Safety

Memory is a security boundary.

Never automatically persist:

```text
passwords
tokens
private keys
session cookies
credentials
secret values
```

Add a scanner before persistence.

```text
Memory Candidate
       │
       ▼
 Sensitive Data Scan
       │
   ┌───┴────┐
   ▼        ▼
 safe     unsafe
   │        │
   ▼        ▼
 store    reject/redact
```

Also support memory permissions.

Example:

```text
agent:global
user:garrett
project:arcella
```

An agent should not retrieve memory from scopes it does not have permission to access.

---

# M17 — Memory Versioning

Important memories should have revision history.

```text
memory_versions

id
memory_id
version
content
confidence
reason
created_at
```

Use this when a memory is edited rather than superseded.

Example:

```text
v1
"The API uses port 8000."

v2
"The API uses port 8080."
```

If this represents a historical change, prefer superseding.

If it corrects a typo or malformed content, use versioning.

---

# M18 — Memory Maintenance

Create a `MemoryCurator`.

Responsibilities:

```text
duplicate detection
contradiction detection
stale-memory review
consolidation
expiration
quality checks
relationship repair
```

Modes:

```rust
pub enum MemoryMaintenanceMode {
    Off,
    Propose,
    Auto,
}
```

Start with:

```text
Propose
```

Later allow low-risk automatic cleanup.

---

# Recommended Database Tables

By maturity:

```text
memories
memory_versions
memory_relationships
memory_usage
episodes
memory_events
```

Optional later:

```text
memory_embeddings
memory_proposals
memory_archive
```

---

# SQLite V1 Schema

```sql
CREATE TABLE memories (
    id TEXT PRIMARY KEY,

    scope TEXT NOT NULL,
    kind TEXT NOT NULL,

    content TEXT NOT NULL,

    source TEXT NOT NULL,

    importance REAL NOT NULL DEFAULT 0.5,
    confidence REAL NOT NULL DEFAULT 1.0,

    status TEXT NOT NULL DEFAULT 'active',

    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_accessed_at TEXT,
    expires_at TEXT,

    tags TEXT
);

CREATE INDEX idx_memories_scope
ON memories(scope);

CREATE INDEX idx_memories_status
ON memories(status);
```

FTS:

```sql
CREATE VIRTUAL TABLE memories_fts USING fts5(
    id UNINDEXED,
    content
);
```

---

# Rust Project Layout

```text
crates/
└── memory/
    ├── src/
    │   ├── lib.rs
    │   ├── model.rs
    │   ├── manager.rs
    │   ├── store.rs
    │   ├── sqlite.rs
    │   │
    │   ├── capture/
    │   │   ├── evaluator.rs
    │   │   ├── candidate.rs
    │   │   └── filter.rs
    │   │
    │   ├── retrieval/
    │   │   ├── search.rs
    │   │   ├── rank.rs
    │   │   └── hybrid.rs
    │   │
    │   ├── context/
    │   │   ├── compiler.rs
    │   │   └── budget.rs
    │   │
    │   ├── relationships/
    │   │   ├── graph.rs
    │   │   └── contradictions.rs
    │   │
    │   ├── consolidation/
    │   │   ├── merge.rs
    │   │   ├── dedupe.rs
    │   │   └── summarize.rs
    │   │
    │   ├── lifecycle/
    │   │   ├── decay.rs
    │   │   ├── expiration.rs
    │   │   └── archive.rs
    │   │
    │   ├── safety/
    │   │   └── scanner.rs
    │   │
    │   └── curator/
    │       └── maintenance.rs
    │
    └── migrations/
```

---

# Public Memory API

Eventually expose:

```text
memory.remember
memory.recall
memory.search
memory.get
memory.list
memory.correct
memory.supersede
memory.archive
memory.forget
memory.history
```

The agent should not have direct database access.

Everything goes through `MemoryManager`.

---

# Integration With Skills

Memory can help discover skills:

```text
Memory:
"PostgreSQL auth failed because userlist.txt had wrong permissions."

          ↓

Repeated occurrence

          ↓

Skill:
"PgBouncer authentication permission recovery"
```

But memory itself should not turn into a procedure automatically.

That decision belongs to the Skills learning system.

---

# Integration With Self-Evolution

Memory provides historical evidence.

```text
Agent Runs
    │
    ▼
Episodes
    │
    ▼
Repeated failures
    │
    ▼
Evolution Evaluator
```

Example:

```text
Memory shows:
Container diagnostics repeatedly require six tool calls.

Evolution concludes:
Create a docker.diagnose tool.
```

This gives the evolution system evidence instead of intuition.

---

# Development Roadmap

I would implement the full system in this sequence:

```text
M1
Persistent SQLite memory
 ↓
M2
Automatic capture
 ↓
M3
Deduplication + updates
 ↓
M4
Relationships + superseding
 ↓
M5
Hybrid lexical/vector retrieval
 ↓
M6
Importance + confidence
 ↓
M7
Working memory
 ↓
M8
Episodic memory
 ↓
M9
Consolidation
 ↓
M10
Decay
 ↓
M11
Archiving + forgetting
 ↓
M12
Context compiler
 ↓
M13
Usage tracking
 ↓
M14
Correction system
 ↓
M15
Introspection
 ↓
M16
Security controls
 ↓
M17
Versioning
 ↓
M18
Autonomous maintenance
```

---

# Recommended Practical Milestones

## Phase 1 — Useful Memory

Build:

```text
M1–M3
```

The agent can:

- remember,
- search,
- automatically capture,
- avoid basic duplicates.

This is your first production-usable version.

---

## Phase 2 — Reliable Memory

Build:

```text
M4–M8
```

Add:

- relationships,
- hybrid search,
- confidence,
- working memory,
- episodic memory.

At this point the memory system becomes meaningfully intelligent.

---

## Phase 3 — Managed Memory

Build:

```text
M9–M14
```

Add:

- consolidation,
- decay,
- archival,
- context optimization,
- usage tracking,
- corrections.

This prevents the memory store from degrading over time.

---

## Phase 4 — Autonomous Memory

Build:

```text
M15–M18
```

Add:

- inspection,
- security controls,
- version history,
- autonomous maintenance.

At this point the memory subsystem can largely maintain itself.

---

# Core Design Rules

1. **Memory is not conversation history.**
2. **Memory is not a skill.**
3. **Memory is not automatically truth.**
4. **Every durable memory should have provenance.**
5. **New information should update or supersede old information rather than blindly duplicate it.**
6. **Never rely solely on vector similarity.**
7. **Retrieval and prompt injection are separate operations.**
8. **A retrieved memory does not automatically deserve context space.**
9. **Inference should have lower confidence than direct evidence.**
10. **Sensitive information should be rejected before persistence.**
11. **Old memories should lose retrieval priority before being deleted.**
12. **Historical changes should remain traceable.**
13. **The runtime owns memory policy; the model proposes memories.**
14. **All autonomous maintenance must be auditable and reversible.**

---

# Final Architecture

```text
                           AGENT
                             │
          ┌──────────────────┼──────────────────┐
          │                  │                  │
          ▼                  ▼                  ▼
       Working           Retrieval           Capture
       Memory             Engine            Evaluator
          │                  │                  │
          │                  ▼                  │
          │           Context Compiler          │
          │                  │                  │
          └──────────────┬───┴──────────────────┘
                         ▼
                    MemoryManager
                         │
       ┌─────────────────┼──────────────────┐
       ▼                 ▼                  ▼
 Semantic Memory    Episodic Memory    Relationships
       │                 │                  │
       └─────────────────┼──────────────────┘
                         ▼
                    Memory Store
                         │
                    SQLite / PG
                         │
                         ▼
                    Memory Curator
                         │
        ┌────────────────┼────────────────┐
        ▼                ▼                ▼
      Merge          Supersede         Archive
        │                │                │
        └────────────────┼────────────────┘
                         ▼
                  Memory History
```

The final goal is not to make the agent remember everything.

The goal is to make it **remember the right things, retrieve them at the right time, understand how trustworthy they are, and continuously keep the memory collection useful instead of allowing it to become an unmanageable pile of context.**
