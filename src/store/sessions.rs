use super::Store;
use crate::error::{ReactorError, Result};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSession {
    pub id: String,
    pub policy_fingerprint: String,
    pub model: Option<String>,
    pub provider: Option<String>,
}

impl Store {
    pub(crate) fn record_agent_events(
        &mut self,
        attempt_id: &str,
        events: &crate::agent_events::Events,
    ) -> Result<()> {
        self.write_transaction(|tx| {
            let value = serde_json::json!({"session_id": events.session_id, "response": events.response.as_deref().map(crate::redact::text),
                "usage_complete": !events.incomplete, "tokens": events.tokens, "usage_seen": events.usage_seen}).to_string();
            tx.execute("INSERT INTO reactor_meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![format!("events:{attempt_id}"), value])?;
            Ok(())
        })
    }

    pub fn workspace_session(&self, workspace_id: &str) -> Result<Option<WorkspaceSession>> {
        self.ensure_supported_schema()?;
        let value: Option<String> = self
            .connection
            .query_row(
                "SELECT value FROM reactor_meta WHERE key = ?1",
                [format!("session:{workspace_id}:codex")],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))
            })
            .transpose()
    }

    pub(crate) fn save_workspace_session(
        &mut self,
        workspace_id: &str,
        session: &WorkspaceSession,
    ) -> Result<()> {
        if uuid::Uuid::parse_str(&session.id).is_err() {
            return Err(ReactorError::InvalidInput("invalid session ID".to_owned()));
        }
        let value = serde_json::to_string(session)
            .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
        self.write_transaction(|tx| {
            tx.execute("INSERT INTO reactor_meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![format!("session:{workspace_id}:codex"), value])?;
            super::write_event(tx, "SESSION_SELECTED", "workspace", workspace_id, serde_json::json!({"session_id": session.id}), &crate::time::now_utc())?;
            Ok(())
        })
    }

    pub fn reset_workspace_session(&mut self, workspace_id: &str) -> Result<()> {
        self.write_transaction(|tx| {
            super::workspaces::query_workspace(tx, workspace_id)?;
            let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE workspace_id = ?1 AND
                (state IN ('PREPARING', 'RUNNING') OR EXISTS(SELECT 1 FROM run_tasks WHERE run_id = runs.id AND state = 'WAITING_REVIEW')
                 OR EXISTS(SELECT 1 FROM leases WHERE resource_type = 'run' AND resource_id = runs.id)))", [workspace_id], |row| row.get(0))?;
            if active { return Err(ReactorError::InvalidInput("session reset requires an idle workspace".to_owned())); }
            tx.execute("DELETE FROM reactor_meta WHERE key = ?1", [format!("session:{workspace_id}:codex")])?;
            super::write_event(tx, "SESSION_RESET", "workspace", workspace_id, serde_json::json!({}), &crate::time::now_utc())?;
            Ok(())
        })
    }
}
