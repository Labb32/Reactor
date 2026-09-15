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
            retryable: matches!(self, Self::Database(error) if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)),
        }
    }

    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::InvalidInput(_) | Self::InvalidArgument(_) | Self::InvalidConcurrency { .. } => 2,
            Self::UnsupportedSchema { .. }
            | Self::UninitializedDatabase
            | Self::SchemaMigrationRequired { .. } => 3,
            Self::CursorExpired { .. } | Self::IdempotencyConflict => 4,
            Self::UserDirectoriesUnavailable
            | Self::EmptyPathOverride { .. }
            | Self::Io { .. }
            | Self::InvalidConfig { .. }
            | Self::OutputSerialization(_)
            | Self::InvalidStoredData(_)
            | Self::Database(_)
            | Self::InvalidRunTransition { .. } => 1,
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
