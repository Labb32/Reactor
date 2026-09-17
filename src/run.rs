use serde::{Deserialize, Serialize};

use crate::{plan::PlanLimits, workspace::WorkspacePolicy};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AttemptRecord {
    pub id: String,
    pub attempt_number: u32,
    pub process_id: Option<u32>,
    pub process_started_at: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub exit_code: Option<i32>,
    pub termination_reason: Option<String>,
    pub log_path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RunRecord {
    pub id: String,
    pub workspace_id: String,
    pub plan_id: String,
    pub plan_version: u32,
    pub occurrence_id: Option<String>,
    pub state: String,
    pub agent_kind: String,
    pub provider: Option<String>,
    pub local_endpoint: Option<String>,
    pub limits: PlanLimits,
    pub ready_at: String,
    pub cancel_requested_at: Option<String>,
    pub attempt: Option<AttemptRecord>,
    pub created_at: String,
    pub updated_at: String,
}

pub struct ExecutionRun {
    pub record: RunRecord,
    pub policy: WorkspacePolicy,
}

pub enum BeginAttempt {
    Started(AttemptRecord),
    CancelRequested,
    LimitReached,
}
