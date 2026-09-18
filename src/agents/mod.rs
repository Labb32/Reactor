use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use clap::ValueEnum;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::{error::Result, workspace::WorkspacePolicy};

mod claude;
mod codex;
mod launcher;
mod qwen;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
    Qwen,
}

impl AgentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Qwen => "qwen",
        }
    }

    pub fn adapter(self) -> &'static dyn AgentAdapter {
        match self {
            Self::Claude => &claude::ClaudeAdapter,
            Self::Codex => &codex::CodexAdapter,
            Self::Qwen => &qwen::QwenAdapter,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "qwen" => Some(Self::Qwen),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AgentPreflight {
    pub ready: bool,
    pub reason: Option<String>,
    pub capabilities: serde_json::Value,
}

#[derive(Debug)]
pub struct AgentCommand {
    pub program: OsString,
    pub arguments: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
    pub current_dir: PathBuf,
}

pub trait AgentAdapter {
    fn policy_files(&self) -> &'static [&'static str];

    fn preflight(
        &self,
        workspace: &std::path::Path,
        policy: &WorkspacePolicy,
    ) -> Result<AgentPreflight>;

    fn command(
        &self,
        workspace: &std::path::Path,
        policy: &WorkspacePolicy,
        model: Option<&str>,
    ) -> Result<AgentCommand>;

    fn failure_requires_review(&self, output: &str) -> bool;

    fn token_usage(&self, output: &str) -> Option<u64>;

    fn quota_reset_at(&self, provider: Option<&str>) -> Result<Option<jiff::Timestamp>>;

    fn provider_group(&self, configured: Option<&str>) -> String;

    fn draft_command(
        &self,
        workspace: &Path,
        schema: &Path,
        output: &Path,
        model: Option<&str>,
    ) -> Result<AgentCommand>;

    fn read_draft(&self, stdout: &[u8], output: &Path) -> Result<Vec<u8>>;
}

fn output_version(output: &std::process::Output) -> Option<Version> {
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .find_map(|value| Version::parse(value.trim_start_matches('v')).ok())
}

fn sandbox_failure(output: &str) -> bool {
    let output = output.to_ascii_lowercase();
    output.contains("permission denied")
        || output.contains("operation not permitted")
        || output.contains("read-only file system")
        || output.contains("outside the workspace")
        || output.contains("approval required")
        || output.contains("requires approval")
        || (output.contains("sandbox")
            && (output.contains("denied")
                || output.contains("blocked")
                || output.contains("violation")))
}
