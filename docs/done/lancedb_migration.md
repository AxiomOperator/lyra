# Memory Database Migration Plan — SQLite to LanceDB

## Objective

Replace SQLite as the agent's persistent memory database with LanceDB while preserving the existing `MemoryManager` abstraction and avoiding changes throughout the agent runtime.

The new architecture should be:

```text id="qlnqqd"
                     Agent Runtime
                          │
                          ▼
                    MemoryManager
                          │
                    MemoryStore
                          │
                          ▼
                       LanceDB
                          │
             ┌────────────┼────────────┐
             ▼            ▼            ▼
          Content      Metadata     Embeddings
```

SQLite remains available for non-memory runtime state:

```text id="76n96i"
data/
├── agent.db
│   ├── skills
│   ├── plans
│   ├── runs
│   ├── configuration
│   └── audit
│
└── memory/
    └── LanceDB
```

LanceDB can operate directly against a local filesystem directory without requiring a separate database server.

---

# Migration Principles

1. Do not make LanceDB-specific APIs visible to the agent runtime.
2. Keep `MemoryManager` as the public memory interface.
3. Introduce LanceDB behind the existing `MemoryStore` trait.
4. Preserve UUIDs during migration.
5. Validate every migrated record.
6. Do not delete SQLite memory until LanceDB has been verified.
7. Introduce vectors independently from the storage migration.
8. Support rollback during the transition.
9. Keep embedding generation outside the storage implementation.
10. Allow future replacement of LanceDB without rewriting the memory system.

---

# Target Memory Architecture

```text id="38u5mt"
                    User / Agent
                         │
                         ▼
                   MemoryManager
                         │
        ┌────────────────┼─────────────────┐
        ▼                ▼                 ▼
     Capture          Retrieval         Lifecycle
        │                │                 │
        └────────────────┼─────────────────┘
                         ▼
                    MemoryStore
                         │
                         ▼
                      LanceDB
                 ┌───────┼───────┐
                 ▼       ▼       ▼
               Text    Vector  Metadata
```

Later:

```text id="27lx2c"
Query
 │
 ├── vector similarity
 ├── full-text search
 ├── metadata filters
 └── scoring/reranking
        │
        ▼
 Context Compiler
```

LanceDB supports vector search, filtering, full-text search, and persistent metadata alongside vectors, so those functions can eventually live behind one storage layer.

---

# L1 — Storage Abstraction Audit

## Goal

Ensure no part of the agent accesses SQLite memory directly.

All memory operations must pass through:

```rust id="7cr2um"
#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn create(
        &self,
        memory: Memory,
    ) -> Result<Memory>;

    async fn get(
        &self,
        id: Uuid,
    ) -> Result<Option<Memory>>;

    async fn search(
        &self,
        query: MemoryQuery,
    ) -> Result<Vec<ScoredMemory>>;

    async fn update(
        &self,
        memory: Memory,
    ) -> Result<Memory>;

    async fn delete(
        &self,
        id: Uuid,
    ) -> Result<()>;
}
```

Locate and remove direct calls such as:

```rust id="vecqla"
sqlx::query(...)
```

from:

```text id="t6i3nf"
MemoryManager
ContextCompiler
MemoryEvaluator
MemoryCurator
```

Only storage implementations should know about their underlying database.

### Completion Criteria

The existing SQLite backend can be replaced by changing dependency injection only.

---

# L2 — Introduce LanceDB Backend

Create:

```text id="kyrn7c"
memory/
├── store.rs
├── sqlite.rs
└── lance.rs
```

Implementation:

```rust id="0glr3u"
pub struct LanceMemoryStore {
    database: lancedb::Connection,
    table: lancedb::Table,
}
```

Startup:

```rust id="34boz2"
let db = lancedb::connect("./data/memory")
    .execute()
    .await?;
```

The Rust SDK currently uses Arrow record batches for table creation and data exchange, with vector columns represented using fixed-size float lists.

Configuration:

```toml id="925hor"
[memory]
backend = "lance"
path = "./data/memory"
table = "memories"
```

Allow:

```text id="lfic0l"
sqlite
lance
```

during migration.

---

# L3 — Define LanceDB Memory Schema

Use one canonical `memories` table initially.

Recommended fields:

```text id="fdhojn"
id
scope
kind
content
source
importance
confidence
status
created_at
updated_at
last_accessed_at
expires_at
tags
embedding
```

Conceptual Arrow schema:

```text id="a7gng4"
id               UTF8
scope            UTF8
kind             UTF8
content          UTF8
source           UTF8

importance       FLOAT32
confidence       FLOAT32

status           UTF8

created_at       TIMESTAMP
updated_at       TIMESTAMP
last_accessed_at TIMESTAMP?
expires_at       TIMESTAMP?

tags             LIST<UTF8>

embedding        FIXED_SIZE_LIST<FLOAT32>
```

The embedding column size should be fixed to the configured embedding model.

For example:

```toml id="vbkf8e"
[memory.embedding]
dimensions = 1024
```

Do not hard-code the dimension in application logic.

---

# L4 — Separate Embedding Provider

LanceDB stores embeddings but should not generate them.

Introduce:

```rust id="9zzmld"
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    fn dimensions(&self) -> usize;

    async fn embed(
        &self,
        text: &str,
    ) -> Result<Vec<f32>>;
}
```

Then:

```text id="usx474"
MemoryManager
      │
      ├── MemoryStore
      │
      └── EmbeddingProvider
```

This allows:

```text id="0osr9t"
local embedding model
vLLM
Ollama
llama.cpp
OpenAI-compatible provider
```

without coupling LanceDB to the model.

---

# L5 — Implement Basic Lance CRUD

Implement:

```text id="2igqwe"
create
get
update
delete
list
```

before vector retrieval.

This validates LanceDB as a replacement storage backend independently from semantic search.

Tests must ensure:

```text id="zrpqca"
UUID preserved
scope preserved
tags preserved
timestamps preserved
confidence preserved
importance preserved
status preserved
```

### Completion Criteria

All existing memory CRUD tests pass against both:

```text id="m85n3z"
SqliteMemoryStore
LanceMemoryStore
```

---

# L6 — Introduce Storage Contract Tests

Create one test suite that every backend must pass.

Example:

```rust id="g8q5ij"
async fn memory_store_contract(
    store: Arc<dyn MemoryStore>
) {
    test_create(&store).await;
    test_get(&store).await;
    test_update(&store).await;
    test_search(&store).await;
    test_delete(&store).await;
}
```

Run against:

```text id="6h0n1x"
SQLite
LanceDB
```

This prevents behavior differences between the two implementations.

---

# L7 — SQLite → Lance Migration Tool

Create a dedicated migration command:

```text id="m7dofx"
agent memory migrate \
  --from sqlite \
  --to lance
```

Migration flow:

```text id="c1t8mx"
SQLite memories
      │
      ▼
Read batch
      │
      ▼
Validate
      │
      ▼
Generate embedding
      │
      ▼
Write Lance batch
      │
      ▼
Verify
      │
      ▼
Next batch
```

Do not process the entire database in memory.

Use configurable batches:

```toml id="y02fdk"
[memory.migration]
batch_size = 500
```

Migration record:

```rust id="uv2gba"
pub struct MigrationStats {
    pub read: u64,
    pub written: u64,
    pub verified: u64,
    pub failed: u64,
}
```

---

# L8 — Migration Checkpointing

Migration must be restartable.

Record:

```text id="et393h"
migration_id
started_at
last_memory_id
records_read
records_written
records_failed
completed
```

If migration stops:

```text id="0bg3np"
restart
   │
   ▼
read checkpoint
   │
   ▼
continue
```

Never restart from record zero unnecessarily.

---

# L9 — Migration Validation

After copying records, perform several validation layers.

## Count Validation

```text id="q3t7mq"
SQLite active memories
=
Lance active memories
```

## ID Validation

Every SQLite UUID must exist in Lance.

## Field Validation

Randomly or completely compare:

```text id="wwhq4y"
scope
content
kind
source
importance
confidence
timestamps
tags
```

## Embedding Validation

Verify:

```text id="02i0o0"
embedding != empty
embedding length == configured dimensions
all values finite
```

Generate a final migration report.

---

# L10 — Dual Write Mode

Before cutover:

```text id="6e52o0"
memory.remember()
       │
       ▼
 MemoryManager
       │
 ┌─────┴─────┐
 ▼           ▼
SQLite     LanceDB
```

Configuration:

```toml id="y0cecx"
[memory]
read_backend = "sqlite"
write_backends = ["sqlite", "lance"]
```

Purpose:

- validate new writes,
- compare retrieval,
- test production behavior,
- provide easy rollback.

Dual-write should be temporary.

---

# L11 — Shadow Reads

Continue returning SQLite results while silently querying Lance.

```text id="cgpzpn"
memory.recall()
       │
       ├── SQLite → user result
       │
       └── Lance → comparison only
```

Measure:

```text id="6hqohw"
result overlap
latency
top result agreement
missing records
ordering differences
errors
```

Log:

```rust id="cym2ci"
pub struct RetrievalComparison {
    pub query: String,
    pub sqlite_ids: Vec<Uuid>,
    pub lance_ids: Vec<Uuid>,
    pub overlap: f32,
}
```

---

# L12 — Add Vector Retrieval

Once storage behavior is validated:

```text id="9kwngx"
query
  │
  ▼
EmbeddingProvider
  │
  ▼
query vector
  │
  ▼
LanceDB
  │
  ▼
nearest memories
```

Initial query interface:

```rust id="x6jyqx"
pub struct MemoryQuery {
    pub text: String,
    pub scope: Option<String>,
    pub kind: Option<MemoryKind>,
    pub status: MemoryStatus,
    pub limit: usize,
}
```

Apply metadata filters before or during retrieval wherever possible.

Examples:

```text id="u24vqg"
scope = project:arcella
status = active
kind = semantic
```

---

# L13 — Add Full-Text Retrieval

Do not abandon lexical search.

Use:

```text id="babfio"
vector search
+
full-text search
```

LanceDB's current Rust SDK supports both vector similarity and full-text search.

Retrieval flow:

```text id="ydhhp5"
                    Query
                      │
          ┌───────────┴───────────┐
          ▼                       ▼
      Vector Search             FTS
          │                       │
          └───────────┬───────────┘
                      ▼
                    Merge
                      │
                      ▼
                   Rerank
```

---

# L14 — Hybrid Ranking

Implement ranking outside Lance initially.

Example:

```text id="7fge4e"
score =
    vector_similarity * 0.45
  + lexical_score     * 0.20
  + importance        * 0.15
  + confidence        * 0.10
  + recency           * 0.10
```

Keep weights configurable:

```toml id="de4up2"
[memory.ranking]
semantic = 0.45
lexical = 0.20
importance = 0.15
confidence = 0.10
recency = 0.10
```

Do not bake ranking policy into the storage engine.

---

# L15 — Vector Index

Initially, use exact vector search.

Do not introduce ANN indexing prematurely.

Once collection size or latency warrants it:

```text id="b00gml"
small dataset
→ exact search

larger dataset
→ vector index
```

Benchmark before deciding.

Track:

```text id="944szd"
P50 latency
P95 latency
recall quality
memory usage
index size
```

---

# L16 — Cut Memory Reads to LanceDB

After shadow-read validation:

```toml id="vgldlx"
[memory]
read_backend = "lance"
write_backends = ["sqlite", "lance"]
```

Architecture:

```text id="8pqhtf"
Read
 ↓
LanceDB

Write
 ├── LanceDB
 └── SQLite fallback
```

Monitor this stage before removing SQLite writes.

---

# L17 — Make LanceDB Authoritative

Once Lance has proven stable:

```toml id="3u6k3a"
[memory]
backend = "lance"
```

Now:

```text id="h97jt8"
MemoryManager
     │
     ▼
 LanceMemoryStore
```

Stop new SQLite memory writes.

Do not delete the old memory table yet.

---

# L18 — Archive SQLite Memory

Rename or export old storage:

```text id="h24uuv"
agent.db
```

Either:

```text id="ple9f8"
rename memories table
→ memories_legacy
```

or:

```text id="osv0t7"
export
→ backup/memory-sqlite-final.db
```

Retain until LanceDB has run successfully for the chosen retention period.

---

# L19 — Update Backup Strategy

Previously:

```text id="rzr5db"
backup agent.db
```

Now:

```text id="8p1gwy"
backup/
├── agent.db
└── memory/
```

Treat LanceDB storage as its own backup unit.

Backup procedure:

```text id="05zgr4"
pause memory mutations if required
        ↓
snapshot Lance directory
        ↓
snapshot agent.db
        ↓
record generation timestamp
```

Restore testing should be part of CI or periodic maintenance.

---

# L20 — Memory Schema Versioning

Add:

```rust id="t20fg5"
pub const MEMORY_SCHEMA_VERSION: u32 = 1;
```

Configuration:

```text id="tj9iyn"
memory metadata
└── schema_version
```

Future changes:

```text id="24ywwn"
v1
 ↓
v2
 ↓
migration
```

Never assume Lance table schema will remain fixed forever.

---

# L21 — Embedding Model Versioning

This is critical.

Store:

```text id="h429t4"
embedding_model
embedding_dimensions
embedding_version
```

either per memory or table generation.

Example:

```text id="bww2tb"
Qwen3-Embedding-8B
dimensions: 1024
generation: 1
```

If the embedding model changes, do not mix incompatible vectors blindly.

Migration:

```text id="m50k6j"
old vectors
    │
    ▼
re-embedding job
    │
    ▼
new vector generation
```

---

# L22 — Re-Embedding Pipeline

Build a background maintenance operation:

```text id="icuyah"
agent memory reembed
```

Flow:

```text id="9ygg4n"
Select outdated memories
       │
       ▼
Generate new embedding
       │
       ▼
Validate dimension
       │
       ▼
Update memory
```

Support:

```text id="hvyz8i"
batch size
checkpointing
pause/resume
progress metrics
```

---

# L23 — Memory Observability

Track:

```text id="3fvzs2"
memory count
table size
embedding count
embedding failures
search latency
FTS latency
vector latency
hybrid latency
write latency
retrieval hit rate
```

With your existing observability architecture:

```text id="7m01xf"
tracing
OpenTelemetry
```

Emit spans such as:

```text id="o9jrb3"
memory.write
memory.vector_search
memory.fts_search
memory.hybrid_search
memory.embed
```

---

# L24 — Failure Handling

Define explicit LanceDB failure classes:

```rust id="ej38nl"
pub enum MemoryStoreError {
    Unavailable,
    CorruptData,
    SchemaMismatch,
    EmbeddingDimensionMismatch,
    InvalidQuery,
    StorageFailure,
}
```

The agent should never receive low-level Arrow/Lance errors directly.

Normalize them through `MemoryManager`.

---

# L25 — Final Architecture

```text id="dt36nw"
                           Agent
                             │
                             ▼
                       MemoryManager
                             │
          ┌──────────────────┼───────────────────┐
          ▼                  ▼                   ▼
      Capture            Retrieval           Lifecycle
          │                  │                   │
          │       ┌──────────┼──────────┐        │
          │       ▼          ▼          ▼        │
          │     Vector      FTS      Metadata     │
          │       │          │          │        │
          └───────┴──────────┼──────────┴────────┘
                             ▼
                         LanceDB
                             │
                  ┌──────────┼───────────┐
                  ▼          ▼           ▼
                Text      Embeddings   Metadata
```

Meanwhile:

```text id="j3a2w9"
SQLite
├── skills
├── plans
├── execution
├── evolution
├── config
└── audit
```

The two systems have deliberately different responsibilities.

---

# Rust Project Layout

```text id="8z0duo"
crates/
└── memory/
    ├── src/
    │   ├── lib.rs
    │   ├── manager.rs
    │   ├── model.rs
    │   ├── query.rs
    │   │
    │   ├── store/
    │   │   ├── mod.rs
    │   │   ├── sqlite.rs
    │   │   └── lance.rs
    │   │
    │   ├── embedding/
    │   │   ├── mod.rs
    │   │   └── provider.rs
    │   │
    │   ├── retrieval/
    │   │   ├── vector.rs
    │   │   ├── fulltext.rs
    │   │   ├── hybrid.rs
    │   │   └── ranking.rs
    │   │
    │   ├── migration/
    │   │   ├── sqlite_to_lance.rs
    │   │   ├── checkpoint.rs
    │   │   └── verify.rs
    │   │
    │   └── maintenance/
    │       ├── reembed.rs
    │       └── optimize.rs
    │
    └── tests/
        ├── store_contract.rs
        ├── migration.rs
        └── retrieval.rs
```

---

# Development Phases

## Phase 1 — Backend Foundation

Implement:

```text id="bvnjtn"
L1–L6
```

Deliverables:

- storage abstraction cleaned up,
- LanceDB dependency added,
- Lance schema defined,
- basic CRUD operational,
- backend contract tests passing.

No production migration yet.

---

## Phase 2 — Data Migration

Implement:

```text id="8hwnau"
L7–L9
```

Deliverables:

- SQLite migration CLI,
- checkpoint support,
- migration validation,
- embedding generation.

At this point existing memory can be reproduced in LanceDB.

---

## Phase 3 — Safe Production Transition

Implement:

```text id="betryo"
L10–L11
```

Deliverables:

- dual writes,
- shadow reads,
- comparison metrics.

Continue using SQLite as authoritative storage during this phase.

---

## Phase 4 — Semantic Retrieval

Implement:

```text id="5cxdxq"
L12–L15
```

Deliverables:

- vector search,
- FTS,
- hybrid retrieval,
- ranking,
- optional vector indexes.

This is when LanceDB starts providing functionality beyond the old SQLite implementation.

---

## Phase 5 — Cutover

Implement:

```text id="fm3vzl"
L16–L18
```

Transition:

```text id="khxwgr"
SQLite primary
     ↓
dual
     ↓
Lance read primary
     ↓
Lance primary
     ↓
SQLite memory archived
```

---

## Phase 6 — Production Hardening

Implement:

```text id="r9xyjd"
L19–L24
```

Deliverables:

- backup/restore,
- schema versioning,
- embedding versioning,
- re-embedding,
- observability,
- normalized error handling.

---

# Recommended Cargo Dependencies

Conceptually:

```toml id="x63y7n"
[dependencies]
lancedb = "0.39"
arrow = "58"
arrow-array = "58"
arrow-schema = "58"

tokio = { version = "1", features = ["full"] }
async-trait = "0.1"

serde = { version = "1", features = ["derive"] }
serde_json = "1"

uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }

anyhow = "1"
thiserror = "2"
```

Pin compatible Arrow versions to whatever the selected LanceDB release requires rather than independently upgrading them. The current LanceDB Rust SDK uses Arrow extensively for schemas and record batches.

---

# Configuration

Recommended:

```toml id="71787f"
[memory]
backend = "lance"
path = "./data/memory"
table = "memories"

[memory.embedding]
provider = "openai-compatible"
model = "Qwen3-Embedding-8B"
dimensions = 1024

[memory.retrieval]
mode = "hybrid"
default_limit = 10

[memory.ranking]
semantic = 0.45
lexical = 0.20
importance = 0.15
confidence = 0.10
recency = 0.10
```

---

# Testing Requirements

## Backend Contract Tests

Every memory operation must behave identically across SQLite and LanceDB.

## Migration Tests

Test:

```text id="x2tv8v"
empty database
single memory
thousands of memories
unicode content
large content
null expiration
many tags
duplicate IDs
malformed data
embedding failure
restart during migration
```

## Retrieval Tests

Create known memories and assert that relevant queries retrieve them.

Example:

```text id="dsj9c4"
Memory:
"The agent runtime is written in Rust."

Query:
"What language is the runtime implemented in?"

Expected:
memory appears in top results
```

## Regression Dataset

Build a permanent memory retrieval benchmark.

Store:

```text id="28wy4r"
query
expected memory IDs
minimum acceptable rank
```

Run it whenever retrieval logic or embedding models change.

---

# Success Criteria

The migration is complete when:

1. LanceDB stores all durable memories.
2. SQLite contains no active memory records.
3. All memory APIs continue working without caller changes.
4. Existing UUIDs and metadata have been preserved.
5. Vector retrieval works.
6. FTS retrieval works.
7. Hybrid retrieval outperforms the previous SQLite FTS retrieval benchmark.
8. Backup and restore have been successfully tested.
9. Re-embedding can occur without rebuilding the entire application.
10. The original SQLite memory database can be retained solely as a migration backup.

---

# Recommended Final Decision

Do **not** replace SQLite globally.

Use:

```text id="25s043"
LanceDB
└── AI memory
    ├── semantic memories
    ├── episodic memories
    ├── embeddings
    ├── retrieval metadata
    └── eventually document chunks

SQLite
└── agent operational state
    ├── skills
    ├── plans
    ├── runs
    ├── execution history
    ├── approvals
    ├── evolution events
    └── configuration
```

This keeps LanceDB focused on the area where its vector and retrieval capabilities provide a real advantage while retaining SQLite's simplicity and transactional behavior for ordinary agent state.
