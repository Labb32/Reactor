use std::{ffi::OsString, path::Path};

use semver::Version;

use crate::{
    agents::{
        AgentAdapter, AgentCommand, AgentPreflight, launcher::Launcher, output_version,
        sandbox_failure,
    },
    error::{ReactorError, Result},
    workspace::WorkspacePolicy,
};

const MINIMUM_VERSION: &str = "0.24.0";
const POLICY_FILES: &[&str] = &["QWEN.md", ".qwen/settings.json"];

pub struct QwenAdapter;

impl AgentAdapter for QwenAdapter {
    fn policy_files(&self) -> &'static [&'static str] {
        POLICY_FILES
    }

    fn preflight(&self, _workspace: &Path, policy: &WorkspacePolicy) -> Result<AgentPreflight> {
        let Some(launcher) = Launcher::find("qwen") else {
            return Ok(failed("not_installed"));
        };
        let version_output = match launcher.command(["--version"]).output() {
            Ok(output) if output.status.success() => output,
            _ => return Ok(failed("version_check_failed")),
        };
        let Some(version) = output_version(&version_output) else {
            return Ok(failed("version_unknown"));
        };
        if version < Version::new(0, 24, 0) {
            return Ok(failed_with_version("version_unsupported", &version));
        }
        if !policy.hooks.is_allowed() {
            return Ok(failed_with_version("hooks_policy_unsupported", &version));
        }
        if !cfg!(target_os = "macos")
            && Launcher::find("docker").is_none()
            && Launcher::find("podman").is_none()
        {
            return Ok(failed_with_version("sandbox_runtime_missing", &version));
        }

        Ok(AgentPreflight {
            ready: true,
            reason: None,
            capabilities: serde_json::json!({
                "version": version.to_string(),
                "minimum_version": MINIMUM_VERSION,
                "authentication_check": false,
                "non_interactive": true,
                "sandbox": true,
                "additional_writable_roots": true,
                "network_policy": true,
                "mcp": true,
                "hooks": true,
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
        if !policy.hooks.is_allowed() {
            return Err(ReactorError::AgentUnavailable {
                agent: "qwen".to_owned(),
                reason: "hooks_policy_unsupported".to_owned(),
            });
        }
        let launcher = launcher()?;
        let mut arguments = Vec::new();
        arguments.extend(
            [
                "--output-format",
                "stream-json",
                "--approval-mode",
                "yolo",
                "--sandbox",
            ]
            .into_iter()
            .map(OsString::from),
        );
        if !policy.mcp.is_allowed() {
            arguments.push("--allowed-mcp-server-names".into());
            arguments.push("__reactor_none__".into());
        }
        for root in &policy.writable_roots {
            arguments.push("--include-directories".into());
            arguments.push(root.into());
        }
        if let Some(model) = model {
            arguments.push("--model".into());
            arguments.push(model.into());
        }
        let arguments = launcher.arguments(arguments);
        Ok(AgentCommand {
            program: launcher.program,
            arguments,
            environment: sandbox_environment(policy.network.is_allowed()),
            current_dir: workspace.to_owned(),
        })
    }

    fn failure_requires_review(&self, output: &str) -> bool {
        sandbox_failure(output)
    }

    fn token_usage(&self, output: &str) -> Option<u64> {
        output.lines().filter_map(qwen_token_usage).next_back()
    }

    fn quota_reset_at(&self, _provider: Option<&str>) -> Result<Option<jiff::Timestamp>> {
        Ok(None)
    }

    fn provider_group(&self, configured: Option<&str>) -> String {
        configured.unwrap_or("qwen").to_owned()
    }

    fn draft_command(
        &self,
        workspace: &Path,
        schema: &Path,
        _output: &Path,
        model: Option<&str>,
    ) -> Result<AgentCommand> {
        let launcher = launcher()?;
        let mut arguments = Vec::new();
        arguments.extend(
            [
                "--safe-mode",
                "--approval-mode",
                "plan",
                "--sandbox",
                "--json-schema",
            ]
            .into_iter()
            .map(OsString::from),
        );
        arguments.push(format!("@{}", schema.display()).into());
        if let Some(model) = model {
            arguments.push("--model".into());
            arguments.push(model.into());
        }
        let arguments = launcher.arguments(arguments);
        Ok(AgentCommand {
            program: launcher.program,
            arguments,
            environment: sandbox_environment(false),
            current_dir: workspace.to_owned(),
        })
    }

    fn read_draft(&self, stdout: &[u8], _output: &Path) -> Result<Vec<u8>> {
        Ok(stdout.to_vec())
    }
}

fn launcher() -> Result<Launcher> {
    Launcher::find("qwen").ok_or_else(|| ReactorError::AgentUnavailable {
        agent: "qwen".to_owned(),
        reason: "not_installed".to_owned(),
    })
}

fn sandbox_environment(network: bool) -> Vec<(OsString, OsString)> {
    let mut environment = vec![("QWEN_SANDBOX".into(), "true".into())];
    if network {
        environment.push(("SEATBELT_PROFILE".into(), "permissive-open".into()));
    } else {
        environment.push(("SEATBELT_PROFILE".into(), "permissive-closed".into()));
        environment.push(("SANDBOX_FLAGS".into(), "--network none".into()));
    }
    environment
}

fn qwen_token_usage(line: &str) -> Option<u64> {
    let event = serde_json::from_str::<serde_json::Value>(line).ok()?;
    if event.get("type")?.as_str()? != "result" {
        return None;
    }
    let usage = event.get("usage")?;
    let tokens = usage
        .get("total_tokens")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            usage
                .get("input_tokens")?
                .as_u64()?
                .checked_add(usage.get("output_tokens")?.as_u64()?)
        })?;
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
