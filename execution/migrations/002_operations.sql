-- P15: operations that changed something, by their stable id, so a repeated
-- call (a retry, a resume after a restart) returns the recorded result
-- instead of acting twice.
CREATE TABLE operations (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL,
    step_id TEXT NOT NULL,
    tool TEXT NOT NULL,
    result TEXT NOT NULL,               -- JSON
    created_at TEXT NOT NULL
);
