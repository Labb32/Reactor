use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::{Store, write_event};
use crate::{
    domain::RunState,
    error::{ReactorError, Result},
    plan::PlanLimits,
    run::{AttemptRecord, BeginAttempt, ExecutionRun, RunRecord, RunResult},
    workspace::WorkspacePolicy,
};

const MAX_EVENT_CHANGED_FILES: usize = 100;

impl Store {
    pub fn record_log_deletion(&mut self, run_id: &str, deleted_files: usize) -> Result<()> {
        self.write_transaction(|transaction| {
            query_run(transaction, run_id)?;
            let now = crate::time::now_utc();
            super::write_event(
                transaction,
                "RUN_LOGS_DELETED",
                "run",
                run_id,
                serde_json::json!({ "deleted_files": deleted_files }),
                &now,
            )?;
            Ok(())
        })
    }

    pub fn get_run(&self, run_id: &str) -> Result<RunRecord> {
        self.ensure_supported_schema()?;
        query_run(&self.connection, run_id)
    }

    pub fn list_runs(&self) -> Result<Vec<RunRecord>> {
        self.ensure_supported_schema()?;
        let mut statement = self
            .connection
            .prepare("SELECT id FROM runs ORDER BY julianday(created_at), rowid")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| query_run(&self.connection, &id))
            .collect()
    }

    pub fn list_runs_filtered(
        &self,
        status: Option<&str>,
        workspace_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<RunRecord>> {
        self.ensure_supported_schema()?;
        let mut statement = self.connection.prepare(
            "SELECT id FROM runs
             WHERE (?1 IS NULL OR state = ?1)
               AND (?2 IS NULL OR workspace_id = ?2)
             ORDER BY julianday(created_at) DESC, rowid DESC
             LIMIT ?3",
        )?;
        let ids = statement
            .query_map(
                params![status, workspace_id, limit.map_or(i64::MAX, i64::from)],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| query_run(&self.connection, &id))
            .collect()
    }

    pub fn find_runs_by_suffix(&self, suffix: &str) -> Result<Vec<RunRecord>> {
        self.ensure_supported_schema()?;
        let length = i64::try_from(suffix.len())
            .map_err(|_| ReactorError::InvalidArgument("Run code is too long".to_owned()))?;
        let mut statement = self.connection.prepare(
            "SELECT id FROM runs
             WHERE length(id) >= ?1 AND substr(id, length(id) - ?1 + 1) = ?2
             ORDER BY julianday(created_at) DESC, rowid DESC
             LIMIT 2",
        )?;
        let ids = statement
            .query_map(params![length, suffix], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| query_run(&self.connection, &id))
            .collect()
    }

    pub(crate) fn execution_run(&self, run_id: &str) -> Result<ExecutionRun> {
        self.ensure_supported_schema()?;
        let policy_json: Option<String> = self
            .connection
            .query_row(
                "SELECT policy_snapshot_json FROM runs WHERE id = ?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(policy_json) = policy_json else {
            return Err(ReactorError::RunNotFound(run_id.to_owned()));
        };
        let policy = serde_json::from_str::<WorkspacePolicy>(&policy_json)
            .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
        Ok(ExecutionRun {
            record: query_run(&self.connection, run_id)?,
            policy,
        })
    }

    pub fn cancel_run(&mut self, run_id: &str, idempotency_key: Option<&str>) -> Result<RunRecord> {
        let request = serde_json::to_vec(&serde_json::json!({
            "operation": "run_cancel",
            "run_id": run_id
        }))
        .map_err(|error| ReactorError::InvalidInput(error.to_string()))?;
        let operation = |transaction: &Transaction<'_>| cancel(transaction, run_id);
        match idempotency_key {
            Some(key) => self.idempotent_write(key, &request, operation),
            None => self.write_transaction(operation),
        }
    }

    pub fn retry_run(&mut self, run_id: &str, idempotency_key: Option<&str>) -> Result<RunRecord> {
        let request = serde_json::to_vec(&serde_json::json!({
            "operation": "run_retry",
            "run_id": run_id
        }))
        .map_err(|error| ReactorError::InvalidInput(error.to_string()))?;
        let operation = |transaction: &Transaction<'_>| retry(transaction, run_id);
        match idempotency_key {
            Some(key) => self.idempotent_write(key, &request, operation),
            None => self.write_transaction(operation),
        }
    }

    pub fn clear_run(&mut self, run_id: &str) -> Result<RunRecord> {
        self.write_transaction(|transaction| clear_run(transaction, run_id))
    }

    pub(crate) fn begin_attempt(
        &mut self,
        run_id: &str,
        attempt_id: &str,
        log_path: &str,
    ) -> Result<BeginAttempt> {
        self.write_transaction(|transaction| {
            let run = query_run(transaction, run_id)?;
            if run.state != RunState::Preparing.as_str() {
                return Err(invalid_transition(&run.state, RunState::Preparing));
            }
            if run.cancel_requested_at.is_some() {
                return Ok(BeginAttempt::CancelRequested);
            }
            let attempts: u32 = transaction.query_row(
                "SELECT COUNT(*) FROM attempts WHERE run_id = ?1",
                [run_id],
                |row| row.get(0),
            )?;
            if attempts >= run.limits.max_attempts {
                return Ok(BeginAttempt::LimitReached);
            }
            let attempt = AttemptRecord {
                id: attempt_id.to_owned(),
                attempt_number: attempts + 1,
                process_id: None,
                process_started_at: None,
                started_at: crate::time::now_utc(),
                ended_at: None,
                exit_code: None,
                termination_reason: None,
                token_usage: None,
                log_path: log_path.to_owned(),
            };
            transaction.execute(
                "INSERT INTO attempts
                 (id, run_id, attempt_number, started_at, log_path)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    attempt.id,
                    run_id,
                    attempt.attempt_number,
                    attempt.started_at,
                    attempt.log_path
                ],
            )?;
            write_event(
                transaction,
                "RUN_ATTEMPT_CREATED",
                "run",
                run_id,
                serde_json::json!({
                    "attempt_id": attempt.id,
                    "attempt_number": attempt.attempt_number
                }),
                &attempt.started_at,
            )?;
            Ok(BeginAttempt::Started(attempt))
        })
    }

    pub(crate) fn mark_run_running(
        &mut self,
        run_id: &str,
        attempt_id: &str,
        process_id: u32,
    ) -> Result<bool> {
        self.write_transaction(|transaction| {
            let now = crate::time::now_utc();
            let changed = transaction.execute(
                "UPDATE runs SET state = 'RUNNING', updated_at = ?1
                 WHERE id = ?2 AND state = 'PREPARING' AND cancel_requested_at IS NULL",
                params![now, run_id],
            )?;
            if changed == 0 {
                return Ok(false);
            }
            let attempt_changed = transaction.execute(
                "UPDATE attempts SET process_id = ?1, process_started_at = ?2
                 WHERE id = ?3 AND run_id = ?4 AND ended_at IS NULL",
                params![process_id, now, attempt_id, run_id],
            )?;
            if attempt_changed != 1 {
                return Err(ReactorError::InvalidStoredData(
                    "active attempt is missing".to_owned(),
                ));
            }
            write_event(
                transaction,
                "RUN_STATE_CHANGED",
                "run",
                run_id,
                serde_json::json!({ "from": "PREPARING", "to": "RUNNING" }),
                &now,
            )?;
            Ok(true)
        })
    }

    pub(crate) fn cancellation_requested(&self, run_id: &str) -> Result<bool> {
        self.ensure_supported_schema()?;
        self.connection
            .query_row(
                "SELECT cancel_requested_at IS NOT NULL FROM runs WHERE id = ?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| ReactorError::RunNotFound(run_id.to_owned()))
    }

    pub(crate) fn heartbeat_run(&self, run_id: &str, owner_id: &str) -> Result<bool> {
        self.ensure_supported_schema()?;
        Ok(self.connection.execute(
            "UPDATE leases SET expires_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?3)
             WHERE resource_type = 'run' AND resource_id = ?1 AND owner_id = ?2",
            params![run_id, owner_id, super::RUN_LEASE_DURATION],
        )? == 1)
    }

    pub(crate) fn claim_run_lease(&self, run_id: &str, owner_id: &str) -> Result<bool> {
        self.ensure_supported_schema()?;
        let now = crate::time::now_utc();
        Ok(self.connection.execute(
            "INSERT INTO leases (resource_type, resource_id, owner_id, acquired_at, expires_at)
             VALUES ('run', ?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?4))
             ON CONFLICT(resource_type, resource_id) DO UPDATE SET
                owner_id = excluded.owner_id,
                acquired_at = excluded.acquired_at,
                expires_at = excluded.expires_at
             WHERE leases.owner_id = excluded.owner_id
                OR julianday(leases.expires_at) <= julianday('now')",
            params![run_id, owner_id, now, super::RUN_LEASE_DURATION],
        )? == 1)
    }

    pub(crate) fn release_run(&self, run_id: &str, owner_id: &str) -> Result<()> {
        self.ensure_supported_schema()?;
        self.connection.execute(
            "DELETE FROM leases
             WHERE resource_type = 'run' AND resource_id = ?1 AND owner_id = ?2",
            params![run_id, owner_id],
        )?;
        Ok(())
    }

    pub(crate) fn finish_run(
        &mut self,
        run_id: &str,
        attempt_id: Option<&str>,
        expected: RunState,
        next: RunState,
        exit_code: Option<i32>,
        reason: &str,
    ) -> Result<RunRecord> {
        if !next.is_terminal() || !expected.can_transition_to(next) {
            return Err(ReactorError::InvalidRunTransition {
                from: expected.to_string(),
                to: next.to_string(),
            });
        }
        self.write_transaction(|transaction| {
            let current = query_run(transaction, run_id)?;
            if current.state != expected.as_str() {
                return Err(invalid_transition(&current.state, next));
            }
            let (next, reason) = if current.cancel_requested_at.is_some()
                && matches!(expected, RunState::Preparing | RunState::Running)
            {
                (RunState::Cancelled, "cancel_requested")
            } else {
                (next, reason)
            };
            let now = crate::time::now_utc();
            let changed = transaction.execute(
                "UPDATE runs SET state = ?1, updated_at = ?2 WHERE id = ?3 AND state = ?4",
                params![next.as_str(), now, run_id, expected.as_str()],
            )?;
            if changed != 1 {
                let current = query_run(transaction, run_id)?;
                return Err(invalid_transition(&current.state, next));
            }
            if let Some(attempt_id) = attempt_id {
                let attempt_changed = transaction.execute(
                    "UPDATE attempts
                     SET ended_at = ?1, exit_code = ?2, termination_reason = ?3
                     WHERE id = ?4 AND run_id = ?5 AND ended_at IS NULL",
                    params![now, exit_code, reason, attempt_id, run_id],
                )?;
                if attempt_changed != 1 {
                    return Err(ReactorError::InvalidStoredData(
                        "active attempt is missing".to_owned(),
                    ));
                }
            }
            write_event(
                transaction,
                "RUN_STATE_CHANGED",
                "run",
                run_id,
                serde_json::json!({
                    "from": expected.as_str(),
                    "to": next.as_str(),
                    "reason": reason
                }),
                &now,
            )?;
            super::schedules::finish_recurring_run(
                transaction,
                current.occurrence_id.as_deref(),
                next == RunState::Succeeded,
                now.parse::<jiff::Timestamp>()
                    .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?,
            )?;
            query_run(transaction, run_id)
        })
    }

    pub(crate) fn record_attempt_usage(
        &self,
        run_id: &str,
        attempt_id: &str,
        tokens: u64,
    ) -> Result<()> {
        self.ensure_supported_schema()?;
        let usage = serde_json::json!({ "tokens": tokens }).to_string();
        let changed = self.connection.execute(
            "UPDATE attempts SET usage_json = ?1
             WHERE id = ?2 AND run_id = ?3 AND ended_at IS NULL",
            params![usage, attempt_id, run_id],
        )?;
        if changed != 1 {
            return Err(ReactorError::InvalidStoredData(
                "active attempt is missing".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn record_run_result(
        &mut self,
        run_id: &str,
        result: &RunResult,
    ) -> Result<RunRecord> {
        self.write_transaction(|transaction| {
            let run = query_run(transaction, run_id)?;
            if run.result.is_some() {
                transaction.execute(
                    "DELETE FROM leases WHERE resource_type = 'run' AND resource_id = ?1",
                    [run_id],
                )?;
                return Ok(run);
            }
            let state = parse_state(&run.state)?;
            if !state.is_terminal() {
                return Err(ReactorError::InvalidArgument(
                    "run result requires a terminal run".to_owned(),
                ));
            }
            let summary = serde_json::to_string(result)
                .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
            transaction.execute(
                "DELETE FROM run_results
                 WHERE run_id IN (
                    SELECT id FROM runs WHERE workspace_id = ?1 AND id <> ?2
                 )",
                params![run.workspace_id, run_id],
            )?;
            transaction.execute(
                "INSERT INTO run_results
                 (run_id, completed_at, manifest_path, diff_path, summary_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(run_id) DO UPDATE SET
                    completed_at = excluded.completed_at,
                    manifest_path = excluded.manifest_path,
                    diff_path = excluded.diff_path,
                    summary_json = excluded.summary_json",
                params![
                    run_id,
                    result.completed_at,
                    result.manifest_path,
                    result.diff_path,
                    summary
                ],
            )?;
            transaction.execute(
                "DELETE FROM leases WHERE resource_type = 'run' AND resource_id = ?1",
                [run_id],
            )?;
            let changed_files = result
                .changed_files
                .iter()
                .take(MAX_EVENT_CHANGED_FILES)
                .map(|change| {
                    serde_json::json!({
                        "root": change.root,
                        "path": change.path,
                        "change": change.change
                    })
                })
                .collect::<Vec<_>>();
            write_event(
                transaction,
                "RUN_RESULT_RECORDED",
                "run",
                run_id,
                serde_json::json!({
                    "state": run.state,
                    "changed_file_count": result.changed_files.len(),
                    "changed_files": changed_files,
                    "omitted_file_count": result.changed_files.len().saturating_sub(MAX_EVENT_CHANGED_FILES)
                }),
                &result.completed_at,
            )?;
            query_run(transaction, run_id)
        })
    }

    pub(crate) fn recover_interrupted_runs(&mut self, owner_id: &str) -> Result<Vec<RunRecord>> {
        self.write_transaction(|transaction| {
            let mut statement = transaction.prepare(
                "SELECT runs.id FROM runs
                 LEFT JOIN leases ON leases.resource_type = 'run' AND leases.resource_id = runs.id
                 WHERE runs.state IN ('PREPARING', 'RUNNING')
                   AND (leases.resource_id IS NULL OR julianday(leases.expires_at) <= julianday('now'))
                 ORDER BY julianday(runs.updated_at), runs.rowid",
            )?;
            let ids = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);
            let mut recovered = Vec::with_capacity(ids.len());
            for run_id in ids {
                let run = query_run(transaction, &run_id)?;
                let now = crate::time::now_utc();
                transaction.execute(
                    "UPDATE runs SET state = 'REVIEW_REQUIRED', updated_at = ?1 WHERE id = ?2",
                    params![now, run_id],
                )?;
                transaction.execute(
                    "UPDATE attempts SET ended_at = ?1, termination_reason = 'daemon_interrupted'
                     WHERE run_id = ?2 AND ended_at IS NULL",
                    params![now, run_id],
                )?;
                transaction.execute(
                    "INSERT INTO leases
                     (resource_type, resource_id, owner_id, acquired_at, expires_at)
                     VALUES ('run', ?1, ?2, ?3,
                             strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?4))
                     ON CONFLICT(resource_type, resource_id) DO UPDATE SET
                        owner_id = excluded.owner_id,
                        acquired_at = excluded.acquired_at,
                        expires_at = excluded.expires_at",
                    params![run_id, owner_id, now, super::RUN_LEASE_DURATION],
                )?;
                write_event(
                    transaction,
                    "RUN_STATE_CHANGED",
                    "run",
                    &run_id,
                    serde_json::json!({
                        "from": run.state,
                        "to": "REVIEW_REQUIRED",
                        "reason": "daemon_interrupted"
                    }),
                    &now,
                )?;
                super::schedules::finish_recurring_run(
                    transaction,
                    run.occurrence_id.as_deref(),
                    false,
                    now.parse::<jiff::Timestamp>()
                        .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?,
                )?;
                recovered.push(query_run(transaction, &run_id)?);
            }
            Ok(recovered)
        })
    }
}

pub(super) fn cancel(transaction: &Transaction<'_>, run_id: &str) -> Result<RunRecord> {
    let run = query_run(transaction, run_id)?;
    let state = parse_state(&run.state)?;
    let now = crate::time::now_utc();
    match state {
        RunState::Waiting | RunState::Queued => {
            let changed = transaction.execute(
                "UPDATE runs SET state = 'CANCELLED', cancel_requested_at = ?1, updated_at = ?1
                 WHERE id = ?2 AND state = ?3",
                params![now, run_id, state.as_str()],
            )?;
            if changed != 1 {
                return Err(invalid_transition(&run.state, RunState::Cancelled));
            }
            if let Some(occurrence_id) = &run.occurrence_id {
                transaction.execute(
                    "UPDATE schedule_occurrences SET state = 'CANCELLED'
                     WHERE id = ?1 AND state IN ('WAITING', 'QUEUED')",
                    [occurrence_id],
                )?;
            }
            write_event(
                transaction,
                "RUN_STATE_CHANGED",
                "run",
                run_id,
                serde_json::json!({
                    "from": state.as_str(),
                    "to": "CANCELLED",
                    "reason": "cancel_requested"
                }),
                &now,
            )?;
            super::schedules::finish_recurring_run(
                transaction,
                run.occurrence_id.as_deref(),
                false,
                now.parse::<jiff::Timestamp>()
                    .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?,
            )?;
        }
        RunState::Preparing | RunState::Running => {
            if run.cancel_requested_at.is_none() {
                transaction.execute(
                    "UPDATE runs SET cancel_requested_at = ?1, updated_at = ?1
                     WHERE id = ?2 AND state = ?3 AND cancel_requested_at IS NULL",
                    params![now, run_id, state.as_str()],
                )?;
                write_event(
                    transaction,
                    "RUN_CANCEL_REQUESTED",
                    "run",
                    run_id,
                    serde_json::json!({ "state": state.as_str() }),
                    &now,
                )?;
            }
        }
        RunState::Cancelled => return Ok(run),
        _ => return Err(invalid_transition(&run.state, RunState::Cancelled)),
    }
    query_run(transaction, run_id)
}

fn retry(transaction: &Transaction<'_>, run_id: &str) -> Result<RunRecord> {
    let source = query_run(transaction, run_id)?;
    let state = parse_state(&source.state)?;
    if !matches!(
        state,
        RunState::Failed | RunState::Cancelled | RunState::LimitReached | RunState::ReviewRequired
    ) {
        return Err(ReactorError::InvalidArgument(format!(
            "run state {} cannot be retried",
            state.as_str()
        )));
    }
    super::plans::query_plan(transaction, &source.plan_id, Some(source.plan_version))?;
    let already_retried: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM runs WHERE retry_of = ?1)",
        [run_id],
        |row| row.get(0),
    )?;
    if already_retried {
        return Err(ReactorError::InvalidArgument(
            "run has already been retried".to_owned(),
        ));
    }
    let used_attempts: u32 = transaction.query_row(
        "SELECT COUNT(*) FROM attempts WHERE run_id = ?1",
        [run_id],
        |row| row.get(0),
    )?;
    if used_attempts >= source.limits.max_attempts {
        return Err(ReactorError::InvalidArgument(
            "run attempt limit has been reached".to_owned(),
        ));
    }
    let mut remaining_limits = source.limits.clone();
    remaining_limits.max_attempts -= used_attempts;
    let limits_json = serde_json::to_string(&remaining_limits)
        .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
    let now = crate::time::now_utc();
    let new_run_id = allocate_retry_id(transaction, run_id)?;
    let changed = transaction.execute(
        "INSERT INTO runs
         (id, workspace_id, plan_id, plan_version, occurrence_id, retry_of, state, agent_kind,
          provider, local_endpoint, policy_snapshot_json, limits_snapshot_json,
          ready_at, created_at, updated_at)
         SELECT ?1, workspace_id, plan_id, plan_version, NULL, ?4, 'QUEUED', agent_kind,
                provider, local_endpoint, policy_snapshot_json, ?2,
                ?3, ?3, ?3
         FROM runs WHERE id = ?4 AND state = ?5",
        params![&new_run_id, limits_json, now, run_id, state.as_str()],
    )?;
    if changed != 1 {
        return Err(ReactorError::InvalidStoredData(
            "retry source changed unexpectedly".to_owned(),
        ));
    }
    write_event(
        transaction,
        "RUN_QUEUED",
        "run",
        &new_run_id,
        serde_json::json!({ "retry_of": run_id, "workspace_id": source.workspace_id }),
        &now,
    )?;
    query_run(transaction, &new_run_id)
}

fn allocate_retry_id(transaction: &Transaction<'_>, source_id: &str) -> Result<String> {
    let base = format!("{source_id}-retry");
    for number in 1_u32..=u32::MAX {
        let id = format!("{base}-{number}");
        if !run_id_reserved(transaction, &id)? {
            return Ok(id);
        }
    }
    Err(ReactorError::InvalidStoredData(
        "could not allocate a retry Run ID".to_owned(),
    ))
}

pub(super) fn run_id_reserved(transaction: &Transaction<'_>, run_id: &str) -> Result<bool> {
    Ok(transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM runs WHERE id = ?1
            UNION ALL
            SELECT 1 FROM audit_events WHERE entity_type = 'run' AND entity_id = ?1
        )",
        [run_id],
        |row| row.get(0),
    )?)
}

fn clear_run(transaction: &Transaction<'_>, run_id: &str) -> Result<RunRecord> {
    let run = query_run(transaction, run_id)?;
    let state = parse_state(&run.state)?;
    if !state.can_clear(true) {
        return Err(ReactorError::InvalidArgument(
            "only a completed Run can be cleared".to_owned(),
        ));
    }
    let now = crate::time::now_utc();
    transaction.execute(
        "UPDATE runs SET retry_of = NULL WHERE retry_of = ?1",
        [run_id],
    )?;
    transaction.execute(
        "DELETE FROM leases WHERE resource_type = 'run' AND resource_id = ?1",
        [run_id],
    )?;
    transaction.execute("DELETE FROM runs WHERE id = ?1", [run_id])?;
    write_event(
        transaction,
        "RUN_DELETED",
        "run",
        run_id,
        serde_json::json!({ "state": &run.state, "workspace_id": &run.workspace_id }),
        &now,
    )?;
    Ok(run)
}

fn query_run(connection: &Connection, run_id: &str) -> Result<RunRecord> {
    struct StoredRun {
        workspace_id: String,
        plan_id: String,
        plan_version: u32,
        occurrence_id: Option<String>,
        retry_of: Option<String>,
        state: String,
        agent_kind: String,
        provider: Option<String>,
        local_endpoint: Option<String>,
        limits_json: String,
        ready_at: String,
        cancel_requested_at: Option<String>,
        created_at: String,
        updated_at: String,
    }
    let stored: Option<StoredRun> = connection
        .query_row(
            "SELECT workspace_id, plan_id, plan_version, occurrence_id, retry_of, state, agent_kind,
                    provider, local_endpoint, limits_snapshot_json, ready_at,
                    cancel_requested_at, created_at, updated_at
             FROM runs WHERE id = ?1",
            [run_id],
            |row| {
                Ok(StoredRun {
                    workspace_id: row.get(0)?,
                    plan_id: row.get(1)?,
                    plan_version: row.get(2)?,
                    occurrence_id: row.get(3)?,
                    retry_of: row.get(4)?,
                    state: row.get(5)?,
                    agent_kind: row.get(6)?,
                    provider: row.get(7)?,
                    local_endpoint: row.get(8)?,
                    limits_json: row.get(9)?,
                    ready_at: row.get(10)?,
                    cancel_requested_at: row.get(11)?,
                    created_at: row.get(12)?,
                    updated_at: row.get(13)?,
                })
            },
        )
        .optional()?;
    let Some(stored) = stored else {
        return Err(ReactorError::RunNotFound(run_id.to_owned()));
    };
    let limits = serde_json::from_str::<PlanLimits>(&stored.limits_json)
        .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
    Ok(RunRecord {
        id: run_id.to_owned(),
        workspace_id: stored.workspace_id,
        plan_id: stored.plan_id,
        plan_version: stored.plan_version,
        occurrence_id: stored.occurrence_id,
        retry_of: stored.retry_of,
        state: stored.state,
        agent_kind: stored.agent_kind,
        provider: stored.provider,
        local_endpoint: stored.local_endpoint,
        limits,
        ready_at: stored.ready_at,
        cancel_requested_at: stored.cancel_requested_at,
        attempt: query_attempt(connection, run_id)?,
        result: query_result(connection, run_id)?,
        created_at: stored.created_at,
        updated_at: stored.updated_at,
    })
}

fn query_result(connection: &Connection, run_id: &str) -> Result<Option<RunResult>> {
    let stored: Option<(String, String, String, String)> = connection
        .query_row(
            "SELECT completed_at, manifest_path, diff_path, summary_json
             FROM run_results WHERE run_id = ?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    stored
        .map(|(completed_at, manifest_path, diff_path, summary)| {
            let mut result = serde_json::from_str::<RunResult>(&summary)
                .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
            result.completed_at = completed_at;
            result.manifest_path = manifest_path;
            result.diff_path = diff_path;
            Ok(result)
        })
        .transpose()
}

fn query_attempt(connection: &Connection, run_id: &str) -> Result<Option<AttemptRecord>> {
    Ok(connection
        .query_row(
            "SELECT id, attempt_number, process_id, process_started_at, started_at,
                    ended_at, exit_code, termination_reason,
                    json_extract(usage_json, '$.tokens'), log_path
             FROM attempts WHERE run_id = ?1 ORDER BY attempt_number DESC LIMIT 1",
            [run_id],
            map_attempt,
        )
        .optional()?)
}

fn map_attempt(row: &Row<'_>) -> rusqlite::Result<AttemptRecord> {
    Ok(AttemptRecord {
        id: row.get(0)?,
        attempt_number: row.get(1)?,
        process_id: row.get(2)?,
        process_started_at: row.get(3)?,
        started_at: row.get(4)?,
        ended_at: row.get(5)?,
        exit_code: row.get(6)?,
        termination_reason: row.get(7)?,
        token_usage: row
            .get::<_, Option<i64>>(8)?
            .and_then(|value| u64::try_from(value).ok()),
        log_path: row.get(9)?,
    })
}

fn parse_state(value: &str) -> Result<RunState> {
    RunState::parse(value)
        .ok_or_else(|| ReactorError::InvalidStoredData(format!("unknown run state: {value}")))
}

fn invalid_transition(from: &str, to: RunState) -> ReactorError {
    ReactorError::InvalidRunTransition {
        from: from.to_owned(),
        to: to.to_string(),
    }
}
