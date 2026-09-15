use std::{ffi::OsString, path::Path, process::Command};

use semver::Version;

use crate::{
    agents::{AgentAdapter, AgentCommand, AgentPreflight},
    error::{ReactorError, Result},
    workspace::{AccessMode, WorkspacePolicy},
};

const MINIMUM_VERSION: &str = "0.154.0";

pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn preflight(&self, workspace: &Path) -> Result<AgentPreflight> {
        let version_output = match Command::new("codex").arg("--version").output() {
            Ok(output) if output.status.success() => output,
            Ok(_) => return Ok(failed("version_check_failed")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(failed("not_installed"));
            }
            Err(error) => return Err(ReactorError::InvalidArgument(error.to_string())),
        };
        let version = String::from_utf8_lossy(&version_output.stdout)
            .split_whitespace()
            .find_map(|value| Version::parse(value).ok());
        let minimum = Version::parse(MINIMUM_VERSION)
            .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
        let Some(version) = version else {
            return Ok(failed("version_unknown"));
        };
        if version < minimum {
            return Ok(AgentPreflight {
                ready: false,
                reason: Some("version_unsupported".to_owned()),
                capabilities: serde_json::json!({
                    "version": version.to_string(),
                    "minimum_version": MINIMUM_VERSION
                }),
            });
        }

        let authenticated = Command::new("codex")
            .args(["login", "status"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !authenticated {
            return Ok(failed_with_version("authentication_required", &version));
        }

        let help = Command::new("codex")
            .args(["exec", "--help"])
            .output()
            .map_err(|error| ReactorError::InvalidArgument(error.to_string()))?;
        let help = String::from_utf8_lossy(&help.stdout);
        let non_interactive = help.contains("--json")
            && help.contains("--sandbox")
            && help.contains("--ask-for-approval")
            && help.contains("--dangerously-bypass-approvals-and-sandbox");
        if !non_interactive {
            return Ok(failed_with_version("non_interactive_unsupported", &version));
        }

        let trusted_workspace = Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(["rev-parse", "--is-inside-work-tree"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !trusted_workspace {
            return Ok(failed_with_version("git_workspace_required", &version));
        }

        Ok(AgentPreflight {
            ready: true,
            reason: None,
            capabilities: serde_json::json!({
                "version": version.to_string(),
                "minimum_version": MINIMUM_VERSION,
                "authenticated": true,
                "non_interactive": true,
                "git_workspace": true,
                "sandbox": true,
                "full_access": true
            }),
        })
    }

    fn command(&self, workspace: &Path, policy: &WorkspacePolicy) -> AgentCommand {
        let mut arguments: Vec<OsString> = ["exec", "--json", "--ephemeral", "--color", "never"]
            .into_iter()
            .map(OsString::from)
            .collect();
        match policy.access {
            AccessMode::Sandbox => arguments.extend(
                [
                    "--ask-for-approval",
                    "never",
                    "--sandbox",
                    "workspace-write",
                ]
                .into_iter()
                .map(OsString::from),
            ),
            AccessMode::FullAccess => {
                arguments.push("--dangerously-bypass-approvals-and-sandbox".into());
            }
        }
        arguments.push("-C".into());
        arguments.push(workspace.as_os_str().to_owned());
        arguments.push("-".into());
        AgentCommand {
            program: "codex".into(),
            arguments,
            current_dir: workspace.to_owned(),
        }
    }
}

fn failed(reason: &str) -> AgentPreflight {
    AgentPreflight {
        ready: false,
        reason: Some(reason.to_owned()),
        capabilities: serde_json::json!({ "minimum_version": MINIMUM_VERSION }),
    }
}

fn failed_with_version(reason: &str, version: &Version) -> AgentPreflight {
    AgentPreflight {
        ready: false,
        reason: Some(reason.to_owned()),
        capabilities: serde_json::json!({
            "version": version.to_string(),
            "minimum_version": MINIMUM_VERSION
        }),
    }
}
