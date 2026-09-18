use std::io::{self, IsTerminal, Write};

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

pub fn display_time(value: &str) -> String {
    value
        .parse::<jiff::Timestamp>()
        .map(|timestamp| {
            timestamp
                .to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M:%S %:z")
                .to_string()
        })
        .unwrap_or_else(|_| value.to_owned())
}

pub fn display_status(value: &str) -> String {
    if !colors_enabled() {
        return value.to_owned();
    }
    let color = match value {
        "READY" | "APPROVED" | "ACTIVE" | "SUCCEEDED" | "COMPLETED" => "32",
        "FAILED" | "DEGRADED" | "MISSED" | "LIMIT_REACHED" => "31",
        "REVIEW_REQUIRED" | "IN_REVIEW" | "DRAFT" | "WAITING" | "PAUSED" => "33",
        "RUNNING" | "PREPARING" | "QUEUED" | "REGISTERED" => "36",
        "CANCELLED" => "90",
        _ => return value.to_owned(),
    };
    format!("\x1b[{color}m{value}\x1b[0m")
}

fn colors_enabled() -> bool {
    if std::env::var_os("NO_COLOR").is_some() || !io::stdout().is_terminal() {
        return false;
    }
    #[cfg(windows)]
    {
        std::env::var_os("WT_SESSION").is_some()
            || std::env::var_os("ANSICON").is_some()
            || std::env::var("ConEmuANSI").is_ok_and(|value| value == "ON")
    }
    #[cfg(not(windows))]
    {
        std::env::var("TERM").is_ok_and(|value| value != "dumb")
    }
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

    eprintln!("reactor: {}", escape_control(&error.to_string()));
    Ok(())
}
