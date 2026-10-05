-- Self-evolution (docs/self_evolution.md): run telemetry, candidates,
-- generations and the immutable evolution history.

CREATE TABLE runs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    outcome TEXT NOT NULL,
    corrected INTEGER NOT NULL,
    generation INTEGER NOT NULL,
    record TEXT NOT NULL,           -- JSON RunRecord
    created_at TEXT NOT NULL
);
CREATE INDEX idx_runs_created ON runs(created_at);

CREATE TABLE candidates (
    id TEXT PRIMARY KEY,
    grp TEXT NOT NULL,
    status TEXT NOT NULL,
    candidate TEXT NOT NULL,        -- JSON Candidate
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_candidates_status ON candidates(status);

CREATE TABLE generations (
    id TEXT PRIMARY KEY,
    number INTEGER NOT NULL UNIQUE,
    parent TEXT,
    generation TEXT NOT NULL,       -- JSON Generation (with its snapshot)
    created_at TEXT NOT NULL
);

-- Append-only: nothing updates or deletes these rows.
CREATE TABLE evolution_events (
    id TEXT PRIMARY KEY,
    candidate TEXT,
    kind TEXT NOT NULL,
    description TEXT NOT NULL,
    old_generation INTEGER,
    new_generation INTEGER,
    fitness_before REAL,
    fitness_after REAL,
    created_at TEXT NOT NULL
);
