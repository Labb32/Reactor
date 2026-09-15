CREATE TABLE reactor_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE workspaces (
    id TEXT PRIMARY KEY,
    canonical_path TEXT NOT NULL UNIQUE,
    canonical_key TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL CHECK (status IN ('REGISTERED', 'READY', 'DEGRADED')),
    policy_json TEXT NOT NULL,
    policy_fingerprint TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE workspace_agents (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    agent_kind TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('UNPREPARED', 'READY', 'DEGRADED')),
    capability_json TEXT NOT NULL,
    policy_fingerprint TEXT,
    checked_at TEXT,
    PRIMARY KEY (workspace_id, agent_kind)
);

CREATE TABLE plans (
    id TEXT PRIMARY KEY,
    current_version INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE plan_versions (
    plan_id TEXT NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    status TEXT NOT NULL CHECK (status IN ('DRAFT', 'IN_REVIEW', 'APPROVED')),
    title TEXT NOT NULL,
    context TEXT NOT NULL,
    completion_criteria_json TEXT NOT NULL,
    agent_kind TEXT NOT NULL,
    model TEXT,
    limits_json TEXT NOT NULL,
    policy_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (plan_id, version)
);

CREATE TABLE plan_approvals (
    plan_id TEXT NOT NULL,
    plan_version INTEGER NOT NULL,
    approved_by TEXT NOT NULL,
    approved_at TEXT NOT NULL,
    PRIMARY KEY (plan_id, plan_version),
    FOREIGN KEY (plan_id, plan_version)
        REFERENCES plan_versions(plan_id, version) ON DELETE CASCADE
);

CREATE TABLE schedules (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL,
    plan_version INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('ACTIVE', 'PAUSED', 'COMPLETED')),
    rule_json TEXT NOT NULL,
    timezone TEXT NOT NULL,
    start_at TEXT,
    start_deadline TEXT,
    gate_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (plan_id, plan_version)
        REFERENCES plan_versions(plan_id, version)
);

CREATE TABLE schedule_occurrences (
    id TEXT PRIMARY KEY,
    schedule_id TEXT NOT NULL REFERENCES schedules(id) ON DELETE CASCADE,
    scheduled_at TEXT NOT NULL,
    deadline_at TEXT,
    state TEXT NOT NULL CHECK (state IN ('WAITING', 'QUEUED', 'MISSED', 'CANCELLED')),
    run_id TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (schedule_id, scheduled_at)
);

CREATE TABLE runs (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    plan_id TEXT NOT NULL,
    plan_version INTEGER NOT NULL,
    occurrence_id TEXT REFERENCES schedule_occurrences(id),
    state TEXT NOT NULL CHECK (state IN (
        'WAITING', 'QUEUED', 'PREPARING', 'RUNNING', 'SUCCEEDED', 'FAILED',
        'CANCELLED', 'LIMIT_REACHED', 'REVIEW_REQUIRED', 'MISSED'
    )),
    agent_kind TEXT NOT NULL,
    provider TEXT,
    local_endpoint TEXT,
    policy_snapshot_json TEXT NOT NULL,
    limits_snapshot_json TEXT NOT NULL,
    ready_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (plan_id, plan_version)
        REFERENCES plan_versions(plan_id, version)
);

CREATE UNIQUE INDEX one_active_run_per_workspace
ON runs(workspace_id)
WHERE state IN ('PREPARING', 'RUNNING');

CREATE INDEX queued_runs_by_ready_time
ON runs(state, ready_at, created_at);

CREATE TABLE attempts (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    attempt_number INTEGER NOT NULL,
    process_id INTEGER,
    process_started_at TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    exit_code INTEGER,
    termination_reason TEXT,
    log_path TEXT NOT NULL,
    usage_json TEXT,
    UNIQUE (run_id, attempt_number)
);

CREATE TABLE leases (
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    acquired_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    PRIMARY KEY (resource_type, resource_id)
);

CREATE TABLE audit_events (
    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
    event_type TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    occurred_at TEXT NOT NULL
);

CREATE INDEX audit_events_by_entity
ON audit_events(entity_type, entity_id, cursor);

CREATE TABLE idempotency_keys (
    key TEXT PRIMARY KEY,
    request_hash TEXT NOT NULL,
    response_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);
