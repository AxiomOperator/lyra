-- The skills ledger: everything about skills except their current content,
-- which lives in the Markdown files.

-- Immutable snapshots; one is taken before every change.
CREATE TABLE skill_versions (
    id TEXT PRIMARY KEY,
    skill_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    instructions TEXT NOT NULL,
    change_reason TEXT NOT NULL,
    run_id TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (skill_id, version)
);

-- Which runs (user turns) had which skills in their prompt, and how it went.
CREATE TABLE skill_usage (
    id TEXT PRIMARY KEY,
    skill_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    used_at TEXT NOT NULL,
    outcome TEXT NOT NULL DEFAULT 'unknown'   -- unknown | success | failure | partial
);
CREATE INDEX idx_skill_usage_skill ON skill_usage(skill_id);
CREATE INDEX idx_skill_usage_run ON skill_usage(run_id);

-- supersedes | extends | conflicts_with | related_to
CREATE TABLE skill_relationships (
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (from_id, to_id, kind)
);

-- Changes waiting for approval (updates, merges, splits, promotions, deprecations).
CREATE TABLE skill_proposals (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    skill_id TEXT,
    change TEXT NOT NULL,          -- JSON
    reason TEXT NOT NULL,
    evidence TEXT NOT NULL,
    confidence REAL NOT NULL,
    run_id TEXT,
    status TEXT NOT NULL DEFAULT 'pending',   -- pending | applied | rejected
    created_at TEXT NOT NULL,
    resolved_at TEXT
);
CREATE INDEX idx_skill_proposals_status ON skill_proposals(status);

-- Audit log of every change to a skill or the collection.
CREATE TABLE skill_events (
    id TEXT PRIMARY KEY,
    skill_id TEXT,
    kind TEXT NOT NULL,
    from_state TEXT,
    to_state TEXT,
    reason TEXT NOT NULL,
    evidence TEXT NOT NULL,
    run_id TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_skill_events_skill ON skill_events(skill_id);
