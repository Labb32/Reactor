use std::{io, path::PathBuf};

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

    #[error("configuration value {field} must be greater than zero")]
    InvalidConcurrency { field: &'static str },

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("database schema {found} is newer than supported schema {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },

    #[error("database has not been initialized; run `reactor init`")]
    UninitializedDatabase,

    #[error("invalid run state transition from {from} to {to}")]
    InvalidRunTransition { from: String, to: String },
}

pub fn io_error(path: impl Into<PathBuf>, source: io::Error) -> ReactorError {
    ReactorError::Io {
        path: path.into(),
        source,
    }
}
