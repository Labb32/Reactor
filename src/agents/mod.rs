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
pub(crate) mod launcher;
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

/// Forward shared login settings without unrelated secrets.
pub fn environment(kind: Option<AgentKind>) -> Vec<(OsString, OsString)> {
    let common = [
        "PATH",
        "PATHEXT",
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "TEMP",
        "TMP",
        "TMPDIR",
        "HOME",
        "USER",
        "LOGNAME",
        "USERNAME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "PSModulePath",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
        "SSH_AUTH_SOCK",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "CURL_CA_BUNDLE",
        "REQUESTS_CA_BUNDLE",
        "NODE_EXTRA_CA_CERTS",
    ];
    let provider: &[&str] = match kind {
        Some(AgentKind::Codex) => &[
            "CODEX_HOME",
            "CODEX_API_KEY",
            "OPENAI_API_KEY",
            "OPENAI_BASE_URL",
            "OPENAI_ORG_ID",
            "OPENAI_PROJECT_ID",
        ],
        Some(AgentKind::Claude) => &[
            "CLAUDE_CONFIG_DIR",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
        ],
        Some(AgentKind::Qwen) => &[
            "QWEN_HOME",
            "QWEN_API_KEY",
            "DASHSCOPE_API_KEY",
            "OPENAI_API_KEY",
            "OPENAI_BASE_URL",
            "GEMINI_API_KEY",
        ],
        None => &[],
    };
    common
        .iter()
        .chain(provider.iter())
        .filter_map(|key| std::env::var_os(key).map(|value| (OsString::from(*key), value)))
        .collect()
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

    fn session_command(
        &self,
        workspace: &Path,
        policy: &WorkspacePolicy,
        model: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<AgentCommand> {
        if session_id.is_some() {
            return Err(crate::error::ReactorError::InvalidInput(
                "session resume unsupported".to_owned(),
            ));
        }
        self.command(workspace, policy, model)
    }

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
