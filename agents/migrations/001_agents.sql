-- Subagents (docs/done/sub_agents.md): profile versions, the delegation log and
-- the creation wizard's draft. Profiles themselves are files in ~/.lyra/agents.

CREATE TABLE agent_versions (
    name TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    snapshot TEXT NOT NULL,        -- JSON AgentProfile
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (name, version)
);

CREATE TABLE delegations (
    id TEXT PRIMARY KEY,
    run_id TEXT,
    from_agent TEXT NOT NULL,
    agent TEXT NOT NULL,
    task TEXT NOT NULL,
    method TEXT NOT NULL,          -- explicit, rule, semantic, model, planner, tool
    confidence REAL,
    status TEXT NOT NULL,
    output TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    model_calls INTEGER NOT NULL,
    tool_calls INTEGER NOT NULL,
    tokens INTEGER NOT NULL,
    outcome TEXT,                  -- the user's reaction: success, partial, failure
    created_at TEXT NOT NULL
);
CREATE INDEX idx_delegations_agent ON delegations(agent, created_at);
CREATE INDEX idx_delegations_run ON delegations(run_id);

CREATE TABLE agent_drafts (
    id TEXT PRIMARY KEY,
    draft TEXT NOT NULL,           -- JSON AgentCreationDraft
    updated_at TEXT NOT NULL
);
