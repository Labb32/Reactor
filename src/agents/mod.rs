use std::{ffi::OsString, path::PathBuf};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::{error::Result, workspace::WorkspacePolicy};

mod codex;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Codex,
}

impl AgentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
        }
    }

    pub fn adapter(self) -> &'static dyn AgentAdapter {
        match self {
            Self::Codex => &codex::CodexAdapter,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "codex" => Some(Self::Codex),
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
#[allow(dead_code)]
pub struct AgentCommand {
    pub program: OsString,
    pub arguments: Vec<OsString>,
    pub current_dir: PathBuf,
    /// Clear inherited variables, then restore only these names.
    pub environment_allowlist: &'static [&'static str],
}

pub trait AgentAdapter {
    fn policy_files(&self) -> &'static [&'static str];

    fn preflight(&self, workspace: &std::path::Path) -> Result<AgentPreflight>;

    #[allow(dead_code)]
    fn command(
        &self,
        workspace: &std::path::Path,
        policy: &WorkspacePolicy,
    ) -> Result<AgentCommand>;
}
