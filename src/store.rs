use std::{path::Path, time::Duration};

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::Serialize;
use serde_json::Value;

use crate::{
    agents::AgentPreflight,
    domain::RunState,
    error::{ReactorError, Result},
    workspace::{
        CanonicalWorkspace, WorkspaceAgentRecord, WorkspacePolicy, WorkspaceRecord, paths_overlap,
    },
};

pub const SUPPORTED_SCHEMA_VERSION: u32 = 3;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_EVENT_COUNT: i64 = 100_000;

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub workspaces: Vec<WorkspaceSnapshot>,
    pub runs: Vec<RunSnapshot>,
    pub queued_runs: i64,
    pub latest_event_cursor: i64,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceSnapshot {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct RunSnapshot {
    pub id: String,
    pub workspace_id: String,
    pub state: String,
    pub agent_kind: String,
    pub ready_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct AuditEvent {
    pub cursor: i64,
    pub event_type: String,
    pub entity_type: String,
    pub entity_id: String,
    pub payload: Value,
    pub occurred_at: String,
}

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
        // Read commands must not create an empty database.
        if !path.exists() {
            return Err(ReactorError::UninitializedDatabase);
        }
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        configure_connection(&connection)?;
        let store = Self { connection };
        store.ensure_supported_schema()?;
        configure_storage(&store.connection)?;
        Ok(store)
    }

    pub fn migrate(&mut self) -> Result<u32> {
        let current = self.schema_version()?;
        reject_newer_schema(current)?;
        configure_storage(&self.connection)?;

        if current == SUPPORTED_SCHEMA_VERSION {
            return Ok(current);
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Exclusive)?;

        if current == 0 {
            transaction.execute_batch(MIGRATION_1)?;
            transaction.execute(
                "INSERT INTO reactor_meta (key, value) VALUES ('instance_id', ?1)",
                [crate::ids::new_id()],
            )?;
            transaction.pragma_update(None, "user_version", SUPPORTED_SCHEMA_VERSION)?;
        } else {
            if current == 1 {
                transaction.execute_batch(MIGRATION_2)?;
            }
            if current <= 2 {
                transaction.execute_batch(MIGRATION_3)?;
            }
            if current == 1 {
                transaction.execute(
                    "INSERT INTO reactor_meta (key, value) VALUES ('instance_id', ?1)",
                    [crate::ids::new_id()],
                )?;
            }
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
        if current < SUPPORTED_SCHEMA_VERSION {
            return Err(ReactorError::SchemaMigrationRequired {
                found: current,
                supported: SUPPORTED_SCHEMA_VERSION,
            });
        }
        Ok(())
    }

    pub fn instance_id(&self) -> Result<String> {
        self.ensure_supported_schema()?;
        Ok(self.connection.query_row(
            "SELECT value FROM reactor_meta WHERE key = 'instance_id'",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn snapshot(&self) -> Result<Snapshot> {
        self.ensure_supported_schema()?;

        let workspaces = query_workspaces(&self.connection)?;
        let runs = query_runs(&self.connection)?;
        let queued_runs = self.connection.query_row(
            "SELECT COUNT(*) FROM runs WHERE state = 'QUEUED'",
            [],
            |row| row.get(0),
        )?;
        let latest_event_cursor = self.connection.query_row(
            "SELECT COALESCE(MAX(cursor), 0) FROM audit_events",
            [],
            |row| row.get(0),
        )?;

        Ok(Snapshot {
            workspaces,
            runs,
            queued_runs,
            latest_event_cursor,
        })
    }

    pub fn events_after(&self, cursor: i64, limit: u32) -> Result<Vec<AuditEvent>> {
        self.ensure_supported_schema()?;

        let oldest: Option<i64> =
            self.connection
                .query_row("SELECT MIN(cursor) FROM audit_events", [], |row| row.get(0))?;
        // The cursor immediately before the oldest event can still resume safely.
        if cursor > 0 && oldest.is_some_and(|oldest| cursor < oldest - 1) {
            return Err(ReactorError::CursorExpired {
                requested: cursor,
                oldest: oldest.unwrap_or_default(),
            });
        }

        let mut statement = self.connection.prepare(
            "SELECT cursor, event_type, entity_type, entity_id, payload_json, occurred_at
             FROM audit_events WHERE cursor > ?1 ORDER BY cursor LIMIT ?2",
        )?;
        let rows = statement.query_map(params![cursor, limit], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<_, String>(4)?,
                row.get(5)?,
            ))
        })?;

        rows.map(|row| {
            let (cursor, event_type, entity_type, entity_id, payload_json, occurred_at) = row?;
            let payload = serde_json::from_str(&payload_json)
                .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
            Ok(AuditEvent {
                cursor,
                event_type,
                entity_type,
                entity_id,
                payload,
                occurred_at,
            })
        })
        .collect()
    }

    pub fn register_workspace(
        &mut self,
        id: &str,
        workspace: &CanonicalWorkspace,
        policy: &WorkspacePolicy,
        fingerprint: &str,
        idempotency_key: Option<&str>,
    ) -> Result<WorkspaceRecord> {
        let policy_json = serde_json::to_string(policy)
            .map_err(|error| ReactorError::InvalidInput(error.to_string()))?;
        let request = serde_json::to_vec(&serde_json::json!({
            "operation": "workspace_add",
            "id": id,
            "canonical_key": workspace.key,
            "policy": policy
        }))
        .map_err(|error| ReactorError::InvalidInput(error.to_string()))?;
        let operation = |transaction: &Transaction<'_>| {
            let record = insert_workspace(
                transaction,
                id,
                workspace,
                &policy_json,
                policy,
                fingerprint,
            )?;
            serde_json::to_value(record)
                .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))
        };
        let value = match idempotency_key {
            Some(key) => self.idempotent_write(key, &request, operation)?.0,
            None => self.write_transaction(operation)?,
        };
        serde_json::from_value(value)
            .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))
    }

    pub fn list_workspaces(&self) -> Result<Vec<WorkspaceRecord>> {
        self.ensure_supported_schema()?;
        let mut statement = self
            .connection
            .prepare("SELECT id FROM workspaces ORDER BY created_at, id")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter().map(|id| self.get_workspace(&id)).collect()
    }

    pub fn get_workspace(&self, id: &str) -> Result<WorkspaceRecord> {
        self.ensure_supported_schema()?;
        query_workspace(&self.connection, id)
    }

    pub fn record_agent_preflight(
        &mut self,
        workspace_id: &str,
        agent_kind: &str,
        preflight: &AgentPreflight,
        fingerprint: &str,
        idempotency_key: Option<&str>,
    ) -> Result<WorkspaceRecord> {
        let request = serde_json::to_vec(&serde_json::json!({
            "operation": "workspace_prepare",
            "workspace_id": workspace_id,
            "agent_kind": agent_kind,
            "fingerprint": fingerprint
        }))
        .map_err(|error| ReactorError::InvalidInput(error.to_string()))?;
        let operation = |transaction: &Transaction<'_>| {
            let record = save_agent_preflight(
                transaction,
                workspace_id,
                agent_kind,
                preflight,
                fingerprint,
            )?;
            serde_json::to_value(record)
                .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))
        };
        let value = match idempotency_key {
            Some(key) => self.idempotent_write(key, &request, operation)?.0,
            None => self.write_transaction(operation)?,
        };
        serde_json::from_value(value)
            .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))
    }

    pub fn mark_workspace_degraded(&mut self, id: &str, reason: &str) -> Result<WorkspaceRecord> {
        let now = crate::time::now_utc();
        self.write_transaction(|transaction| {
            let current = query_workspace(transaction, id)?;
            if current.status == "DEGRADED" {
                return Ok(current);
            }
            transaction.execute(
                "UPDATE workspaces SET status = 'DEGRADED', updated_at = ?1 WHERE id = ?2",
                params![now, id],
            )?;
            write_event(
                transaction,
                "WORKSPACE_DEGRADED",
                "workspace",
                id,
                serde_json::json!({ "reason": reason }),
                &now,
            )?;
            query_workspace(transaction, id)
        })
    }

    #[allow(dead_code)]
    pub fn require_ready_workspace_agent(
        &self,
        workspace_id: &str,
        agent_kind: &str,
    ) -> Result<()> {
        let workspace = self.get_workspace(workspace_id)?;
        let ready = workspace.status == "READY"
            && workspace.agents.iter().any(|agent| {
                agent.agent_kind == agent_kind
                    && agent.status == "READY"
                    && agent.policy_fingerprint.as_deref()
                        == Some(workspace.policy_fingerprint.as_str())
            });
        if !ready {
            return Err(ReactorError::AgentNotReady {
                workspace_id: workspace_id.to_owned(),
                agent: agent_kind.to_owned(),
                reason: "run preflight again".to_owned(),
            });
        }
        Ok(())
    }

    pub fn idempotent_write(
        &mut self,
        key: &str,
        request: &[u8],
        operation: impl FnOnce(&Transaction<'_>) -> Result<Value>,
    ) -> Result<(Value, bool)> {
        self.ensure_supported_schema()?;
        let request_hash = blake3::hash(request).to_hex().to_string();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT request_hash, response_json FROM idempotency_keys WHERE key = ?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        if let Some((stored_hash, stored_response)) = existing {
            if stored_hash != request_hash {
                return Err(ReactorError::IdempotencyConflict);
            }
            let response = serde_json::from_str(&stored_response)
                .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
            return Ok((response, true));
        }

        let response = operation(&transaction)?;
        transaction.execute(
            "INSERT INTO idempotency_keys (key, request_hash, response_json, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                key,
                request_hash,
                response.to_string(),
                crate::time::now_utc()
            ],
        )?;
        transaction.commit()?;
        Ok((response, false))
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
                        serde_json::json!({
                            "from": expected.as_str(),
                            "to": next.as_str()
                        })
                        .to_string(),
                        occurred_at
                    ],
                )?;
                prune_events(transaction)?;
            }

            Ok(changed == 1)
        })
    }
}

fn insert_workspace(
    transaction: &Transaction<'_>,
    id: &str,
    workspace: &CanonicalWorkspace,
    policy_json: &str,
    policy: &WorkspacePolicy,
    fingerprint: &str,
) -> Result<WorkspaceRecord> {
    let mut statement =
        transaction.prepare("SELECT id, canonical_key, policy_fingerprint FROM workspaces")?;
    let locations = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (registered_id, registered_key, registered_fingerprint) in locations {
        if registered_id == id
            && registered_key == workspace.key
            && registered_fingerprint == fingerprint
        {
            return query_workspace(transaction, id);
        }
        if registered_id == id || paths_overlap(&registered_key, &workspace.key) {
            return Err(ReactorError::WorkspaceOverlap {
                workspace_id: registered_id,
            });
        }
    }

    let now = crate::time::now_utc();
    transaction.execute(
        "INSERT INTO workspaces
         (id, canonical_path, canonical_key, status, policy_json, policy_fingerprint, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'REGISTERED', ?4, ?5, ?6, ?6)",
        params![id, workspace.display, workspace.key, policy_json, fingerprint, now],
    )?;
    write_event(
        transaction,
        "WORKSPACE_REGISTERED",
        "workspace",
        id,
        serde_json::json!({ "status": "REGISTERED", "policy": policy }),
        &now,
    )?;
    query_workspace(transaction, id)
}

fn save_agent_preflight(
    transaction: &Transaction<'_>,
    workspace_id: &str,
    agent_kind: &str,
    preflight: &AgentPreflight,
    fingerprint: &str,
) -> Result<WorkspaceRecord> {
    query_workspace(transaction, workspace_id)?;
    let now = crate::time::now_utc();
    let agent_status = if preflight.ready { "READY" } else { "DEGRADED" };
    let workspace_status = agent_status;
    let capability_json = serde_json::to_string(&preflight.capabilities)
        .map_err(|error| ReactorError::InvalidInput(error.to_string()))?;
    transaction.execute(
        "INSERT INTO workspace_agents
         (workspace_id, agent_kind, status, capability_json, policy_fingerprint, checked_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(workspace_id, agent_kind) DO UPDATE SET
           status = excluded.status,
           capability_json = excluded.capability_json,
           policy_fingerprint = excluded.policy_fingerprint,
           checked_at = excluded.checked_at",
        params![
            workspace_id,
            agent_kind,
            agent_status,
            capability_json,
            fingerprint,
            now
        ],
    )?;
    transaction.execute(
        "UPDATE workspaces SET status = ?1, policy_fingerprint = ?2, updated_at = ?3 WHERE id = ?4",
        params![workspace_status, fingerprint, now, workspace_id],
    )?;
    write_event(
        transaction,
        "AGENT_PREFLIGHT_COMPLETED",
        "workspace",
        workspace_id,
        serde_json::json!({
            "agent_kind": agent_kind,
            "status": agent_status,
            "reason": preflight.reason
        }),
        &now,
    )?;
    query_workspace(transaction, workspace_id)
}

fn query_workspace(connection: &Connection, id: &str) -> Result<WorkspaceRecord> {
    let row: Option<(String, String, String, String, String, String)> = connection
        .query_row(
            "SELECT canonical_path, status, policy_json, policy_fingerprint, created_at, updated_at
             FROM workspaces WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((canonical_path, status, policy_json, policy_fingerprint, created_at, updated_at)) =
        row
    else {
        return Err(ReactorError::WorkspaceNotFound(id.to_owned()));
    };
    let policy = serde_json::from_str(&policy_json)
        .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
    Ok(WorkspaceRecord {
        id: id.to_owned(),
        canonical_path,
        status,
        policy,
        policy_fingerprint,
        agents: query_workspace_agents(connection, id)?,
        created_at,
        updated_at,
    })
}

fn query_workspace_agents(
    connection: &Connection,
    workspace_id: &str,
) -> Result<Vec<WorkspaceAgentRecord>> {
    let mut statement = connection.prepare(
        "SELECT agent_kind, status, capability_json, policy_fingerprint, checked_at
         FROM workspace_agents WHERE workspace_id = ?1 ORDER BY agent_kind",
    )?;
    let rows = statement.query_map([workspace_id], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get::<_, String>(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    })?;
    rows.map(|row| {
        let (agent_kind, status, capability_json, policy_fingerprint, checked_at) = row?;
        let capabilities = serde_json::from_str(&capability_json)
            .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
        Ok(WorkspaceAgentRecord {
            agent_kind,
            status,
            capabilities,
            policy_fingerprint,
            checked_at,
        })
    })
    .collect()
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

fn configure_storage(connection: &Connection) -> Result<()> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

fn query_workspaces(connection: &Connection) -> Result<Vec<WorkspaceSnapshot>> {
    let mut statement =
        connection.prepare("SELECT id, status FROM workspaces ORDER BY created_at, id")?;
    let rows = statement.query_map([], |row| {
        Ok(WorkspaceSnapshot {
            id: row.get(0)?,
            status: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn query_runs(connection: &Connection) -> Result<Vec<RunSnapshot>> {
    let mut statement = connection.prepare(
        "SELECT id, workspace_id, state, agent_kind, ready_at, updated_at
         FROM runs
         WHERE state IN ('WAITING', 'QUEUED', 'PREPARING', 'RUNNING', 'REVIEW_REQUIRED')
         ORDER BY ready_at, created_at, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(RunSnapshot {
            id: row.get(0)?,
            workspace_id: row.get(1)?,
            state: row.get(2)?,
            agent_kind: row.get(3)?,
            ready_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn prune_events(transaction: &Transaction<'_>) -> Result<()> {
    // Keep the newest 100,000 events while also enforcing the age limit.
    transaction.execute(
        "DELETE FROM audit_events
         WHERE julianday(occurred_at) < julianday('now', '-90 days')
            OR cursor < COALESCE((
                SELECT cursor FROM audit_events
                ORDER BY cursor DESC LIMIT 1 OFFSET ?1
            ), 0)",
        [MAX_EVENT_COUNT - 1],
    )?;
    Ok(())
}

fn write_event(
    transaction: &Transaction<'_>,
    event_type: &str,
    entity_type: &str,
    entity_id: &str,
    payload: Value,
    occurred_at: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO audit_events (event_type, entity_type, entity_id, payload_json, occurred_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event_type,
            entity_type,
            entity_id,
            payload.to_string(),
            occurred_at
        ],
    )?;
    prune_events(transaction)
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

const MIGRATION_1: &str = include_str!("../migrations/0001_initial.sql");
const MIGRATION_2: &str = include_str!("../migrations/0002_instance_id.sql");
const MIGRATION_3: &str = include_str!("../migrations/0003_workspace_policy.sql");
