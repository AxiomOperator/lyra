-- Planning and execution state (docs/planning_execution_system.md). Plans are
-- structured state: steps are stored as JSON on the plan row so a revision
-- is one atomic write; everything that happens is recorded alongside.

CREATE TABLE goals (
    id TEXT PRIMARY KEY,
    request TEXT NOT NULL,
    description TEXT NOT NULL,
    success_criteria TEXT NOT NULL,     -- JSON array
    constraints TEXT NOT NULL,          -- JSON array
    outputs TEXT NOT NULL,              -- JSON array
    destructive INTEGER NOT NULL,
    ambiguities TEXT NOT NULL,          -- JSON array
    status TEXT NOT NULL,
    evaluation TEXT,                    -- JSON, once judged
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE plans (
    id TEXT PRIMARY KEY,
    goal_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    status TEXT NOT NULL,
    steps TEXT NOT NULL,                -- JSON array of steps
    budget TEXT NOT NULL,               -- JSON
    usage TEXT NOT NULL,                -- JSON
    note TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_plans_status ON plans(status);

-- Every attempt at every step: the durable result (P4, P6).
CREATE TABLE step_executions (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL,
    step_id TEXT NOT NULL,
    plan_version INTEGER NOT NULL,
    attempt INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT NOT NULL,
    success INTEGER NOT NULL,
    output TEXT NOT NULL,
    error TEXT,
    failure_class TEXT,
    operation_id TEXT
);
CREATE INDEX idx_step_executions_plan ON step_executions(plan_id);

CREATE TABLE verification_results (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL,
    step_id TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    verified INTEGER NOT NULL,
    evidence TEXT NOT NULL,             -- JSON array
    reason TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE plan_revisions (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL,
    from_version INTEGER NOT NULL,
    to_version INTEGER NOT NULL,
    revision TEXT NOT NULL,             -- JSON
    created_at TEXT NOT NULL
);

CREATE TABLE checkpoints (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL,
    plan_version INTEGER NOT NULL,
    completed_steps TEXT NOT NULL,      -- JSON array
    runtime_state TEXT NOT NULL,        -- JSON
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE execution_events (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL,
    step_id TEXT,
    kind TEXT NOT NULL,
    message TEXT NOT NULL,
    data TEXT NOT NULL,                 -- JSON
    created_at TEXT NOT NULL
);
CREATE INDEX idx_execution_events_plan ON execution_events(plan_id);

-- Approvals are bound to the exact action (P14).
CREATE TABLE approvals (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL,
    step_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    approved_at TEXT NOT NULL
);
