use std::{ffi::OsString, fs, path::Path, process::Command};

use semver::Version;

use crate::{
    agents::{
        AgentAdapter, AgentCommand, AgentPreflight, launcher::{Launcher, BoundedOutput}, output_version,
        sandbox_failure,
    },
    error::{Result, io_error},
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
        let Some(launcher) = Launcher::find("codex") else {
            return Ok(failed("not_installed"));
        };
        let version_output = match launcher.command(["--version"]).bounded_output() {
            Ok(output) if output.status.success() => output,
            _ => return Ok(failed("version_check_failed")),
        };
        let version = output_version(&version_output);
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
            .bounded_output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !valid_config {
            return Ok(failed_with_version("configuration_invalid", &version));
        }

        let authenticated = launcher
            .command(["login", "status"])
            .bounded_output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !authenticated {
            return Ok(failed_with_version("authentication_required", &version));
        }

        let root_help = match launcher.command(["--help"]).bounded_output() {
            Ok(output) if output.status.success() => output,
            _ => return Ok(failed_with_version("non_interactive_unsupported", &version)),
        };
        let exec_help = match launcher.command(["exec", "--help"]).bounded_output() {
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
            .bounded_output()
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
            .bounded_output()
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
        let launcher = Launcher::find("codex").ok_or_else(|| {
            crate::error::ReactorError::AgentUnavailable {
                agent: "codex".to_owned(),
                reason: "not_installed".to_owned(),
            }
        })?;
        let mut arguments = Vec::new();
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
        let arguments = launcher.arguments(arguments);
        Ok(AgentCommand {
            program: launcher.program,
            arguments,
            environment: Vec::new(),
            current_dir: workspace.to_owned(),
        })
    }

    fn failure_requires_review(&self, output: &str) -> bool {
        sandbox_failure(output)
    }

    fn token_usage(&self, output: &str) -> Option<u64> {
        output.lines().filter_map(codex_token_usage).next_back()
    }

    fn quota_reset_at(&self, _provider: Option<&str>) -> Result<Option<jiff::Timestamp>> {
        Ok(None)
    }

    fn provider_group(&self, configured: Option<&str>) -> String {
        configured.unwrap_or("openai").to_owned()
    }

    fn draft_command(
        &self,
        workspace: &Path,
        schema: &Path,
        output: &Path,
        model: Option<&str>,
    ) -> Result<AgentCommand> {
        let launcher = Launcher::find("codex").ok_or_else(|| {
            crate::error::ReactorError::AgentUnavailable {
                agent: "codex".to_owned(),
                reason: "not_installed".to_owned(),
            }
        })?;
        let mut arguments = Vec::new();
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
        let arguments = launcher.arguments(arguments);
        Ok(AgentCommand {
            program: launcher.program,
            arguments,
            environment: Vec::new(),
            current_dir: workspace.to_owned(),
        })
    }

    fn read_draft(&self, _stdout: &[u8], output: &Path) -> Result<Vec<u8>> {
        fs::read(output).map_err(|source| io_error(output, source))
    }
}

fn codex_token_usage(line: &str) -> Option<u64> {
    let event = serde_json::from_str::<serde_json::Value>(line).ok()?;
    if event.get("type")?.as_str()? != "turn.completed" {
        return None;
    }
    let usage = event.get("usage")?;
    let tokens = usage
        .get("input_tokens")?
        .as_u64()?
        .checked_add(usage.get("output_tokens")?.as_u64()?)?;
    i64::try_from(tokens).ok().map(|_| tokens)
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
