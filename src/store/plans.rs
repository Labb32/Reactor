use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{Store, write_event};
use crate::{
    domain::PlanStatus,
    error::{ReactorError, Result},
    plan::{PlanApproval, PlanInput, PlanRecord, PlanVersionRecord, validate_requirements},
};

impl Store {
    pub fn create_plan(
        &mut self,
        id: Option<&str>,
        input: &PlanInput,
        idempotency_key: Option<&str>,
    ) -> Result<PlanRecord> {
        let request = request_bytes("plan_create", id, input)?;
        let id = id.map(str::to_owned).unwrap_or_else(crate::ids::new_id);
        self.insert_plan_with_request(&id, input, idempotency_key, &request)
    }

    pub fn create_drafted_plan(
        &mut self,
        id: Option<&str>,
        input: &PlanInput,
        idempotency_key: Option<&str>,
        request: &[u8],
    ) -> Result<PlanRecord> {
        let id = id.map(str::to_owned).unwrap_or_else(crate::ids::new_id);
        self.insert_plan_with_request(&id, input, idempotency_key, request)
    }

    fn insert_plan_with_request(
        &mut self,
        id: &str,
        input: &PlanInput,
        idempotency_key: Option<&str>,
        request: &[u8],
    ) -> Result<PlanRecord> {
        let operation = |transaction: &Transaction<'_>| insert_plan(transaction, id, input);
        match idempotency_key {
            Some(key) => self.idempotent_write(key, request, operation),
            None => self.write_transaction(operation),
        }
    }

    pub fn revise_plan(
        &mut self,
        plan_id: &str,
        input: &PlanInput,
        idempotency_key: Option<&str>,
    ) -> Result<PlanRecord> {
        let request = request_bytes("plan_revise", Some(plan_id), input)?;
        let operation = |transaction: &Transaction<'_>| revise_plan(transaction, plan_id, input);
        match idempotency_key {
            Some(key) => self.idempotent_write(key, &request, operation),
            None => self.write_transaction(operation),
        }
    }

    pub fn review_plan(
        &mut self,
        plan_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<PlanRecord> {
        transition_plan(
            self,
            plan_id,
            PlanStatus::Draft,
            PlanStatus::InReview,
            idempotency_key,
        )
    }

    pub fn approve_plan(
        &mut self,
        plan_id: &str,
        expected_version: u32,
        approved_by: &str,
        idempotency_key: Option<&str>,
    ) -> Result<PlanRecord> {
        let request = serde_json::to_vec(&serde_json::json!({
            "operation": "plan_approve",
            "plan_id": plan_id,
            "expected_version": expected_version,
            "approved_by": approved_by
        }))
        .map_err(|error| ReactorError::InvalidInput(error.to_string()))?;
        let operation = |transaction: &Transaction<'_>| {
            approve_plan(transaction, plan_id, expected_version, approved_by)
        };
        match idempotency_key {
            Some(key) => self.idempotent_write(key, &request, operation),
            None => self.write_transaction(operation),
        }
    }

    pub fn unapprove_plan(
        &mut self,
        plan_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<PlanRecord> {
        let request = mutation_request("plan_unapprove", plan_id)?;
        let operation = |transaction: &Transaction<'_>| unapprove_plan(transaction, plan_id);
        match idempotency_key {
            Some(key) => self.idempotent_write(key, &request, operation),
            None => self.write_transaction(operation),
        }
    }

    pub fn delete_plan(
        &mut self,
        plan_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<PlanRecord> {
        let request = mutation_request("plan_delete", plan_id)?;
        let operation = |transaction: &Transaction<'_>| delete_plan(transaction, plan_id);
        match idempotency_key {
            Some(key) => self.idempotent_write(key, &request, operation),
            None => self.write_transaction(operation),
        }
    }

    pub fn get_plan(&self, plan_id: &str, version: Option<u32>) -> Result<PlanRecord> {
        self.ensure_supported_schema()?;
        query_plan(&self.connection, plan_id, version)
    }

    pub fn list_plans(&self) -> Result<Vec<PlanRecord>> {
        self.ensure_supported_schema()?;
        let mut statement = self
            .connection
            .prepare("SELECT id FROM plans WHERE deleted_at IS NULL ORDER BY created_at, id")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| query_plan(&self.connection, &id, None))
            .collect()
    }
}

fn insert_plan(transaction: &Transaction<'_>, id: &str, input: &PlanInput) -> Result<PlanRecord> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM plans WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?;
    if exists {
        return Err(ReactorError::InvalidPlan(format!(
            "plan name already exists: {id}"
        )));
    }
    let workspace = super::workspaces::query_workspace(transaction, &input.workspace_id)?;
    validate_requirements(&input.requirements, &workspace)?;
    let now = crate::time::now_utc();
    transaction.execute(
        "INSERT INTO plans (id, current_version, created_at, updated_at) VALUES (?1, 1, ?2, ?2)",
        params![id, now],
    )?;
    insert_version(transaction, id, 1, input, &workspace, &now)?;
    write_event(
        transaction,
        "PLAN_CREATED",
        "plan",
        id,
        serde_json::json!({ "version": 1, "status": "DRAFT" }),
        &now,
    )?;
    query_plan(transaction, id, None)
}

fn revise_plan(
    transaction: &Transaction<'_>,
    plan_id: &str,
    input: &PlanInput,
) -> Result<PlanRecord> {
    let current: Option<u32> = transaction
        .query_row(
            "SELECT current_version FROM plans WHERE id = ?1",
            [plan_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(current) = current else {
        return Err(ReactorError::PlanNotFound(plan_id.to_owned()));
    };
    let workspace = super::workspaces::query_workspace(transaction, &input.workspace_id)?;
    validate_requirements(&input.requirements, &workspace)?;
    let version = current + 1;
    let now = crate::time::now_utc();
    insert_version(transaction, plan_id, version, input, &workspace, &now)?;
    transaction.execute(
        "UPDATE plans SET current_version = ?1, updated_at = ?2 WHERE id = ?3",
        params![version, now, plan_id],
    )?;
    write_event(
        transaction,
        "PLAN_REVISED",
        "plan",
        plan_id,
        serde_json::json!({ "version": version, "status": "DRAFT" }),
        &now,
    )?;
    query_plan(transaction, plan_id, None)
}

fn insert_version(
    transaction: &Transaction<'_>,
    plan_id: &str,
    version: u32,
    input: &PlanInput,
    workspace: &crate::workspace::WorkspaceRecord,
    now: &str,
) -> Result<()> {
    let completion = to_json(&input.completion_criteria)?;
    let requirements = to_json(&input.requirements)?;
    let limits = to_json(&input.limits)?;
    let policy = to_json(&workspace.policy)?;
    transaction.execute(
        "INSERT INTO plan_versions
         (plan_id, version, workspace_id, status, title, context, completion_criteria_json,
          agent_kind, provider, model, requirements_json, limits_json, policy_json,
          workspace_policy_fingerprint, created_at)
         VALUES (?1, ?2, ?3, 'DRAFT', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            plan_id,
            version,
            input.workspace_id,
            input.title.trim(),
            input.context,
            completion,
            input.agent.as_str(),
            input.provider,
            input.model,
            requirements,
            limits,
            policy,
            workspace.policy_fingerprint,
            now
        ],
    )?;
    Ok(())
}

fn transition_plan(
    store: &mut Store,
    plan_id: &str,
    expected: PlanStatus,
    next: PlanStatus,
    idempotency_key: Option<&str>,
) -> Result<PlanRecord> {
    let request = serde_json::to_vec(&serde_json::json!({
        "operation": "plan_transition",
        "plan_id": plan_id,
        "from": expected.as_str(),
        "to": next.as_str()
    }))
    .map_err(|error| ReactorError::InvalidInput(error.to_string()))?;
    let operation = |transaction: &Transaction<'_>| {
        let current = query_plan(transaction, plan_id, None)?;
        if current.version.status != expected.as_str() {
            return Err(ReactorError::InvalidPlanTransition {
                plan_id: plan_id.to_owned(),
                from: current.version.status,
                to: next.as_str().to_owned(),
            });
        }
        let now = crate::time::now_utc();
        transaction.execute(
            "UPDATE plan_versions SET status = ?1 WHERE plan_id = ?2 AND version = ?3",
            params![next.as_str(), plan_id, current.current_version],
        )?;
        transaction.execute(
            "UPDATE plans SET updated_at = ?1 WHERE id = ?2",
            params![now, plan_id],
        )?;
        write_event(
            transaction,
            "PLAN_STATE_CHANGED",
            "plan",
            plan_id,
            serde_json::json!({
                "version": current.current_version,
                "from": expected.as_str(),
                "to": next.as_str()
            }),
            &now,
        )?;
        query_plan(transaction, plan_id, None)
    };
    match idempotency_key {
        Some(key) => store.idempotent_write(key, &request, operation),
        None => store.write_transaction(operation),
    }
}

fn approve_plan(
    transaction: &Transaction<'_>,
    plan_id: &str,
    expected_version: u32,
    approved_by: &str,
) -> Result<PlanRecord> {
    let current = query_plan(transaction, plan_id, None)?;
    if current.current_version != expected_version {
        return Err(ReactorError::PlanVersionChanged {
            plan_id: plan_id.to_owned(),
            expected: expected_version,
            found: current.current_version,
        });
    }
    if current.version.status != PlanStatus::InReview.as_str() {
        return Err(ReactorError::InvalidPlanTransition {
            plan_id: plan_id.to_owned(),
            from: current.version.status,
            to: PlanStatus::Approved.as_str().to_owned(),
        });
    }
    let workspace = super::workspaces::query_workspace(transaction, &current.version.workspace_id)?;
    if workspace.policy_fingerprint != current.version.workspace_policy_fingerprint {
        return Err(ReactorError::PlanPolicyChanged);
    }
    validate_requirements(&current.version.requirements, &workspace)?;
    let ready = workspace.status == "READY"
        && workspace.agents.iter().any(|agent| {
            agent.agent_kind == current.version.agent.as_str() && agent.status == "READY"
        });
    if !ready {
        return Err(ReactorError::AgentNotReady {
            workspace_id: workspace.id,
            agent: current.version.agent.as_str().to_owned(),
            reason: "run workspace prepare before approval".to_owned(),
        });
    }

    let now = crate::time::now_utc();
    transaction.execute(
        "UPDATE plan_versions SET status = 'APPROVED' WHERE plan_id = ?1 AND version = ?2",
        params![plan_id, current.current_version],
    )?;
    transaction.execute(
        "UPDATE plans SET updated_at = ?1 WHERE id = ?2",
        params![now, plan_id],
    )?;
    transaction.execute(
        "INSERT INTO plan_approvals (plan_id, plan_version, approved_by, approved_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![plan_id, current.current_version, approved_by, now],
    )?;
    write_event(
        transaction,
        "PLAN_APPROVED",
        "plan",
        plan_id,
        serde_json::json!({
            "version": current.current_version,
            "approved_by": approved_by
        }),
        &now,
    )?;
    query_plan(transaction, plan_id, None)
}

fn unapprove_plan(transaction: &Transaction<'_>, plan_id: &str) -> Result<PlanRecord> {
    let current = query_plan(transaction, plan_id, None)?;
    if current.version.status != PlanStatus::Approved.as_str() {
        return Err(ReactorError::InvalidPlanTransition {
            plan_id: plan_id.to_owned(),
            from: current.version.status,
            to: PlanStatus::InReview.as_str().to_owned(),
        });
    }
    ensure_version_unreferenced(transaction, plan_id, current.current_version)?;
    transaction.execute(
        "DELETE FROM plan_approvals WHERE plan_id = ?1 AND plan_version = ?2",
        params![plan_id, current.current_version],
    )?;
    let now = crate::time::now_utc();
    transaction.execute(
        "UPDATE plan_versions SET status = 'IN_REVIEW' WHERE plan_id = ?1 AND version = ?2",
        params![plan_id, current.current_version],
    )?;
    transaction.execute(
        "UPDATE plans SET updated_at = ?1 WHERE id = ?2",
        params![now, plan_id],
    )?;
    write_event(
        transaction,
        "PLAN_APPROVAL_REVOKED",
        "plan",
        plan_id,
        serde_json::json!({ "version": current.current_version }),
        &now,
    )?;
    query_plan(transaction, plan_id, None)
}

fn delete_plan(transaction: &Transaction<'_>, plan_id: &str) -> Result<PlanRecord> {
    let plan = query_plan(transaction, plan_id, None)?;
    ensure_plan_unreferenced(transaction, plan_id)?;
    let now = crate::time::now_utc();
    transaction.execute(
        "UPDATE plans SET deleted_at = ?1, updated_at = ?1
         WHERE id = ?2 AND deleted_at IS NULL",
        params![now, plan_id],
    )?;
    write_event(
        transaction,
        "PLAN_DELETED",
        "plan",
        plan_id,
        serde_json::json!({ "versions": plan.current_version }),
        &now,
    )?;
    Ok(plan)
}

fn ensure_version_unreferenced(
    transaction: &Transaction<'_>,
    plan_id: &str,
    version: u32,
) -> Result<()> {
    let referenced: bool = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM schedules WHERE plan_id = ?1 AND plan_version = ?2
            UNION ALL
            SELECT 1 FROM runs WHERE plan_id = ?1 AND plan_version = ?2
        )",
        params![plan_id, version],
        |row| row.get(0),
    )?;
    if referenced {
        return Err(ReactorError::InvalidPlan(
            "an approved Plan Version referenced by a Schedule or Run cannot be unapproved"
                .to_owned(),
        ));
    }
    Ok(())
}

fn ensure_plan_unreferenced(transaction: &Transaction<'_>, plan_id: &str) -> Result<()> {
    let referenced: bool = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM schedules WHERE plan_id = ?1 AND deleted_at IS NULL
            UNION ALL
            SELECT 1 FROM runs
            WHERE plan_id = ?1 AND state IN ('WAITING', 'QUEUED', 'PREPARING', 'RUNNING')
        )",
        [plan_id],
        |row| row.get(0),
    )?;
    if referenced {
        return Err(ReactorError::InvalidPlan(
            "a Plan referenced by a registered Schedule or active Run cannot be deleted".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn query_plan(
    connection: &Connection,
    plan_id: &str,
    version: Option<u32>,
) -> Result<PlanRecord> {
    let plan: Option<(u32, String, String)> = connection
        .query_row(
            "SELECT current_version, created_at, updated_at
             FROM plans WHERE id = ?1 AND deleted_at IS NULL",
            [plan_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((current_version, created_at, updated_at)) = plan else {
        return Err(ReactorError::PlanNotFound(plan_id.to_owned()));
    };
    let selected = version.unwrap_or(current_version);
    let version = query_version(connection, plan_id, selected)?;
    Ok(PlanRecord {
        id: plan_id.to_owned(),
        current_version,
        version,
        created_at,
        updated_at,
    })
}

fn query_version(
    connection: &Connection,
    plan_id: &str,
    version: u32,
) -> Result<PlanVersionRecord> {
    struct StoredVersion {
        workspace_id: String,
        status: String,
        title: String,
        context: String,
        completion: String,
        agent_kind: String,
        requirements: String,
        provider: Option<String>,
        model: Option<String>,
        limits: String,
        policy: String,
        workspace_policy_fingerprint: String,
        created_at: String,
        approved_by: Option<String>,
        approved_at: Option<String>,
    }
    let stored: Option<StoredVersion> = connection
        .query_row(
            "SELECT pv.workspace_id, pv.status, pv.title, pv.context,
                    pv.completion_criteria_json, pv.agent_kind, pv.requirements_json,
                    pv.provider, pv.model, pv.limits_json, pv.policy_json,
                    pv.workspace_policy_fingerprint, pv.created_at,
                    pa.approved_by, pa.approved_at
             FROM plan_versions pv
             LEFT JOIN plan_approvals pa
               ON pa.plan_id = pv.plan_id AND pa.plan_version = pv.version
             WHERE pv.plan_id = ?1 AND pv.version = ?2",
            params![plan_id, version],
            |row| {
                Ok(StoredVersion {
                    workspace_id: row.get(0)?,
                    status: row.get(1)?,
                    title: row.get(2)?,
                    context: row.get(3)?,
                    completion: row.get(4)?,
                    agent_kind: row.get(5)?,
                    requirements: row.get(6)?,
                    provider: row.get(7)?,
                    model: row.get(8)?,
                    limits: row.get(9)?,
                    policy: row.get(10)?,
                    workspace_policy_fingerprint: row.get(11)?,
                    created_at: row.get(12)?,
                    approved_by: row.get(13)?,
                    approved_at: row.get(14)?,
                })
            },
        )
        .optional()?;
    let Some(stored) = stored else {
        return Err(ReactorError::PlanVersionNotFound {
            plan_id: plan_id.to_owned(),
            version,
        });
    };
    let agent = crate::agents::AgentKind::parse(&stored.agent_kind).ok_or_else(|| {
        ReactorError::InvalidStoredData(format!("unknown agent: {}", stored.agent_kind))
    })?;
    let approval = match (stored.approved_by, stored.approved_at) {
        (Some(approved_by), Some(approved_at)) => Some(PlanApproval {
            approved_by,
            approved_at,
        }),
        (None, None) => None,
        _ => {
            return Err(ReactorError::InvalidStoredData(
                "incomplete plan approval".to_owned(),
            ));
        }
    };
    Ok(PlanVersionRecord {
        version,
        workspace_id: stored.workspace_id,
        status: stored.status,
        title: stored.title,
        context: stored.context,
        completion_criteria: from_json(&stored.completion)?,
        agent,
        provider: stored.provider,
        model: stored.model,
        requirements: from_json(&stored.requirements)?,
        limits: from_json(&stored.limits)?,
        policy_snapshot: from_json(&stored.policy)?,
        workspace_policy_fingerprint: stored.workspace_policy_fingerprint,
        approval,
        created_at: stored.created_at,
    })
}

fn request_bytes(operation: &str, plan_id: Option<&str>, input: &PlanInput) -> Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "operation": operation,
        "plan_id": plan_id,
        "input": input
    }))
    .map_err(|error| ReactorError::InvalidInput(error.to_string()))
}

fn mutation_request(operation: &str, plan_id: &str) -> Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "operation": operation,
        "plan_id": plan_id
    }))
    .map_err(|error| ReactorError::InvalidInput(error.to_string()))
}

fn to_json(value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(|error| ReactorError::InvalidInput(error.to_string()))
}

fn from_json<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|error| ReactorError::InvalidStoredData(error.to_string()))
}
