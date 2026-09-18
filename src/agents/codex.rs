use std::{ffi::OsString, path::Path, process::Command};

use semver::Version;

use crate::{
    agents::{AgentAdapter, AgentCommand, AgentPreflight},
    error::Result,
    workspace::WorkspacePolicy,
};

const MINIMUM_VERSION: &str = "0.154.0";
const POLICY_FILES: &[&str] = &["AGENTS.md", ".codex/config.toml"];
pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn policy_files(&self) -> &'static [&'static str] {
        POLICY_FILES
    }

    fn preflight(&self, workspace: &Path, policy: &WorkspacePolicy) -> Result<AgentPreflight> {
        let Some(launcher) = Launcher::find() else {
            return Ok(failed("not_installed"));
        };
        let version_output = match launcher.command(["--version"]).output() {
            Ok(output) if output.status.success() => output,
            _ => return Ok(failed("version_check_failed")),
        };
        let version = String::from_utf8_lossy(&version_output.stdout)
            .split_whitespace()
            .find_map(|value| Version::parse(value).ok());
        let minimum = Version::new(0, 154, 0);
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
        let valid_config = launcher
            .command([
                "--strict-config",
                "--config",
                "sandbox_workspace_write.network_access=false",
                "--version",
            ])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !valid_config {
            return Ok(failed_with_version("configuration_invalid", &version));
        }

        let authenticated = launcher
            .command(["login", "status"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !authenticated {
            return Ok(failed_with_version("authentication_required", &version));
        }

        let root_help = match launcher.command(["--help"]).output() {
            Ok(output) if output.status.success() => output,
            _ => return Ok(failed_with_version("non_interactive_unsupported", &version)),
        };
        let exec_help = match launcher.command(["exec", "--help"]).output() {
            Ok(output) if output.status.success() => output,
            _ => return Ok(failed_with_version("non_interactive_unsupported", &version)),
        };
        let root_help = String::from_utf8_lossy(&root_help.stdout);
        let exec_help = String::from_utf8_lossy(&exec_help.stdout);
        let non_interactive = root_help.contains("--ask-for-approval")
            && exec_help.contains("--json")
            && exec_help.contains("--sandbox")
            && exec_help.contains("--add-dir")
            && exec_help.contains("--config")
            && exec_help.contains("--disable")
            && exec_help.contains("--strict-config");
        if !non_interactive {
            return Ok(failed_with_version("non_interactive_unsupported", &version));
        }
        let mcp_supported = launcher
            .command(["mcp", "--help"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        let hooks_supported = root_help.contains("--dangerously-bypass-hook-trust");
        if policy.mcp.is_allowed() && !mcp_supported {
            return Ok(failed_with_version("mcp_unsupported", &version));
        }
        if policy.hooks.is_allowed() && !hooks_supported {
            return Ok(failed_with_version("hooks_unsupported", &version));
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
                "additional_writable_roots": true,
                "network_policy": true,
                "mcp": mcp_supported,
                "hooks": hooks_supported,
                "token_usage": true,
                "quota_reset": false,
                "user_config_loaded": true
            }),
        })
    }

    fn command(
        &self,
        workspace: &Path,
        policy: &WorkspacePolicy,
        model: Option<&str>,
    ) -> Result<AgentCommand> {
        let launcher =
            Launcher::find().ok_or_else(|| crate::error::ReactorError::AgentUnavailable {
                agent: "codex".to_owned(),
                reason: "not_installed".to_owned(),
            })?;
        let mut arguments = launcher.prefix;
        arguments.extend(
            [
                "--ask-for-approval",
                "never",
                "exec",
                "--json",
                "--ephemeral",
                "--strict-config",
                "--color",
                "never",
                "--sandbox",
                "workspace-write",
            ]
            .into_iter()
            .map(OsString::from),
        );
        arguments.push("--config".into());
        arguments.push(
            format!(
                "sandbox_workspace_write.network_access={}",
                policy.network.is_allowed()
            )
            .into(),
        );
        if !policy.mcp.is_allowed() {
            arguments.push("--config".into());
            arguments.push("mcp_servers={}".into());
        }
        if !policy.hooks.is_allowed() {
            arguments.push("--disable".into());
            arguments.push("hooks".into());
        }
        for root in &policy.writable_roots {
            arguments.push("--add-dir".into());
            arguments.push(root.into());
        }
        if let Some(model) = model {
            arguments.push("--model".into());
            arguments.push(model.into());
        }
        arguments.push("-C".into());
        arguments.push(workspace.as_os_str().to_owned());
        arguments.push("-".into());
        Ok(AgentCommand {
            program: launcher.program,
            arguments,
            current_dir: workspace.to_owned(),
        })
    }

    fn failure_requires_review(&self, output: &str) -> bool {
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

    fn token_usage(&self, output: &str) -> Option<u64> {
        output.lines().filter_map(codex_token_usage).next_back()
    }

    fn quota_reset_at(&self, _provider: Option<&str>) -> Result<Option<jiff::Timestamp>> {
        Ok(None)
    }

    fn draft_command(
        &self,
        workspace: &Path,
        schema: &Path,
        output: &Path,
        model: Option<&str>,
    ) -> Result<AgentCommand> {
        let launcher =
            Launcher::find().ok_or_else(|| crate::error::ReactorError::AgentUnavailable {
                agent: "codex".to_owned(),
                reason: "not_installed".to_owned(),
            })?;
        let mut arguments = launcher.prefix;
        arguments.extend(
            [
                "--ask-for-approval",
                "never",
                "exec",
                "--ephemeral",
                "--ignore-user-config",
                "--config",
                "mcp_servers={}",
                "--disable",
                "hooks",
                "--strict-config",
                "--color",
                "never",
                "--sandbox",
                "read-only",
                "--output-schema",
            ]
            .into_iter()
            .map(OsString::from),
        );
        arguments.push(schema.into());
        arguments.push("--output-last-message".into());
        arguments.push(output.into());
        if let Some(model) = model {
            arguments.push("--model".into());
            arguments.push(model.into());
        }
        arguments.push("-C".into());
        arguments.push(workspace.into());
        arguments.push("-".into());
        Ok(AgentCommand {
            program: launcher.program,
            arguments,
            current_dir: workspace.to_owned(),
        })
    }
}

fn codex_token_usage(line: &str) -> Option<u64> {
    let event = serde_json::from_str::<serde_json::Value>(line).ok()?;
    if event.get("type")?.as_str()? != "turn.completed" {
        return None;
    }
    let usage = event.get("usage")?;
    usage
        .get("input_tokens")?
        .as_u64()?
        .checked_add(usage.get("output_tokens")?.as_u64()?)
}

struct Launcher {
    program: OsString,
    prefix: Vec<OsString>,
}

impl Launcher {
    fn command<const N: usize>(&self, arguments: [&str; N]) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.prefix).args(arguments);
        command
    }

    #[cfg(not(windows))]
    fn find() -> Option<Self> {
        Some(Self {
            program: find_on_path("codex")?,
            prefix: Vec::new(),
        })
    }

    #[cfg(windows)]
    fn find() -> Option<Self> {
        if let Some(program) = find_on_path("codex.exe") {
            return Some(Self {
                program,
                prefix: Vec::new(),
            });
        }
        let script = find_on_path("codex.ps1")?;
        Some(Self {
            program: find_on_path("powershell.exe")?,
            prefix: vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-File".into(),
                script,
            ],
        })
    }
}

fn find_on_path(name: &str) -> Option<OsString> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| dunce::canonicalize(candidate).ok())
        .map(|candidate| candidate.into_os_string())
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
