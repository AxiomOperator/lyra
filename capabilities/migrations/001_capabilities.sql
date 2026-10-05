-- Capability usage history (C5): every invocation and how it went.
CREATE TABLE capability_usage (
    id TEXT PRIMARY KEY,
    capability_id TEXT NOT NULL,
    run_id TEXT,
    success INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    retries INTEGER NOT NULL,
    error_code TEXT,
    error TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_capability_usage_cap ON capability_usage(capability_id, created_at);
