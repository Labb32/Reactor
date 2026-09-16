use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use super::{Store, prune_events};
use crate::{
    config::ConcurrencyConfig,
    domain::PlanStatus,
    error::{ReactorError, Result},
    schedule::{
        OccurrenceRecord, PreparedSchedule, QueueRun, ScheduleInput, ScheduleRecord, ScheduleRule,
        deadline,
    },
};

#[derive(Deserialize, Serialize)]
struct ScheduleGate {
    start_deadline_seconds: u64,
    local_endpoint: Option<String>,
}

impl Store {
    pub fn create_schedule(
        &mut self,
        input: &ScheduleInput,
        prepared: &PreparedSchedule,
        idempotency_key: Option<&str>,
    ) -> Result<ScheduleRecord> {
        let id = crate::ids::new_id();
        let request = request_bytes("schedule_create", None, input)?;
        let operation =
            |transaction: &Transaction<'_>| insert_schedule(transaction, &id, input, prepared);
        match idempotency_key {
            Some(key) => self.idempotent_write(key, &request, operation),
            None => self.write_transaction(operation),
        }
    }

    pub fn get_schedule(&self, schedule_id: &str) -> Result<ScheduleRecord> {
        self.ensure_supported_schema()?;
        query_schedule(&self.connection, schedule_id)
    }

    pub fn list_schedules(&self) -> Result<Vec<ScheduleRecord>> {
        self.ensure_supported_schema()?;
        let mut statement = self
            .connection
            .prepare("SELECT id FROM schedules ORDER BY julianday(created_at), id")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| query_schedule(&self.connection, &id))
            .collect()
    }

    pub fn pause_schedule(
        &mut self,
        schedule_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<ScheduleRecord> {
        transition_schedule(self, schedule_id, "ACTIVE", "PAUSED", idempotency_key)
    }

    pub fn resume_schedule(
        &mut self,
        schedule_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<ScheduleRecord> {
        transition_schedule(self, schedule_id, "PAUSED", "ACTIVE", idempotency_key)
    }

    pub fn trigger_schedule(
        &mut self,
        schedule_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<ScheduleRecord> {
        let request = request_bytes::<()>("schedule_trigger", Some(schedule_id), &())?;
        let operation = |transaction: &Transaction<'_>| trigger(transaction, schedule_id);
        match idempotency_key {
            Some(key) => self.idempotent_write(key, &request, operation),
            None => self.write_transaction(operation),
        }
    }

    pub fn refresh_due_schedules(&mut self) -> Result<u32> {
        self.write_transaction(refresh_due)
    }

    pub fn queue(&self, config: &ConcurrencyConfig) -> Result<Vec<QueueRun>> {
        self.ensure_supported_schema()?;
        evaluate_queue(&self.connection, config)
    }

    #[allow(dead_code)]
    pub fn claim_next_run(&mut self, config: &ConcurrencyConfig) -> Result<Option<QueueRun>> {
        self.write_transaction(|transaction| {
            let selected = evaluate_queue(transaction, config)?
                .into_iter()
                .filter(|run| run.eligible)
                .min_by_key(|run| run.dispatch_order);
            let Some(mut run) = selected else {
                return Ok(None);
            };
            let now = crate::time::now_utc();
            let changed = transaction.execute(
                "UPDATE runs SET state = 'PREPARING', updated_at = ?1
                 WHERE id = ?2 AND state = 'QUEUED'",
                params![now, run.id],
            )?;
            if changed == 0 {
                return Ok(None);
            }
            transaction.execute(
                "INSERT INTO reactor_meta (key, value) VALUES ('queue_last_workspace', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [&run.workspace_id],
            )?;
            write_event(
                transaction,
                "RUN_STATE_CHANGED",
                "run",
                &run.id,
                serde_json::json!({ "from": "QUEUED", "to": "PREPARING" }),
                &now,
            )?;
            run.state = "PREPARING".to_owned();
            run.eligible = false;
            run.dispatch_order = None;
            run.blocked_by = None;
            Ok(Some(run))
        })
    }
}

fn insert_schedule(
    transaction: &Transaction<'_>,
    id: &str,
    input: &ScheduleInput,
    prepared: &PreparedSchedule,
) -> Result<ScheduleRecord> {
    let plan = super::plans::query_plan(transaction, &input.plan_id, input.plan_version)?;
    if plan.version.status != PlanStatus::Approved.as_str() {
        return Err(ReactorError::InvalidSchedule(
            "schedule requires an approved plan version".to_owned(),
        ));
    }
    let now = jiff::Timestamp::now();
    let now_text = now.to_string();
    let rule_json = to_json(&input.rule)?;
    let gate_json = to_json(&ScheduleGate {
        start_deadline_seconds: input.start_deadline_seconds,
        local_endpoint: input.local_endpoint.clone(),
    })?;
    transaction.execute(
        "INSERT INTO schedules
         (id, plan_id, plan_version, status, rule_json, timezone, start_at, start_deadline,
          gate_json, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'ACTIVE', ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
        params![
            id,
            input.plan_id,
            plan.version.version,
            rule_json,
            prepared.timezone,
            prepared.scheduled_at,
            prepared.deadline_at,
            gate_json,
            now_text
        ],
    )?;
    write_event(
        transaction,
        "SCHEDULE_CREATED",
        "schedule",
        id,
        serde_json::json!({
            "plan_id": input.plan_id,
            "plan_version": plan.version.version
        }),
        &now_text,
    )?;

    match &input.rule {
        ScheduleRule::Manual => {}
        ScheduleRule::Immediate => {
            let deadline_at = deadline(now, input.start_deadline_seconds)?.to_string();
            update_schedule_window(transaction, id, &now_text, &deadline_at)?;
            let occurrence_id = insert_occurrence(transaction, id, &now_text, &deadline_at)?;
            materialize_occurrence(transaction, id, &occurrence_id, now)?;
        }
        ScheduleRule::At { .. } => {
            let scheduled_at = prepared.scheduled_at.as_deref().ok_or_else(|| {
                ReactorError::InvalidSchedule("scheduled time is missing".to_owned())
            })?;
            let deadline_at = prepared.deadline_at.as_deref().ok_or_else(|| {
                ReactorError::InvalidSchedule("schedule deadline is missing".to_owned())
            })?;
            let occurrence_id = insert_occurrence(transaction, id, scheduled_at, deadline_at)?;
            let scheduled = scheduled_at
                .parse::<jiff::Timestamp>()
                .map_err(|error| ReactorError::InvalidSchedule(error.to_string()))?;
            if scheduled <= now {
                materialize_occurrence(transaction, id, &occurrence_id, now)?;
            }
        }
    }
    query_schedule(transaction, id)
}

fn trigger(transaction: &Transaction<'_>, schedule_id: &str) -> Result<ScheduleRecord> {
    let schedule = query_schedule(transaction, schedule_id)?;
    if schedule.status != "ACTIVE" {
        return Err(invalid_transition(&schedule, "TRIGGERED"));
    }
    if !matches!(schedule.rule, ScheduleRule::Manual) {
        return Err(ReactorError::InvalidSchedule(
            "only a manual schedule can be triggered".to_owned(),
        ));
    }
    let now = jiff::Timestamp::now();
    let deadline_at = deadline(now, schedule.start_deadline_seconds)?;
    update_schedule_window(
        transaction,
        schedule_id,
        &now.to_string(),
        &deadline_at.to_string(),
    )?;
    let occurrence_id = insert_occurrence(
        transaction,
        schedule_id,
        &now.to_string(),
        &deadline_at.to_string(),
    )?;
    materialize_occurrence(transaction, schedule_id, &occurrence_id, now)?;
    query_schedule(transaction, schedule_id)
}

fn transition_schedule(
    store: &mut Store,
    schedule_id: &str,
    expected: &str,
    next: &str,
    idempotency_key: Option<&str>,
) -> Result<ScheduleRecord> {
    let request = serde_json::to_vec(&serde_json::json!({
        "operation": "schedule_transition",
        "schedule_id": schedule_id,
        "from": expected,
        "to": next
    }))
    .map_err(|error| ReactorError::InvalidInput(error.to_string()))?;
    let operation = |transaction: &Transaction<'_>| {
        let schedule = query_schedule(transaction, schedule_id)?;
        if schedule.status != expected {
            return Err(invalid_transition(&schedule, next));
        }
        let now = crate::time::now_utc();
        transaction.execute(
            "UPDATE schedules SET status = ?1, updated_at = ?2 WHERE id = ?3 AND status = ?4",
            params![next, now, schedule_id, expected],
        )?;
        write_event(
            transaction,
            "SCHEDULE_STATE_CHANGED",
            "schedule",
            schedule_id,
            serde_json::json!({ "from": expected, "to": next }),
            &now,
        )?;
        query_schedule(transaction, schedule_id)
    };
    match idempotency_key {
        Some(key) => store.idempotent_write(key, &request, operation),
        None => store.write_transaction(operation),
    }
}

fn refresh_due(transaction: &Transaction<'_>) -> Result<u32> {
    let now = jiff::Timestamp::now();
    let now_text = now.to_string();
    let mut statement = transaction.prepare(
        "SELECT so.schedule_id, so.id
         FROM schedule_occurrences so
         JOIN schedules s ON s.id = so.schedule_id
         WHERE so.state = 'WAITING' AND s.status = 'ACTIVE'
           AND julianday(so.scheduled_at) <= julianday(?1)
         ORDER BY julianday(so.scheduled_at), julianday(so.created_at), so.id",
    )?;
    let due = statement
        .query_map([&now_text], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (schedule_id, occurrence_id) in &due {
        materialize_occurrence(transaction, schedule_id, occurrence_id, now)?;
    }
    Ok(due.len() as u32)
}

fn materialize_occurrence(
    transaction: &Transaction<'_>,
    schedule_id: &str,
    occurrence_id: &str,
    now: jiff::Timestamp,
) -> Result<()> {
    let schedule = query_schedule(transaction, schedule_id)?;
    let occurrence = schedule.occurrence.ok_or_else(|| {
        ReactorError::InvalidStoredData("schedule occurrence is missing".to_owned())
    })?;
    if occurrence.id != occurrence_id || occurrence.state != "WAITING" {
        return Ok(());
    }
    let deadline = occurrence
        .deadline_at
        .as_deref()
        .ok_or_else(|| ReactorError::InvalidStoredData("deadline is missing".to_owned()))?
        .parse::<jiff::Timestamp>()
        .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
    let now_text = now.to_string();
    if now > deadline {
        transaction.execute(
            "UPDATE schedule_occurrences SET state = 'MISSED' WHERE id = ?1 AND state = 'WAITING'",
            [occurrence_id],
        )?;
        complete_schedule(transaction, schedule_id, &now_text)?;
        write_event(
            transaction,
            "SCHEDULE_OCCURRENCE_MISSED",
            "schedule",
            schedule_id,
            serde_json::json!({ "occurrence_id": occurrence_id }),
            &now_text,
        )?;
        return Ok(());
    }

    let plan =
        super::plans::query_plan(transaction, &schedule.plan_id, Some(schedule.plan_version))?;
    let run_id = crate::ids::new_id();
    transaction.execute(
        "INSERT INTO runs
         (id, workspace_id, plan_id, plan_version, occurrence_id, state, agent_kind,
          provider, local_endpoint, policy_snapshot_json, limits_snapshot_json,
          ready_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'QUEUED', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
        params![
            run_id,
            plan.version.workspace_id,
            schedule.plan_id,
            schedule.plan_version,
            occurrence_id,
            plan.version.agent.as_str(),
            plan.version.provider,
            schedule.local_endpoint,
            to_json(&plan.version.policy_snapshot)?,
            to_json(&plan.version.limits)?,
            occurrence.scheduled_at,
            now_text
        ],
    )?;
    transaction.execute(
        "UPDATE schedule_occurrences SET state = 'QUEUED', run_id = ?1
         WHERE id = ?2 AND state = 'WAITING'",
        params![run_id, occurrence_id],
    )?;
    complete_schedule(transaction, schedule_id, &now_text)?;
    write_event(
        transaction,
        "RUN_QUEUED",
        "run",
        &run_id,
        serde_json::json!({
            "schedule_id": schedule_id,
            "occurrence_id": occurrence_id,
            "workspace_id": plan.version.workspace_id
        }),
        &now_text,
    )
}

fn insert_occurrence(
    transaction: &Transaction<'_>,
    schedule_id: &str,
    scheduled_at: &str,
    deadline_at: &str,
) -> Result<String> {
    let id = crate::ids::new_id();
    transaction.execute(
        "INSERT INTO schedule_occurrences
         (id, schedule_id, scheduled_at, deadline_at, state, created_at)
         VALUES (?1, ?2, ?3, ?4, 'WAITING', ?5)",
        params![
            id,
            schedule_id,
            scheduled_at,
            deadline_at,
            crate::time::now_utc()
        ],
    )?;
    Ok(id)
}

fn update_schedule_window(
    transaction: &Transaction<'_>,
    schedule_id: &str,
    start_at: &str,
    start_deadline: &str,
) -> Result<()> {
    transaction.execute(
        "UPDATE schedules SET start_at = ?1, start_deadline = ?2 WHERE id = ?3",
        params![start_at, start_deadline, schedule_id],
    )?;
    Ok(())
}

fn complete_schedule(
    transaction: &Transaction<'_>,
    schedule_id: &str,
    updated_at: &str,
) -> Result<()> {
    transaction.execute(
        "UPDATE schedules SET status = 'COMPLETED', updated_at = ?1 WHERE id = ?2",
        params![updated_at, schedule_id],
    )?;
    Ok(())
}

fn evaluate_queue(connection: &Connection, config: &ConcurrencyConfig) -> Result<Vec<QueueRun>> {
    let mut runs = query_queued_runs(connection)?;
    let active = query_active_runs(connection)?;
    let active_workspaces: HashSet<&str> =
        active.iter().map(|run| run.workspace_id.as_str()).collect();
    let mut agent_counts = counts(active.iter().map(|run| run.agent_kind.clone()));
    let mut provider_counts = counts(active.iter().filter_map(|run| run.provider.clone()));
    let mut endpoint_counts = counts(active.iter().filter_map(|run| run.local_endpoint.clone()));
    let mut active_count = active.len();
    let mut heads = Vec::new();
    let mut seen = HashSet::new();
    for (index, run) in runs.iter_mut().enumerate() {
        if !seen.insert(run.workspace_id.clone()) {
            run.blocked_by = Some("workspace_fifo".to_owned());
        } else if active_workspaces.contains(run.workspace_id.as_str()) {
            run.blocked_by = Some("workspace_active".to_owned());
        } else {
            heads.push(index);
        }
    }
    rotate_after_last_workspace(connection, &runs, &mut heads)?;

    let mut dispatch_order = 0;
    for index in heads {
        let run = &mut runs[index];
        let blocked = if active_count >= usize::from(config.global) {
            Some("global_limit")
        } else if count_of(&agent_counts, &run.agent_kind) >= usize::from(config.per_agent) {
            Some("agent_limit")
        } else if run.provider.as_ref().is_some_and(|provider| {
            count_of(&provider_counts, provider) >= usize::from(config.per_provider)
        }) {
            Some("provider_limit")
        } else if run.local_endpoint.as_ref().is_some_and(|endpoint| {
            count_of(&endpoint_counts, endpoint) >= usize::from(config.per_local_endpoint)
        }) {
            Some("local_endpoint_limit")
        } else {
            None
        };
        if let Some(blocked) = blocked {
            run.blocked_by = Some(blocked.to_owned());
            continue;
        }
        run.eligible = true;
        run.dispatch_order = Some(dispatch_order);
        dispatch_order += 1;
        active_count += 1;
        increment(&mut agent_counts, run.agent_kind.clone());
        if let Some(provider) = &run.provider {
            increment(&mut provider_counts, provider.clone());
        }
        if let Some(endpoint) = &run.local_endpoint {
            increment(&mut endpoint_counts, endpoint.clone());
        }
    }
    Ok(runs)
}

fn query_queued_runs(connection: &Connection) -> Result<Vec<QueueRun>> {
    let mut statement = connection.prepare(
        "SELECT id, workspace_id, state, agent_kind, provider, local_endpoint, ready_at, created_at
         FROM runs WHERE state = 'QUEUED'
         ORDER BY julianday(ready_at), julianday(created_at), id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(QueueRun {
            id: row.get(0)?,
            workspace_id: row.get(1)?,
            state: row.get(2)?,
            agent_kind: row.get(3)?,
            provider: row.get(4)?,
            local_endpoint: row.get(5)?,
            ready_at: row.get(6)?,
            created_at: row.get(7)?,
            eligible: false,
            dispatch_order: None,
            blocked_by: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn query_active_runs(connection: &Connection) -> Result<Vec<QueueRun>> {
    let mut statement = connection.prepare(
        "SELECT id, workspace_id, state, agent_kind, provider, local_endpoint, ready_at, created_at
         FROM runs WHERE state IN ('PREPARING', 'RUNNING')
         ORDER BY julianday(created_at), id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(QueueRun {
            id: row.get(0)?,
            workspace_id: row.get(1)?,
            state: row.get(2)?,
            agent_kind: row.get(3)?,
            provider: row.get(4)?,
            local_endpoint: row.get(5)?,
            ready_at: row.get(6)?,
            created_at: row.get(7)?,
            eligible: false,
            dispatch_order: None,
            blocked_by: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn rotate_after_last_workspace(
    connection: &Connection,
    runs: &[QueueRun],
    heads: &mut [usize],
) -> Result<()> {
    let last: Option<String> = connection
        .query_row(
            "SELECT value FROM reactor_meta WHERE key = 'queue_last_workspace'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(last) = last else {
        return Ok(());
    };
    if let Some(position) = heads
        .iter()
        .position(|index| runs[*index].workspace_id == last)
    {
        heads.rotate_left(position + 1);
    }
    Ok(())
}

fn query_schedule(connection: &Connection, schedule_id: &str) -> Result<ScheduleRecord> {
    type StoredSchedule = (
        String,
        u32,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
    );
    let row: Option<StoredSchedule> = connection
        .query_row(
            "SELECT plan_id, plan_version, status, rule_json, timezone, start_at,
                    start_deadline, gate_json, created_at, updated_at
             FROM schedules WHERE id = ?1",
            [schedule_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        plan_id,
        plan_version,
        status,
        rule_json,
        timezone,
        start_at,
        start_deadline,
        gate_json,
        created_at,
        updated_at,
    )) = row
    else {
        return Err(ReactorError::ScheduleNotFound(schedule_id.to_owned()));
    };
    let gate: ScheduleGate = from_json(&gate_json)?;
    Ok(ScheduleRecord {
        id: schedule_id.to_owned(),
        plan_id,
        plan_version,
        status,
        rule: from_json(&rule_json)?,
        timezone,
        start_at,
        start_deadline,
        start_deadline_seconds: gate.start_deadline_seconds,
        local_endpoint: gate.local_endpoint,
        occurrence: query_occurrence(connection, schedule_id)?,
        created_at,
        updated_at,
    })
}

fn query_occurrence(
    connection: &Connection,
    schedule_id: &str,
) -> Result<Option<OccurrenceRecord>> {
    Ok(connection
        .query_row(
            "SELECT id, scheduled_at, deadline_at, state, run_id, created_at
             FROM schedule_occurrences WHERE schedule_id = ?1
             ORDER BY julianday(created_at) DESC, id DESC LIMIT 1",
            [schedule_id],
            |row| {
                Ok(OccurrenceRecord {
                    id: row.get(0)?,
                    scheduled_at: row.get(1)?,
                    deadline_at: row.get(2)?,
                    state: row.get(3)?,
                    run_id: row.get(4)?,
                    created_at: row.get(5)?,
                })
            },
        )
        .optional()?)
}

fn invalid_transition(schedule: &ScheduleRecord, next: &str) -> ReactorError {
    ReactorError::InvalidScheduleTransition {
        schedule_id: schedule.id.clone(),
        from: schedule.status.clone(),
        to: next.to_owned(),
    }
}

fn counts<K: std::hash::Hash + Eq>(values: impl Iterator<Item = K>) -> HashMap<K, usize> {
    let mut counts = HashMap::new();
    for value in values {
        increment(&mut counts, value);
    }
    counts
}

fn increment<K: std::hash::Hash + Eq>(counts: &mut HashMap<K, usize>, key: K) {
    *counts.entry(key).or_default() += 1;
}

fn count_of<K: std::hash::Hash + Eq>(counts: &HashMap<K, usize>, key: &K) -> usize {
    counts.get(key).copied().unwrap_or_default()
}

fn request_bytes<T: Serialize>(operation: &str, id: Option<&str>, input: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "operation": operation,
        "schedule_id": id,
        "input": input
    }))
    .map_err(|error| ReactorError::InvalidInput(error.to_string()))
}

fn to_json(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(|error| ReactorError::InvalidInput(error.to_string()))
}

fn from_json<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|error| ReactorError::InvalidStoredData(error.to_string()))
}

fn write_event(
    transaction: &Transaction<'_>,
    event_type: &str,
    entity_type: &str,
    entity_id: &str,
    payload: serde_json::Value,
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
