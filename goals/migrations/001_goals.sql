-- Long-lived goals (docs/goal_manager.md): what the agent is trying to
-- accomplish over time, the plans that worked toward each, what blocks them,
-- what wakes them up, and everything that happened to them.

CREATE TABLE goals (
    id TEXT PRIMARY KEY,
    parent_id TEXT,
    status TEXT NOT NULL,
    goal TEXT NOT NULL,            -- JSON Goal
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_goals_status ON goals(status);

-- G4: each plan is one attempt or work session toward a goal.
CREATE TABLE goal_plans (
    goal_id TEXT NOT NULL,
    plan_id TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    outcome TEXT,                  -- execution goal status when it ended
    summary TEXT,
    tokens INTEGER NOT NULL DEFAULT 0,
    autonomous INTEGER NOT NULL DEFAULT 0,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    PRIMARY KEY (goal_id, plan_id)
);

-- G6: why a goal can't progress, until the state changes.
CREATE TABLE goal_blockers (
    id TEXT PRIMARY KEY,
    goal_id TEXT NOT NULL,
    blocker_type TEXT NOT NULL,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL,
    resolved_at TEXT
);
CREATE INDEX idx_goal_blockers_goal ON goal_blockers(goal_id);

-- G9, G10: when to wake a goal.
CREATE TABLE goal_triggers (
    id TEXT PRIMARY KEY,
    goal_id TEXT NOT NULL,
    trigger TEXT NOT NULL,         -- JSON Trigger
    last_fired TEXT,
    active INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

-- Everything that happened, for history, Memory and Evolution.
CREATE TABLE goal_events (
    id TEXT PRIMARY KEY,
    goal_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    message TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_goal_events_goal ON goal_events(goal_id, created_at);
