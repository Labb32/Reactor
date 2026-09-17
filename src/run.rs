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
    pub result: Option<RunResult>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RunResult {
    pub state: String,
    pub completed_at: String,
    pub exit_code: Option<i32>,
    pub termination_reason: Option<String>,
    pub log_path: Option<String>,
    pub manifest_path: String,
    pub diff_path: String,
    pub changed_files: Vec<FileChangeSummary>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FileChangeSummary {
    pub root: String,
    pub path: String,
    pub change: String,
    pub kind: String,
    pub before_size: Option<u64>,
    pub after_size: Option<u64>,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
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
