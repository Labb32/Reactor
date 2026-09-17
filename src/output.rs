use std::io::{self, Write};

use clap::ValueEnum;
use serde::Serialize;

use crate::error::{ReactorError, Result};

pub const PUBLIC_SCHEMA_VERSION: &str = "1.0";

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    Jsonl,
}

#[derive(Debug, Serialize)]
pub struct Envelope<T> {
    pub schema_version: &'static str,
    pub instance_id: String,
    pub timestamp: String,
    pub data: T,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    schema_version: &'static str,
    instance_id: Option<String>,
    timestamp: String,
    error: crate::error::ErrorDetails,
}

impl<T> Envelope<T> {
    pub fn new(instance_id: impl Into<String>, data: T) -> Self {
        Self {
            schema_version: PUBLIC_SCHEMA_VERSION,
            instance_id: instance_id.into(),
            timestamp: crate::time::now_utc(),
            data,
        }
    }
}

pub fn write_json<T: Serialize>(value: &T) -> Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, value)
        .map_err(|error| ReactorError::OutputSerialization(error.to_string()))?;
    output
        .write_all(b"\n")
        .map_err(|error| crate::error::io_error("stdout", error))?;
    Ok(())
}

pub fn escape_control(value: &str) -> String {
    value.chars().fold(String::new(), |mut output, character| {
        if character.is_control() {
            output.extend(character.escape_default());
        } else {
            output.push(character);
        }
        output
    })
}

pub fn write_error(
    format: OutputFormat,
    instance_id: Option<&str>,
    error: &ReactorError,
) -> Result<()> {
    if matches!(format, OutputFormat::Json | OutputFormat::Jsonl) {
        return write_json(&ErrorEnvelope {
            schema_version: PUBLIC_SCHEMA_VERSION,
            instance_id: instance_id.map(str::to_owned),
            timestamp: crate::time::now_utc(),
            error: error.details(),
        });
    }

    eprintln!("reactor: {error}");
    Ok(())
}
