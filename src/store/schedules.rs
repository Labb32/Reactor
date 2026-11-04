use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};

use super::{Store, write_event};
use crate::{
    domain::PlanStatus,
    error::{ReactorError, Result},
    schedule::{
        OccurrenceRecord, PreparedSchedule, ScheduleInput, ScheduleRecord, ScheduleRule, deadline,
    },
};

#[derive(Deserialize, Serialize)]
#[serde(default)]
struct ScheduleGate {
    start_deadline_seconds: u64,
    local_endpoint: Option<String>,
    after_quota_reset: bool,
}

impl Default for ScheduleGate {
    fn default() -> Self {
        Self {
            start_deadline_seconds: crate::schedule::DEFAULT_DEADLINE_SECONDS,
            local_endpoint: None,
            after_quota_reset: false,
        }
    }
}

impl Store {
    pub fn create_schedule(
        &mut self,
        id: Option<&str>,
        input: &ScheduleInput,
        prepared: &PreparedSchedule,
        idempotency_key: Option<&str>,
    ) -> Result<ScheduleRecord> {
        let request = request_bytes("schedule_create", id, input)?;
        let operation =
            |transaction: &Transaction<'_>| insert_schedule(transaction, id, input, prepared);
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
        let mut statement = self.connection.prepare(
            "SELECT id FROM schedules
                 WHERE deleted_at IS NULL ORDER BY julianday(created_at), rowid",
        )?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| query_schedule(&self.connection, &id))
            .collect()
    }

    pub fn stop_schedule(
        &mut self,
        schedule_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<ScheduleRecord> {
        let request = request_bytes::<()>("schedule_stop", Some(schedule_id), &())?;
        let operation = |transaction: &Transaction<'_>| stop_schedule(transaction, schedule_id);
        match idempotency_key {
            Some(key) => self.idempotent_write(key, &request, operation),
            None => self.write_transaction(operation),
        }
    }

    pub fn delete_schedule(
        &mut self,
        schedule_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<ScheduleRecord> {
        let request = request_bytes::<()>("schedule_delete", Some(schedule_id), &())?;
        let operation = |transaction: &Transaction<'_>| delete_schedule(transaction, schedule_id);
        match idempotency_key {
            Some(key) => self.idempotent_write(key, &request, operation),
            None => self.write_transaction(operation),
        }
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
}

fn insert_schedule(
    transaction: &Transaction<'_>,
    requested_id: Option<&str>,
    input: &ScheduleInput,
    prepared: &PreparedSchedule,
) -> Result<ScheduleRecord> {
    let plan = super::plans::query_plan(transaction, &input.plan_id, input.plan_version)?;
    if plan.version.status != PlanStatus::Approved.as_str() {
        return Err(ReactorError::InvalidSchedule(
            "schedule requires an approved plan version".to_owned(),
        ));
    }
    let id = allocate_schedule_id(transaction, requested_id, input)?;
    let quota_reset = if input.after_quota_reset {
        Some(
            plan.version
                .agent
                .adapter()
                .quota_reset_at(plan.version.provider.as_deref())?
                .ok_or_else(|| {
                    ReactorError::InvalidSchedule(
                        "the selected Agent does not provide quota reset information".to_owned(),
                    )
                })?,
        )
    } else {
        None
    };
    let now = jiff::Timestamp::now();
    let now_text = now.to_string();
    let rule_json = to_json(&input.rule)?;
    let gate_json = to_json(&ScheduleGate {
        start_deadline_seconds: input.start_deadline_seconds,
        local_endpoint: input.local_endpoint.clone(),
        after_quota_reset: input.after_quota_reset,
    })?;
    transaction.execute(
        "INSERT INTO schedules
         (id, plan_id, plan_version, status, rule_json, timezone, start_at, start_deadline,
          gate_json, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'ACTIVE', ?4, ?5, NULL, NULL, ?6, ?7, ?7)",
        params![
            &id,
            input.plan_id,
            plan.version.version,
            rule_json,
            prepared.timezone,
            gate_json,
            now_text
        ],
    )?;
    write_event(
        transaction,
        "SCHEDULE_CREATED",
        "schedule",
        &id,
        serde_json::json!({
            "plan_id": input.plan_id,
            "plan_version": plan.version.version
        }),
        &now_text,
    )?;

    match &input.rule {
        ScheduleRule::Manual => {}
        ScheduleRule::Immediate | ScheduleRule::IntervalAfterSuccess { .. } => {
            let scheduled = quota_reset.map_or(now, |reset| reset.max(now));
            let scheduled_at = scheduled.to_string();
            let deadline_at = deadline(scheduled, input.start_deadline_seconds)?.to_string();
            update_schedule_window(transaction, &id, &scheduled_at, &deadline_at)?;
            let occurrence_id = insert_occurrence(transaction, &id, &scheduled_at, &deadline_at)?;
            if scheduled <= now {
                materialize_occurrence(transaction, &id, &occurrence_id, now)?;
            }
        }
        ScheduleRule::At { .. }
        | ScheduleRule::Daily { .. }
        | ScheduleRule::Weekdays { .. }
        | ScheduleRule::Weekly { .. } => {
            let mut scheduled = prepared.scheduled_at.ok_or_else(|| {
                ReactorError::InvalidSchedule("scheduled time is missing".to_owned())
            })?;
            if let Some(reset) = quota_reset {
                scheduled = scheduled.max(reset);
            }
            let scheduled_at = scheduled.to_string();
            let deadline_at = deadline(scheduled, input.start_deadline_seconds)?.to_string();
            update_schedule_window(transaction, &id, &scheduled_at, &deadline_at)?;
            let occurrence_id = insert_occurrence(transaction, &id, &scheduled_at, &deadline_at)?;
            if scheduled <= now {
                materialize_occurrence(transaction, &id, &occurrence_id, now)?;
            }
        }
    }
    query_schedule(transaction, &id)
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
    let scheduled = quota_ready_at(transaction, &schedule)?.map_or(now, |reset| reset.max(now));
    let deadline_at = deadline(scheduled, schedule.start_deadline_seconds)?;
    update_schedule_window(
        transaction,
        schedule_id,
        &scheduled.to_string(),
        &deadline_at.to_string(),
    )?;
    let occurrence_id = insert_occurrence(
        transaction,
        schedule_id,
        &scheduled.to_string(),
        &deadline_at.to_string(),
    )?;
    if scheduled <= now {
        materialize_occurrence(transaction, schedule_id, &occurrence_id, now)?;
    }
    query_schedule(transaction, schedule_id)
}

fn stop_schedule(transaction: &Transaction<'_>, schedule_id: &str) -> Result<ScheduleRecord> {
    let schedule = query_schedule(transaction, schedule_id)?;
    if schedule.status == "STOPPED" {
        return Ok(schedule);
    }
    if !matches!(schedule.status.as_str(), "ACTIVE" | "COMPLETED") {
        return Err(invalid_transition(&schedule, "STOPPED"));
    }
    let run_ids = {
        let mut statement = transaction.prepare(
            "SELECT r.id FROM runs r
             JOIN schedule_occurrences so ON so.id = r.occurrence_id
             WHERE so.schedule_id = ?1
               AND r.state IN ('WAITING', 'QUEUED', 'PREPARING', 'RUNNING')",
        )?;
        let rows = statement.query_map([schedule_id], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for run_id in run_ids {
        super::runs::cancel(transaction, &run_id)?;
    }
    let now = crate::time::now_utc();
    transaction.execute(
        "UPDATE schedule_occurrences SET state = 'CANCELLED'
         WHERE schedule_id = ?1 AND state = 'WAITING'",
        [schedule_id],
    )?;
    // Keep the existing storage value while exposing STOPPED through the CLI contract.
    transaction.execute(
        "UPDATE schedules SET status = 'PAUSED', updated_at = ?1
         WHERE id = ?2 AND deleted_at IS NULL",
        params![now, schedule_id],
    )?;
    write_event(
        transaction,
        "SCHEDULE_STATE_CHANGED",
        "schedule",
        schedule_id,
        serde_json::json!({ "from": &schedule.status, "to": "STOPPED" }),
        &now,
    )?;
    query_schedule(transaction, schedule_id)
}

fn delete_schedule(transaction: &Transaction<'_>, schedule_id: &str) -> Result<ScheduleRecord> {
    let schedule = query_schedule(transaction, schedule_id)?;
    if schedule.status != "STOPPED" {
        return Err(ReactorError::InvalidSchedule(
            "only a stopped Schedule can be deleted".to_owned(),
        ));
    }
    let now = crate::time::now_utc();
    transaction.execute(
        "UPDATE schedules SET deleted_at = ?1, updated_at = ?1
         WHERE id = ?2 AND deleted_at IS NULL",
        params![now, schedule_id],
    )?;
    write_event(
        transaction,
        "SCHEDULE_DELETED",
        "schedule",
        schedule_id,
        serde_json::json!({ "plan_id": &schedule.plan_id }),
        &now,
    )?;
    Ok(schedule)
}

fn allocate_schedule_id(
    transaction: &Transaction<'_>,
    requested: Option<&str>,
    input: &ScheduleInput,
) -> Result<String> {
    if let Some(id) = requested {
        crate::schedule::validate_id(id)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM schedules WHERE id = ?1)",
            [id],
            |row| row.get(0),
        )?;
        if exists {
            return Err(ReactorError::InvalidSchedule(format!(
                "schedule name already exists: {id}"
            )));
        }
        return Ok(id.to_owned());
    }

    let base = format!(
        "{}-{}",
        input.plan_id,
        crate::schedule::rule_name(&input.rule)
    );
    for number in 1_u32..=u32::MAX {
        let id = if number == 1 {
            base.clone()
        } else {
            format!("{base}-{number}")
        };
        crate::schedule::validate_id(&id)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM schedules WHERE id = ?1)",
            [&id],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(id);
        }
    }
    Err(ReactorError::InvalidSchedule(
        "could not allocate a schedule name".to_owned(),
    ))
}

pub(super) fn refresh_due(transaction: &Transaction<'_>) -> Result<u32> {
    let now = jiff::Timestamp::now();
    let now_text = now.to_string();
    let mut statement = transaction.prepare(
        "SELECT so.schedule_id, so.id
         FROM schedule_occurrences so
         JOIN schedules s ON s.id = so.schedule_id
         WHERE so.state = 'WAITING' AND s.status = 'ACTIVE' AND s.deleted_at IS NULL
           AND julianday(so.scheduled_at) <= julianday(?1)
         ORDER BY julianday(so.scheduled_at), julianday(so.created_at), so.rowid",
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
    let expired = expire_queued_runs(transaction, &now_text)?;
    u32::try_from(due.len() + expired)
        .map_err(|_| ReactorError::InvalidStoredData("too many schedule updates".to_owned()))
}

fn expire_queued_runs(transaction: &Transaction<'_>, now: &str) -> Result<usize> {
    let mut statement = transaction.prepare(
        "SELECT r.id, so.id, so.schedule_id
         FROM runs r
         JOIN schedule_occurrences so ON so.id = r.occurrence_id
         WHERE r.state = 'QUEUED' AND so.state = 'QUEUED'
           AND so.deadline_at IS NOT NULL
           AND julianday(so.deadline_at) < julianday(?1)
         ORDER BY julianday(so.deadline_at), r.rowid",
    )?;
    let expired = statement
        .query_map([now], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);

    let mut changed = 0;
    for (run_id, occurrence_id, schedule_id) in expired {
        if transaction.execute(
            "UPDATE runs SET state = 'MISSED', updated_at = ?1
             WHERE id = ?2 AND state = 'QUEUED'",
            params![now, run_id],
        )? == 0
        {
            continue;
        }
        let occurrence_changed = transaction.execute(
            "UPDATE schedule_occurrences SET state = 'MISSED'
             WHERE id = ?1 AND state = 'QUEUED'",
            [&occurrence_id],
        )?;
        if occurrence_changed != 1 {
            return Err(ReactorError::InvalidStoredData(
                "queued occurrence changed unexpectedly".to_owned(),
            ));
        }
        write_event(
            transaction,
            "RUN_STATE_CHANGED",
            "run",
            &run_id,
            serde_json::json!({
                "from": "QUEUED",
                "to": "MISSED",
                "reason": "start_deadline"
            }),
            now,
        )?;
        write_event(
            transaction,
            "SCHEDULE_OCCURRENCE_MISSED",
            "schedule",
            &schedule_id,
            serde_json::json!({ "occurrence_id": occurrence_id, "run_id": run_id }),
            now,
        )?;
        let schedule = query_schedule(transaction, &schedule_id)?;
        if matches!(schedule.rule, ScheduleRule::IntervalAfterSuccess { .. }) {
            complete_schedule(transaction, &schedule_id, now)?;
        }
        changed += 1;
    }
    Ok(changed)
}

fn materialize_occurrence(
    transaction: &Transaction<'_>,
    schedule_id: &str,
    occurrence_id: &str,
    now: jiff::Timestamp,
) -> Result<()> {
    let schedule = query_schedule(transaction, schedule_id)?;
    let occurrence =
        query_occurrence_by_id(transaction, schedule_id, occurrence_id)?.ok_or_else(|| {
            ReactorError::InvalidStoredData("schedule occurrence is missing".to_owned())
        })?;
    if occurrence.state != "WAITING" {
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
        if matches!(schedule.rule, ScheduleRule::IntervalAfterSuccess { .. }) {
            complete_schedule(transaction, schedule_id, &now_text)?;
        } else {
            advance_schedule(transaction, &schedule, now)?;
        }
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
    let run_id = allocate_run_id(transaction)?;
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
    let changed = transaction.execute(
        "UPDATE schedule_occurrences SET state = 'QUEUED', run_id = ?1
         WHERE id = ?2 AND state = 'WAITING'",
        params![run_id, occurrence_id],
    )?;
    if changed != 1 {
        return Err(ReactorError::InvalidStoredData(
            "waiting occurrence changed unexpectedly".to_owned(),
        ));
    }
    advance_schedule(transaction, &schedule, now)?;
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

fn allocate_run_id(transaction: &Transaction<'_>) -> Result<String> {
    for _ in 0..16 {
        let id = crate::ids::new_id();
        if !super::runs::run_id_reserved(transaction, &id)? {
            return Ok(id);
        }
    }
    Err(ReactorError::InvalidStoredData(
        "could not allocate a Run ID".to_owned(),
    ))
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

fn advance_schedule(
    transaction: &Transaction<'_>,
    schedule: &ScheduleRecord,
    now: jiff::Timestamp,
) -> Result<()> {
    if let Some(next) = crate::schedule::next_occurrence(&schedule.rule, &schedule.timezone, now)? {
        let next = quota_ready_at(transaction, schedule)?.map_or(next, |reset| reset.max(next));
        let deadline_at = deadline(next, schedule.start_deadline_seconds)?;
        let next_text = next.to_string();
        let deadline_text = deadline_at.to_string();
        update_schedule_window(transaction, &schedule.id, &next_text, &deadline_text)?;
        insert_occurrence(transaction, &schedule.id, &next_text, &deadline_text)?;
        transaction.execute(
            "UPDATE schedules SET updated_at = ?1 WHERE id = ?2",
            params![now.to_string(), schedule.id],
        )?;
    } else if !matches!(schedule.rule, ScheduleRule::IntervalAfterSuccess { .. }) {
        complete_schedule(transaction, &schedule.id, &now.to_string())?;
    }
    Ok(())
}

pub(super) fn finish_recurring_run(
    transaction: &Transaction<'_>,
    occurrence_id: Option<&str>,
    succeeded: bool,
    completed_at: jiff::Timestamp,
) -> Result<()> {
    let Some(occurrence_id) = occurrence_id else {
        return Ok(());
    };
    let schedule_id: Option<String> = transaction
        .query_row(
            "SELECT s.id
             FROM schedules s
             JOIN schedule_occurrences so ON so.schedule_id = s.id
             WHERE so.id = ?1 AND s.status = 'ACTIVE' AND s.deleted_at IS NULL",
            [occurrence_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(schedule_id) = schedule_id else {
        return Ok(());
    };
    let schedule = query_schedule(transaction, &schedule_id)?;
    if !matches!(schedule.rule, ScheduleRule::IntervalAfterSuccess { .. }) {
        return Ok(());
    }
    if !succeeded {
        return complete_schedule(transaction, &schedule_id, &completed_at.to_string());
    }
    let next = crate::schedule::interval_after_success(&schedule.rule, completed_at)?
        .ok_or_else(|| ReactorError::InvalidStoredData("success interval is missing".to_owned()))?;
    let next = quota_ready_at(transaction, &schedule)?.map_or(next, |reset| reset.max(next));
    let deadline_at = deadline(next, schedule.start_deadline_seconds)?;
    let next_text = next.to_string();
    let deadline_text = deadline_at.to_string();
    update_schedule_window(transaction, &schedule_id, &next_text, &deadline_text)?;
    insert_occurrence(transaction, &schedule_id, &next_text, &deadline_text)?;
    transaction.execute(
        "UPDATE schedules SET updated_at = ?1 WHERE id = ?2",
        params![completed_at.to_string(), schedule_id],
    )?;
    Ok(())
}

fn quota_ready_at(
    transaction: &Transaction<'_>,
    schedule: &ScheduleRecord,
) -> Result<Option<jiff::Timestamp>> {
    if !schedule.after_quota_reset {
        return Ok(None);
    }
    let plan =
        super::plans::query_plan(transaction, &schedule.plan_id, Some(schedule.plan_version))?;
    plan.version
        .agent
        .adapter()
        .quota_reset_at(plan.version.provider.as_deref())?
        .ok_or_else(|| {
            ReactorError::InvalidSchedule(
                "quota reset information is no longer available".to_owned(),
            )
        })
        .map(Some)
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
             FROM schedules WHERE id = ?1 AND deleted_at IS NULL",
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
        status: if status == "PAUSED" {
            "STOPPED".to_owned()
        } else {
            status
        },
        rule: from_json(&rule_json)?,
        timezone,
        start_at,
        start_deadline,
        start_deadline_seconds: gate.start_deadline_seconds,
        local_endpoint: gate.local_endpoint,
        after_quota_reset: gate.after_quota_reset,
        occurrence: query_latest_occurrence(connection, schedule_id)?,
        created_at,
        updated_at,
    })
}

fn query_latest_occurrence(
    connection: &Connection,
    schedule_id: &str,
) -> Result<Option<OccurrenceRecord>> {
    Ok(connection
        .query_row(
            "SELECT so.id, so.scheduled_at, so.deadline_at,
                    CASE
                        WHEN so.run_id IS NOT NULL AND r.id IS NULL THEN 'CLEARED'
                        ELSE COALESCE(r.state, so.state)
                    END,
                    so.run_id, so.created_at
             FROM schedule_occurrences so
             LEFT JOIN runs r ON r.id = so.run_id
             WHERE so.schedule_id = ?1
             ORDER BY so.rowid DESC LIMIT 1",
            [schedule_id],
            map_occurrence,
        )
        .optional()?)
}

fn query_occurrence_by_id(
    connection: &Connection,
    schedule_id: &str,
    occurrence_id: &str,
) -> Result<Option<OccurrenceRecord>> {
    Ok(connection
        .query_row(
            "SELECT id, scheduled_at, deadline_at, state, run_id, created_at
             FROM schedule_occurrences WHERE id = ?1 AND schedule_id = ?2",
            params![occurrence_id, schedule_id],
            map_occurrence,
        )
        .optional()?)
}

fn map_occurrence(row: &Row<'_>) -> rusqlite::Result<OccurrenceRecord> {
    Ok(OccurrenceRecord {
        id: row.get(0)?,
        scheduled_at: row.get(1)?,
        deadline_at: row.get(2)?,
        state: row.get(3)?,
        run_id: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn invalid_transition(schedule: &ScheduleRecord, next: &str) -> ReactorError {
    ReactorError::InvalidScheduleTransition {
        schedule_id: schedule.id.clone(),
        from: schedule.status.clone(),
        to: next.to_owned(),
    }
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
