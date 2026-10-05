-- The full memory system (docs/memory_system.md): kinds, trust, lifecycle,
-- provenance, history, relationships, usage, episodes and vectors.

ALTER TABLE memories ADD COLUMN kind TEXT NOT NULL DEFAULT 'semantic';      -- semantic | episodic | working
ALTER TABLE memories ADD COLUMN importance REAL NOT NULL DEFAULT 0.5;
ALTER TABLE memories ADD COLUMN confidence REAL NOT NULL DEFAULT 0.9;
ALTER TABLE memories ADD COLUMN status TEXT NOT NULL DEFAULT 'active';      -- active | superseded | archived | deleted
ALTER TABLE memories ADD COLUMN last_accessed_at TEXT;
ALTER TABLE memories ADD COLUMN expires_at TEXT;
ALTER TABLE memories ADD COLUMN run_id TEXT;                                -- provenance: the run it came from
ALTER TABLE memories ADD COLUMN tool_call_id TEXT;                          -- ...and the tool call, if any
CREATE INDEX idx_memories_status ON memories(status);

-- Edits (corrections of the same fact); historical changes supersede instead.
CREATE TABLE memory_versions (
    id TEXT PRIMARY KEY,
    memory_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    content TEXT NOT NULL,
    confidence REAL NOT NULL,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (memory_id, version)
);

-- supports | contradicts | supersedes | derived_from | related_to
CREATE TABLE memory_relationships (
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    relationship TEXT NOT NULL,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (from_id, to_id, relationship)
);

-- Which runs had which memories put in front of the model, and whether they helped.
CREATE TABLE memory_usage (
    id TEXT PRIMARY KEY,
    memory_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    retrieved_at TEXT NOT NULL,
    injected INTEGER NOT NULL,          -- 1: in the prompt, 0: retrieved only
    helpful INTEGER                     -- NULL until the user's reaction says
);
CREATE INDEX idx_memory_usage_memory ON memory_usage(memory_id);
CREATE INDEX idx_memory_usage_run ON memory_usage(run_id);

-- Episodes: summaries of significant runs. Each is also an episodic memory
-- (same id) so it can be searched like any other.
CREATE TABLE episodes (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL,
    summary TEXT NOT NULL,
    outcome TEXT NOT NULL,
    entities TEXT NOT NULL,             -- JSON array
    started_at TEXT NOT NULL,
    ended_at TEXT NOT NULL,
    source_run_id TEXT
);

-- Embedding vectors (little-endian f32), per model.
CREATE TABLE memory_embeddings (
    memory_id TEXT NOT NULL,
    model TEXT NOT NULL,
    dims INTEGER NOT NULL,
    vector BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (memory_id, model)
);

-- Maintenance changes waiting for approval.
CREATE TABLE memory_proposals (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    change TEXT NOT NULL,               -- JSON
    reason TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',   -- pending | applied | rejected
    created_at TEXT NOT NULL,
    resolved_at TEXT
);

-- Audit log of every change.
CREATE TABLE memory_events (
    id TEXT PRIMARY KEY,
    memory_id TEXT,
    kind TEXT NOT NULL,
    from_state TEXT,
    to_state TEXT,
    reason TEXT NOT NULL,
    run_id TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_memory_events_memory ON memory_events(memory_id);
