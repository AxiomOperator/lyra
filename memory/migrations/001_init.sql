CREATE TABLE memories (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL,
    content TEXT NOT NULL,
    tags TEXT,            -- JSON array of strings
    source TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_memories_scope ON memories(scope);

-- Keyword search over content; kept in sync with `memories` by the store.
-- Porter stemming lets "decide" match "decided".
CREATE VIRTUAL TABLE memories_fts USING fts5(
    id UNINDEXED,
    content,
    tokenize = 'porter unicode61'
);
