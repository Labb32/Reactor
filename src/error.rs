use std::{io, path::PathBuf};

use serde::Serialize;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, ReactorError>;

#[derive(Debug, Error)]
pub enum ReactorError {
    #[error("could not determine the current user's standard directories")]
    UserDirectoriesUnavailable,

    #[error("environment variable {name} must not be empty")]
    EmptyPathOverride { name: &'static str },

    #[error("failed to access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("invalid configuration at {path}: {source}")]
    InvalidConfig {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("failed to parse input document: {0}")]
    InvalidInput(String),

    #[error("failed to serialize output: {0}")]
    OutputSerialization(String),

    #[error("stored data is invalid: {0}")]
    InvalidStoredData(String),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("configuration value {field} must be greater than zero")]
    InvalidConcurrency { field: &'static str },

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("database schema {found} is newer than supported schema {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },

    #[error("database has not been initialized; run `reactor init`")]
    UninitializedDatabase,

    #[error("database schema {found} must be migrated to schema {supported}")]
    SchemaMigrationRequired { found: u32, supported: u32 },

    #[error("event cursor {requested} has expired; the oldest available cursor is {oldest}")]
    CursorExpired { requested: i64, oldest: i64 },

    #[error("idempotency key was already used with a different request")]
    IdempotencyConflict,

    #[error("invalid run state transition from {from} to {to}")]
    InvalidRunTransition { from: String, to: String },

    #[error("run {0} was not found")]
    RunNotFound(String),

    #[error("workspace ID is invalid: {0}")]
    InvalidWorkspaceId(String),

    #[error("workspace path is invalid: {0}")]
    InvalidWorkspacePath(String),

    #[error("sandbox policy is invalid: {0}")]
    InvalidSandboxPolicy(String),

    #[error("workspace overlaps registered workspace {workspace_id}")]
    WorkspaceOverlap { workspace_id: String },

    #[error("workspace {0} was not found")]
    WorkspaceNotFound(String),

    #[error("agent {agent} is not ready for workspace {workspace_id}: {reason}")]
    AgentNotReady {
        workspace_id: String,
        agent: String,
        reason: String,
    },

    #[error("agent {agent} is unavailable: {reason}")]
    AgentUnavailable { agent: String, reason: String },

    #[error("plan is invalid: {0}")]
    InvalidPlan(String),

    #[error("plan {0} was not found")]
    PlanNotFound(String),

    #[error("version {version} of plan {plan_id} was not found")]
    PlanVersionNotFound { plan_id: String, version: u32 },

    #[error("plan {plan_id} changed from version {expected} to {found} before approval")]
    PlanVersionChanged {
        plan_id: String,
        expected: u32,
        found: u32,
    },

    #[error("plan {plan_id} cannot transition from {from} to {to}")]
    InvalidPlanTransition {
        plan_id: String,
        from: String,
        to: String,
    },

    #[error("plan requirement is unavailable: {requirement}")]
    PlanRequirementUnavailable { requirement: String },

    #[error("workspace policy changed after the plan version was created")]
    PlanPolicyChanged,

    #[error("schedule is invalid: {0}")]
    InvalidSchedule(String),

    #[error("schedule {0} was not found")]
    ScheduleNotFound(String),

    #[error("schedule {schedule_id} cannot transition from {from} to {to}")]
    InvalidScheduleTransition {
        schedule_id: String,
        from: String,
        to: String,
    },
}

#[derive(Debug, Serialize)]
pub struct ErrorDetails {
    pub code: &'static str,
    pub message: String,
    pub details: serde_json::Value,
    pub retryable: bool,
}

impl ReactorError {
    pub fn details(&self) -> ErrorDetails {
        ErrorDetails {
            code: self.code(),
            message: self.to_string(),
            details: self.context(),
            retryable: matches!(
                self,
                Self::Database(error)
                    if matches!(
                        error.sqlite_error_code(),
                        Some(
                            rusqlite::ErrorCode::DatabaseBusy
                                | rusqlite::ErrorCode::DatabaseLocked
                        )
                    )
            ),
        }
    }

    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::InvalidInput(_)
            | Self::InvalidArgument(_)
            | Self::InvalidConcurrency { .. }
            | Self::InvalidWorkspaceId(_)
            | Self::InvalidWorkspacePath(_)
            | Self::InvalidSandboxPolicy(_)
            | Self::InvalidPlan(_)
            | Self::InvalidSchedule(_) => 2,
            Self::UnsupportedSchema { .. }
            | Self::UninitializedDatabase
            | Self::SchemaMigrationRequired { .. } => 3,
            Self::CursorExpired { .. }
            | Self::IdempotencyConflict
            | Self::InvalidRunTransition { .. }
            | Self::RunNotFound(_)
            | Self::WorkspaceOverlap { .. }
            | Self::WorkspaceNotFound(_)
            | Self::AgentNotReady { .. }
            | Self::AgentUnavailable { .. }
            | Self::PlanNotFound(_)
            | Self::PlanVersionNotFound { .. }
            | Self::PlanVersionChanged { .. }
            | Self::InvalidPlanTransition { .. }
            | Self::PlanRequirementUnavailable { .. }
            | Self::PlanPolicyChanged
            | Self::ScheduleNotFound(_)
            | Self::InvalidScheduleTransition { .. } => 4,
            Self::UserDirectoriesUnavailable
            | Self::EmptyPathOverride { .. }
            | Self::Io { .. }
            | Self::InvalidConfig { .. }
            | Self::OutputSerialization(_)
            | Self::InvalidStoredData(_)
            | Self::Database(_) => 1,
        }
    }

    const fn code(&self) -> &'static str {
        match self {
            Self::UserDirectoriesUnavailable => "user_directories_unavailable",
            Self::EmptyPathOverride { .. } => "empty_path_override",
            Self::Io { .. } => "io_error",
            Self::InvalidConfig { .. } => "invalid_config",
            Self::InvalidInput(_) => "invalid_input",
            Self::OutputSerialization(_) => "output_serialization",
            Self::InvalidStoredData(_) => "invalid_stored_data",
            Self::InvalidArgument(_) => "invalid_argument",
            Self::InvalidConcurrency { .. } => "invalid_concurrency",
            Self::Database(_) => "database_error",
            Self::UnsupportedSchema { .. } => "unsupported_schema",
            Self::UninitializedDatabase => "uninitialized_database",
            Self::SchemaMigrationRequired { .. } => "schema_migration_required",
            Self::CursorExpired { .. } => "cursor_expired",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::InvalidRunTransition { .. } => "invalid_run_transition",
            Self::RunNotFound(_) => "run_not_found",
            Self::InvalidWorkspaceId(_) => "invalid_workspace_id",
            Self::InvalidWorkspacePath(_) => "invalid_workspace_path",
            Self::InvalidSandboxPolicy(_) => "invalid_sandbox_policy",
            Self::WorkspaceOverlap { .. } => "workspace_overlap",
            Self::WorkspaceNotFound(_) => "workspace_not_found",
            Self::AgentNotReady { .. } => "agent_not_ready",
            Self::AgentUnavailable { .. } => "agent_unavailable",
            Self::InvalidPlan(_) => "invalid_plan",
            Self::PlanNotFound(_) => "plan_not_found",
            Self::PlanVersionNotFound { .. } => "plan_version_not_found",
            Self::PlanVersionChanged { .. } => "plan_version_changed",
            Self::InvalidPlanTransition { .. } => "invalid_plan_transition",
            Self::PlanRequirementUnavailable { .. } => "plan_requirement_unavailable",
            Self::PlanPolicyChanged => "plan_policy_changed",
            Self::InvalidSchedule(_) => "invalid_schedule",
            Self::ScheduleNotFound(_) => "schedule_not_found",
            Self::InvalidScheduleTransition { .. } => "invalid_schedule_transition",
        }
    }

    fn context(&self) -> serde_json::Value {
        match self {
            Self::EmptyPathOverride { name } => serde_json::json!({ "name": name }),
            Self::InvalidConcurrency { field } => serde_json::json!({ "field": field }),
            Self::UnsupportedSchema { found, supported }
            | Self::SchemaMigrationRequired { found, supported } => {
                serde_json::json!({ "found": found, "supported": supported })
            }
            Self::CursorExpired { requested, oldest } => {
                serde_json::json!({ "requested": requested, "oldest": oldest })
            }
            Self::InvalidRunTransition { from, to } => {
                serde_json::json!({ "from": from, "to": to })
            }
            Self::RunNotFound(run_id) => serde_json::json!({ "run_id": run_id }),
            Self::WorkspaceOverlap { workspace_id } => {
                serde_json::json!({ "workspace_id": workspace_id })
            }
            Self::WorkspaceNotFound(workspace_id) => {
                serde_json::json!({ "workspace_id": workspace_id })
            }
            Self::AgentNotReady {
                workspace_id,
                agent,
                reason,
            } => serde_json::json!({
                "workspace_id": workspace_id,
                "agent": agent,
                "reason": reason
            }),
            Self::AgentUnavailable { agent, reason } => {
                serde_json::json!({ "agent": agent, "reason": reason })
            }
            Self::PlanNotFound(plan_id) => serde_json::json!({ "plan_id": plan_id }),
            Self::PlanVersionNotFound { plan_id, version } => {
                serde_json::json!({ "plan_id": plan_id, "version": version })
            }
            Self::PlanVersionChanged {
                plan_id,
                expected,
                found,
            } => serde_json::json!({
                "plan_id": plan_id,
                "expected": expected,
                "found": found
            }),
            Self::InvalidPlanTransition { plan_id, from, to } => {
                serde_json::json!({ "plan_id": plan_id, "from": from, "to": to })
            }
            Self::PlanRequirementUnavailable { requirement } => {
                serde_json::json!({ "requirement": requirement })
            }
            Self::ScheduleNotFound(schedule_id) => {
                serde_json::json!({ "schedule_id": schedule_id })
            }
            Self::InvalidScheduleTransition {
                schedule_id,
                from,
                to,
            } => serde_json::json!({
                "schedule_id": schedule_id,
                "from": from,
                "to": to
            }),
            _ => serde_json::json!({}),
        }
    }
}

pub fn io_error(path: impl Into<PathBuf>, source: io::Error) -> ReactorError {
    ReactorError::Io {
        path: path.into(),
        source,
    }
}
