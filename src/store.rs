use std::{path::Path, time::Duration};

use rusqlite::{Connection, Transaction, TransactionBehavior, params};

use crate::{
    domain::RunState,
    error::{ReactorError, Result},
};

pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        configure_connection(&connection)?;
        Ok(Self { connection })
    }

    pub fn open_initialized(path: &Path) -> Result<Self> {
        let store = Self::open(path)?;
        store.ensure_supported_schema()?;
        Ok(store)
    }

    pub fn migrate(&mut self) -> Result<u32> {
        let current = self.schema_version()?;
        reject_newer_schema(current)?;

        if current == SUPPORTED_SCHEMA_VERSION {
            return Ok(current);
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Exclusive)?;

        if current == 0 {
            transaction.execute_batch(MIGRATION_1)?;
            transaction.pragma_update(None, "user_version", SUPPORTED_SCHEMA_VERSION)?;
        }

        transaction.commit()?;
        Ok(SUPPORTED_SCHEMA_VERSION)
    }

    pub fn schema_version(&self) -> Result<u32> {
        Ok(self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    pub fn ensure_supported_schema(&self) -> Result<()> {
        let current = self.schema_version()?;
        reject_newer_schema(current)?;
        if current == 0 {
            return Err(ReactorError::UninitializedDatabase);
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn write_transaction<T>(
        &mut self,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        self.ensure_supported_schema()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = operation(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }

    #[allow(dead_code)]
    pub fn transition_run_state(
        &mut self,
        run_id: &str,
        expected: RunState,
        next: RunState,
        occurred_at: &str,
    ) -> Result<bool> {
        if !expected.can_transition_to(next) {
            return Err(ReactorError::InvalidRunTransition {
                from: expected.to_string(),
                to: next.to_string(),
            });
        }

        self.write_transaction(|transaction| {
            let changed = transaction.execute(
                "UPDATE runs SET state = ?1, updated_at = ?2 WHERE id = ?3 AND state = ?4",
                params![next.as_str(), occurred_at, run_id, expected.as_str()],
            )?;

            if changed == 1 {
                transaction.execute(
                    "INSERT INTO audit_events (event_type, entity_type, entity_id, payload_json, occurred_at)
                     VALUES ('RUN_STATE_CHANGED', 'run', ?1, ?2, ?3)",
                    params![
                        run_id,
                        format!(
                            "{{\"from\":\"{}\",\"to\":\"{}\"}}",
                            expected.as_str(),
                            next.as_str()
                        ),
                        occurred_at
                    ],
                )?;
            }

            Ok(changed == 1)
        })
    }
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

fn reject_newer_schema(found: u32) -> Result<()> {
    if found > SUPPORTED_SCHEMA_VERSION {
        return Err(ReactorError::UnsupportedSchema {
            found,
            supported: SUPPORTED_SCHEMA_VERSION,
        });
    }
    Ok(())
}

const MIGRATION_1: &str = r#"
CREATE TABLE workspaces (
    id TEXT PRIMARY KEY,
    canonical_path TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL CHECK (status IN ('REGISTERED', 'READY', 'DEGRADED')),
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
"#;
