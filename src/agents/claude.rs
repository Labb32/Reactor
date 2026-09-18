use std::{ffi::OsString, fs, path::Path};

use semver::Version;

use crate::{
    agents::{
        AgentAdapter, AgentCommand, AgentPreflight, launcher::Launcher, output_version,
        sandbox_failure,
    },
    error::{ReactorError, Result, io_error},
    workspace::WorkspacePolicy,
};

const MINIMUM_VERSION: &str = "2.1.259";
const POLICY_FILES: &[&str] = &[
    "CLAUDE.md",
    ".claude/settings.json",
    ".claude/settings.local.json",
    ".mcp.json",
];

pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn policy_files(&self) -> &'static [&'static str] {
        POLICY_FILES
    }

    fn preflight(&self, _workspace: &Path, _policy: &WorkspacePolicy) -> Result<AgentPreflight> {
        let Some(launcher) = Launcher::find("claude") else {
            return Ok(failed("not_installed"));
        };
        let version_output = match launcher.command(["--version"]).output() {
            Ok(output) if output.status.success() => output,
            _ => return Ok(failed("version_check_failed")),
        };
        let Some(version) = output_version(&version_output) else {
            return Ok(failed("version_unknown"));
        };
        if version < Version::new(2, 1, 259) {
            return Ok(failed_with_version("version_unsupported", &version));
        }
        if cfg!(windows) {
            return Ok(failed_with_version("sandbox_unsupported", &version));
        }
        if cfg!(target_os = "linux")
            && (Launcher::find("bwrap").is_none() || Launcher::find("socat").is_none())
        {
            return Ok(failed_with_version("sandbox_dependency_missing", &version));
        }
        let authenticated = launcher
            .command(["auth", "status"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !authenticated {
            return Ok(failed_with_version("authentication_required", &version));
        }

        Ok(AgentPreflight {
            ready: true,
            reason: None,
            capabilities: serde_json::json!({
                "version": version.to_string(),
                "minimum_version": MINIMUM_VERSION,
                "authenticated": true,
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
        let launcher = launcher()?;
        let settings = execution_settings(policy)?;
        let mut arguments = Vec::new();
        arguments.extend(
            [
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "acceptEdits",
                "--permission-prompts",
                "none",
                "--no-session-persistence",
                "--settings",
            ]
            .into_iter()
            .map(OsString::from),
        );
        arguments.push(settings.into());
        let mut disallowed_tools = Vec::new();
        if !policy.mcp.is_allowed() {
            arguments.extend(
                ["--strict-mcp-config", "--mcp-config", "{}"]
                    .into_iter()
                    .map(OsString::from),
            );
            disallowed_tools.push("mcp__*");
        }
        if !policy.network.is_allowed() {
            disallowed_tools.extend(["WebFetch", "WebSearch"]);
        }
        if !disallowed_tools.is_empty() {
            arguments.push("--disallowedTools".into());
            arguments.extend(disallowed_tools.into_iter().map(OsString::from));
        }
        for root in &policy.writable_roots {
            arguments.push("--add-dir".into());
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
            environment: Vec::new(),
            current_dir: workspace.to_owned(),
        })
    }

    fn failure_requires_review(&self, output: &str) -> bool {
        sandbox_failure(output)
    }

    fn token_usage(&self, output: &str) -> Option<u64> {
        output.lines().filter_map(claude_token_usage).next_back()
    }

    fn quota_reset_at(&self, _provider: Option<&str>) -> Result<Option<jiff::Timestamp>> {
        Ok(None)
    }

    fn provider_group(&self, configured: Option<&str>) -> String {
        configured.unwrap_or("anthropic").to_owned()
    }

    fn draft_command(
        &self,
        workspace: &Path,
        schema: &Path,
        _output: &Path,
        model: Option<&str>,
    ) -> Result<AgentCommand> {
        let launcher = launcher()?;
        let schema = fs::read_to_string(schema).map_err(|source| io_error(schema, source))?;
        let mut arguments = Vec::new();
        arguments.extend(
            [
                "-p",
                "--output-format",
                "json",
                "--permission-prompts",
                "none",
                "--no-session-persistence",
                "--safe-mode",
                "--restricted",
                "--tools",
                "Read,Glob,Grep",
                "--json-schema",
            ]
            .into_iter()
            .map(OsString::from),
        );
        arguments.push(schema.into());
        if let Some(model) = model {
            arguments.push("--model".into());
            arguments.push(model.into());
        }
        let arguments = launcher.arguments(arguments);
        Ok(AgentCommand {
            program: launcher.program,
            arguments,
            environment: Vec::new(),
            current_dir: workspace.to_owned(),
        })
    }

    fn read_draft(&self, stdout: &[u8], _output: &Path) -> Result<Vec<u8>> {
        let response: serde_json::Value = serde_json::from_slice(stdout).map_err(|error| {
            ReactorError::InvalidInput(format!("invalid Claude draft response: {error}"))
        })?;
        let draft = response.get("structured_output").ok_or_else(|| {
            ReactorError::InvalidInput("Claude draft response has no structured output".to_owned())
        })?;
        serde_json::to_vec(draft)
            .map_err(|error| ReactorError::OutputSerialization(error.to_string()))
    }
}

fn launcher() -> Result<Launcher> {
    Launcher::find("claude").ok_or_else(|| ReactorError::AgentUnavailable {
        agent: "claude".to_owned(),
        reason: "not_installed".to_owned(),
    })
}

fn execution_settings(policy: &WorkspacePolicy) -> Result<String> {
    let mut settings = serde_json::json!({
        "sandbox": {
            "enabled": true,
            "failIfUnavailable": true,
            "allowUnsandboxedCommands": false,
            "network": {
                "allowedDomains": if policy.network.is_allowed() {
                    serde_json::json!(["*"])
                } else {
                    serde_json::json!([])
                },
                "strictAllowlist": true
            }
        },
        "permissions": { "blockReadsOutsideWorkingDirectories": true }
    });
    if !policy.hooks.is_allowed() {
        settings["disableAllHooks"] = serde_json::Value::Bool(true);
    }
    serde_json::to_string(&settings)
        .map_err(|error| ReactorError::OutputSerialization(error.to_string()))
}

fn claude_token_usage(line: &str) -> Option<u64> {
    let event = serde_json::from_str::<serde_json::Value>(line).ok()?;
    if event.get("type")?.as_str()? != "result" {
        return None;
    }
    let usage = event.get("usage")?;
    let tokens = usage
        .get("input_tokens")?
        .as_u64()?
        .checked_add(usage.get("output_tokens")?.as_u64()?)?
        .checked_add(
            usage
                .get("cache_creation_input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        )?
        .checked_add(
            usage
                .get("cache_read_input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        )?;
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
