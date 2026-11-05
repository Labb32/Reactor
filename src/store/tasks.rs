use rusqlite::{Connection, params};
use super::{Store, runs::query_run};
use crate::{error::{Result, ReactorError}, run::{RunRecord, RunTaskRecord}};

pub(super) fn query_tasks(connection: &Connection, run_id: &str) -> Result<Vec<RunTaskRecord>> {
    let mut statement = connection.prepare("SELECT task_index, task_id, state, warnings_json, response FROM run_tasks WHERE run_id = ?1 ORDER BY task_index")?;
    let rows = statement.query_map([run_id], |row| Ok((row.get::<_,u32>(0)?, row.get::<_,String>(1)?, row.get::<_,String>(2)?, row.get::<_,String>(3)?, row.get::<_,Option<String>>(4)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(|(index, id, state, warnings, response)| {
        Ok(RunTaskRecord { index, id, state, response, warnings: serde_json::from_str(&warnings)
            .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))? })
    }).collect()
}

impl Store {
    pub(crate) fn initialize_tasks(&mut self, run_id: &str) -> Result<()> {
        self.write_transaction(|tx| {
            let run = query_run(tx, run_id)?;
            let plan = super::plans::query_plan(tx, &run.plan_id, Some(run.plan_version))?;
            for (index, task) in plan.version.tasks.iter().enumerate() {
                tx.execute("INSERT OR IGNORE INTO run_tasks (run_id, task_index, task_id, state, updated_at) VALUES (?1, ?2, ?3, 'WAITING', ?4)",
                    params![run_id, index as u32, task.id, crate::time::now_utc()])?;
            }
            Ok(())
        })
    }

    pub(crate) fn set_execution_path(&mut self, run_id: &str, path: &str) -> Result<()> {
        self.write_transaction(|tx| {
            tx.execute("UPDATE runs SET execution_path = ?1 WHERE id = ?2 AND state = 'PREPARING' AND execution_path IS NULL", params![path, run_id])?;
            Ok(())
        })
    }

    pub(crate) fn record_task_outcome(&mut self, run_id: &str, state: &str, warnings: &[String], response: &str) -> Result<()> {
        let response = crate::redact::text(response);
        let warnings = warnings.iter().map(|warning| crate::redact::text(warning)).collect::<Vec<_>>();
        self.write_transaction(|tx| {
            let run = query_run(tx, run_id)?;
            tx.execute("UPDATE run_tasks SET state = ?1, warnings_json = ?2, response = ?3, updated_at = ?4 WHERE run_id = ?5 AND task_index = ?6",
                params![state, serde_json::to_string(&warnings).map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?, response, crate::time::now_utc(), run_id, run.task_index])?;
            if !matches!(state, "SUCCEEDED" | "WAITING_REVIEW") {
                tx.execute("UPDATE run_tasks SET state = 'BLOCKED', updated_at = ?1 WHERE run_id = ?2 AND task_index > ?3",
                    params![crate::time::now_utc(), run_id, run.task_index])?;
            }
            Ok(())
        })
    }

    pub(crate) fn advance_task(&mut self, run_id: &str, attempt_id: &str, exit_code: Option<i32>) -> Result<RunRecord> {
        self.write_transaction(|tx| {
            let run = query_run(tx, run_id)?;
            if run.state != "RUNNING" || run.cancel_requested_at.is_some() {
                return Err(ReactorError::InvalidInput("cannot advance stopped run".to_owned()));
            }
            tx.execute("UPDATE attempts SET ended_at = ?1, exit_code = ?2, termination_reason = 'task_completed' WHERE id = ?3 AND run_id = ?4 AND ended_at IS NULL",
                params![crate::time::now_utc(), exit_code, attempt_id, run_id])?;
            let plan = super::plans::query_plan(tx, &run.plan_id, Some(run.plan_version))?;
            let limits = serde_json::to_string(&plan.version.limits).map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
            tx.execute("UPDATE runs SET task_index = task_index + 1, state = 'PREPARING', updated_at = ?1, limits_snapshot_json = ?3 WHERE id = ?2", params![crate::time::now_utc(), run_id, limits])?;
            super::write_event(tx, "TASK_ADVANCED", "run", run_id, serde_json::json!({"completed_index": run.task_index}), &crate::time::now_utc())?;
            query_run(tx, run_id)
        })
    }

    pub fn continue_tasks(&mut self, run_id: &str) -> Result<RunRecord> {
        self.write_transaction(|tx| {
            let run = query_run(tx, run_id)?;
            if run.state != "REVIEW_REQUIRED" || run.cancel_requested_at.is_some()
                || !run.tasks.iter().any(|task| task.index == run.task_index && task.state == "WAITING_REVIEW")
                || run.attempt.as_ref().is_some_and(|attempt| attempt.ended_at.is_none()) {
                return Err(ReactorError::InvalidInput("only a completed task warning can be acknowledged".to_owned()));
            }
            tx.execute("UPDATE run_tasks SET state = 'SUCCEEDED', updated_at = ?1 WHERE run_id = ?2 AND task_index = ?3", params![crate::time::now_utc(), run_id, run.task_index])?;
            tx.execute("UPDATE runs SET task_index = task_index + 1, state = 'QUEUED', updated_at = ?1 WHERE id = ?2", params![crate::time::now_utc(), run_id])?;
            let plan = super::plans::query_plan(tx, &run.plan_id, Some(run.plan_version))?;
            tx.execute("UPDATE runs SET limits_snapshot_json = ?1 WHERE id = ?2",
                params![serde_json::to_string(&plan.version.limits).map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?, run_id])?;
            tx.execute("DELETE FROM leases WHERE resource_type = 'run' AND resource_id = ?1", [run_id])?;
            super::write_event(tx, "TASK_WARNINGS_ACKNOWLEDGED", "run", run_id, serde_json::json!({"task_index": run.task_index}), &crate::time::now_utc())?;
            query_run(tx, run_id)
        })
    }
}
