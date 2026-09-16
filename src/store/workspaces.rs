use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;

use super::{Store, prune_events};
use crate::{
    agents::AgentPreflight,
    error::{ReactorError, Result},
    workspace::{
        CanonicalWorkspace, WorkspaceAgentRecord, WorkspacePolicy, WorkspaceRecord, paths_overlap,
    },
};

impl Store {
    pub fn register_workspace(
        &mut self,
        id: &str,
        workspace: &CanonicalWorkspace,
        policy: &WorkspacePolicy,
        fingerprint: &str,
        idempotency_key: Option<&str>,
    ) -> Result<WorkspaceRecord> {
        let request = serde_json::to_vec(&serde_json::json!({
            "operation": "workspace_add",
            "id": id,
            "canonical_key": workspace.key,
            "policy": policy
        }))
        .map_err(|error| ReactorError::InvalidInput(error.to_string()))?;
        let operation = |transaction: &Transaction<'_>| {
            insert_workspace(transaction, id, workspace, policy, fingerprint)
        };
        let record = match idempotency_key {
            Some(key) => self.idempotent_write(key, &request, operation)?,
            None => self.write_transaction(operation)?,
        };
        Ok(record)
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
            save_agent_preflight(
                transaction,
                workspace_id,
                agent_kind,
                preflight,
                fingerprint,
            )
        };
        let record = match idempotency_key {
            Some(key) => self.idempotent_write(key, &request, operation)?,
            None => self.write_transaction(operation)?,
        };
        Ok(record)
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

    pub fn adopt_migrated_workspace_policy(
        &mut self,
        id: &str,
        fingerprint: &str,
    ) -> Result<WorkspaceRecord> {
        let now = crate::time::now_utc();
        self.write_transaction(|transaction| {
            let changed = transaction.execute(
                "UPDATE workspaces SET policy_fingerprint = ?1, updated_at = ?2
                 WHERE id = ?3 AND policy_fingerprint = ''",
                params![fingerprint, now, id],
            )?;
            if changed == 0 {
                return query_workspace(transaction, id);
            }
            write_event(
                transaction,
                "WORKSPACE_POLICY_MIGRATED",
                "workspace",
                id,
                serde_json::json!({ "policy_fingerprint": fingerprint }),
                &now,
            )?;
            query_workspace(transaction, id)
        })
    }

    pub fn mark_workspace_agent_degraded(
        &mut self,
        workspace_id: &str,
        agent_kind: &str,
        reason: &str,
    ) -> Result<WorkspaceRecord> {
        let now = crate::time::now_utc();
        self.write_transaction(|transaction| {
            query_workspace(transaction, workspace_id)?;
            let changed = transaction.execute(
                "UPDATE workspace_agents SET status = 'DEGRADED', checked_at = ?1
                 WHERE workspace_id = ?2 AND agent_kind = ?3 AND status <> 'DEGRADED'",
                params![now, workspace_id, agent_kind],
            )?;
            if changed > 0 {
                refresh_workspace_status(transaction, workspace_id, &now)?;
                write_event(
                    transaction,
                    "AGENT_DEGRADED",
                    "workspace",
                    workspace_id,
                    serde_json::json!({ "agent_kind": agent_kind, "reason": reason }),
                    &now,
                )?;
            }
            query_workspace(transaction, workspace_id)
        })
    }

    #[allow(dead_code)]
    pub fn require_ready_workspace_agent(
        &self,
        workspace_id: &str,
        agent_kind: &str,
        policy_fingerprint: &str,
    ) -> Result<()> {
        let workspace = self.get_workspace(workspace_id)?;
        let ready = workspace.status == "READY"
            && workspace.agents.iter().any(|agent| {
                agent.agent_kind == agent_kind
                    && agent.status == "READY"
                    && agent.policy_fingerprint.as_deref() == Some(policy_fingerprint)
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
}

fn insert_workspace(
    transaction: &Transaction<'_>,
    id: &str,
    workspace: &CanonicalWorkspace,
    policy: &WorkspacePolicy,
    fingerprint: &str,
) -> Result<WorkspaceRecord> {
    let mut statement = transaction
        .prepare("SELECT id, canonical_key, policy_fingerprint, policy_json FROM workspaces")?;
    let locations = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let requested_keys = policy.write_keys(&workspace.key);
    let mut existing_fingerprint = None;
    for (registered_id, registered_key, registered_fingerprint, registered_policy) in locations {
        if registered_id == id {
            if registered_key != workspace.key {
                return Err(ReactorError::WorkspaceOverlap {
                    workspace_id: registered_id,
                });
            }
            existing_fingerprint = Some(registered_fingerprint);
            continue;
        }
        let registered_policy: WorkspacePolicy = serde_json::from_str(&registered_policy)
            .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
        let registered_keys = registered_policy.write_keys(&registered_key);
        let overlaps = requested_keys.iter().any(|requested| {
            registered_keys
                .iter()
                .any(|registered| paths_overlap(registered, requested))
        });
        if overlaps {
            return Err(ReactorError::WorkspaceOverlap {
                workspace_id: registered_id,
            });
        }
    }

    let now = crate::time::now_utc();
    let policy_json = serde_json::to_string(policy)
        .map_err(|error| ReactorError::InvalidInput(error.to_string()))?;
    if let Some(existing_fingerprint) = existing_fingerprint {
        if existing_fingerprint == fingerprint {
            return query_workspace(transaction, id);
        }
        transaction.execute(
            "UPDATE workspaces
             SET status = 'REGISTERED', policy_json = ?1, policy_fingerprint = ?2, updated_at = ?3
             WHERE id = ?4",
            params![policy_json, fingerprint, now, id],
        )?;
        transaction.execute(
            "UPDATE workspace_agents
             SET status = 'DEGRADED', policy_fingerprint = NULL, checked_at = ?1
             WHERE workspace_id = ?2",
            params![now, id],
        )?;
        write_event(
            transaction,
            "WORKSPACE_POLICY_UPDATED",
            "workspace",
            id,
            serde_json::json!({ "status": "REGISTERED", "policy": policy }),
            &now,
        )?;
        return query_workspace(transaction, id);
    }
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
    refresh_workspace_status(transaction, workspace_id, &now)?;
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

fn refresh_workspace_status(
    transaction: &Transaction<'_>,
    workspace_id: &str,
    updated_at: &str,
) -> Result<()> {
    transaction.execute(
        "UPDATE workspaces SET status = CASE
             WHEN EXISTS (
                 SELECT 1 FROM workspace_agents
                 WHERE workspace_id = ?1 AND status = 'READY'
             ) THEN 'READY' ELSE 'DEGRADED' END,
         updated_at = ?2 WHERE id = ?1",
        params![workspace_id, updated_at],
    )?;
    Ok(())
}

pub(super) fn query_workspace(connection: &Connection, id: &str) -> Result<WorkspaceRecord> {
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
