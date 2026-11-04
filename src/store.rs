use std::{path::Path, time::Duration};

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::error::{ReactorError, Result};

mod plans;
mod queue;
mod runs;
mod schedules;
mod workspaces;

pub const SUPPORTED_SCHEMA_VERSION: u32 = 10;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_EVENT_COUNT: i64 = 100_000;
const RUN_LEASE_DURATION: &str = "+60 seconds";

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
    execution_fence: Option<(String, String)>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        configure_connection(&connection)?;
        Ok(Self { connection, execution_fence: None })
    }

    pub fn open_initialized(path: &Path) -> Result<Self> {
        // Read commands must not create an empty database.
        if !path.exists() {
            return Err(ReactorError::UninitializedDatabase);
        }
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        configure_connection(&connection)?;
        let store = Self { connection, execution_fence: None };
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

        if current == 9 {
            let occupied: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM leases WHERE julianday(expires_at) > julianday('now'))",
                [], |row| row.get(0),
            )?;
            if occupied { return Err(ReactorError::InvalidInput("stop active executions before migration".to_owned())); }
            if let Some(path) = self.connection.path().filter(|path| !path.is_empty()) {
                let backup = format!("{path}.pre-v10-{}.sqlite3", crate::ids::new_id());
                self.connection.execute("VACUUM INTO ?1", [&backup])?;
                crate::permissions::restrict_file(Path::new(&backup))
                    .map_err(|source| crate::error::io_error(&backup, source))?;
            }
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Exclusive)?;

        if current == 0 {
            transaction.execute_batch(INITIAL_SCHEMA)?;
            transaction.execute_batch(include_str!("../migrations/0002_tasks.sql"))?;
            insert_instance_id(&transaction)?;
        } else if current == 9 {
            transaction.execute_batch(include_str!("../migrations/0002_tasks.sql"))?;
        } else {
            return Err(ReactorError::SchemaMigrationRequired {
                found: current,
                supported: SUPPORTED_SCHEMA_VERSION,
            });
        }
        transaction.pragma_update(None, "user_version", SUPPORTED_SCHEMA_VERSION)?;

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

    pub(super) fn idempotent_write<T: DeserializeOwned + Serialize>(
        &mut self,
        key: &str,
        request: &[u8],
        operation: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        self.ensure_supported_schema()?;
        validate_idempotency_key(key)?;
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
            return Ok(response);
        }

        let response = operation(&transaction)?;
        let response_json = serde_json::to_string(&response)
            .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
        transaction.execute(
            "INSERT INTO idempotency_keys (key, request_hash, response_json, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![key, request_hash, response_json, crate::time::now_utc()],
        )?;
        transaction.commit()?;
        Ok(response)
    }

    pub(crate) fn idempotent_response<T: DeserializeOwned>(
        &self,
        key: &str,
        request: &[u8],
    ) -> Result<Option<T>> {
        self.ensure_supported_schema()?;
        validate_idempotency_key(key)?;
        let request_hash = blake3::hash(request).to_hex().to_string();
        let existing: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT request_hash, response_json FROM idempotency_keys WHERE key = ?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((stored_hash, response)) = existing else {
            return Ok(None);
        };
        if stored_hash != request_hash {
            return Err(ReactorError::IdempotencyConflict);
        }
        serde_json::from_str(&response)
            .map(Some)
            .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))
    }

    pub(super) fn write_transaction<T>(
        &mut self,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        self.ensure_supported_schema()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((run_id, owner_id)) = &self.execution_fence {
            let owned: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM leases WHERE resource_type = 'run'
                 AND resource_id = ?1 AND owner_id = ?2
                 AND julianday(expires_at) > julianday('now'))",
                params![run_id, owner_id], |row| row.get(0),
            )?;
            if !owned {
                return Err(ReactorError::InvalidStoredData("run ownership lost".to_owned()));
            }
        }
        let result = operation(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn fence_execution(&mut self, owner: Option<(&str, &str)>) {
        self.execution_fence = owner.map(|(run, owner)| (run.to_owned(), owner.to_owned()));
    }
}

fn insert_instance_id(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute(
        "INSERT INTO reactor_meta (key, value) VALUES ('instance_id', ?1)",
        [crate::ids::new_id()],
    )?;
    Ok(())
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

pub(super) fn write_event(
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

fn reject_newer_schema(found: u32) -> Result<()> {
    if found > SUPPORTED_SCHEMA_VERSION {
        return Err(ReactorError::UnsupportedSchema {
            found,
            supported: SUPPORTED_SCHEMA_VERSION,
        });
    }
    Ok(())
}

fn validate_idempotency_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
        return Err(ReactorError::InvalidArgument(
            "idempotency key must contain 1 to 128 bytes without control characters".to_owned(),
        ));
    }
    Ok(())
}

const INITIAL_SCHEMA: &str = include_str!("../migrations/0001_schema.sql");
