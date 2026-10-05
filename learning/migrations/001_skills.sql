CREATE TABLE skills (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    instructions TEXT NOT NULL,
    source TEXT NOT NULL,
    confidence REAL NOT NULL DEFAULT 0.5,
    status TEXT NOT NULL DEFAULT 'proposed',   -- proposed | active | rejected
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_skills_status ON skills(status);
CREATE UNIQUE INDEX idx_skills_name ON skills(name);

-- Keyword search for picking skills relevant to a task; kept in sync by the store.
CREATE VIRTUAL TABLE skills_fts USING fts5(
    id UNINDEXED,
    name,
    description,
    instructions,
    tokenize = 'porter unicode61'
);
