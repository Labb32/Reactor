use std::{ffi::OsString, path::Path, process::Command};

use semver::Version;

use crate::{
    agents::{AgentAdapter, AgentCommand, AgentPreflight},
    error::Result,
    workspace::{AccessMode, WorkspacePolicy},
};

const MINIMUM_VERSION: &str = "0.154.0";
const POLICY_FILES: &[&str] = &["AGENTS.md", ".codex/config.toml"];
const ENVIRONMENT_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "SYSTEMROOT",
    "WINDIR",
    "TEMP",
    "TMP",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "CODEX_HOME",
    "LANG",
    "LC_ALL",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn policy_files(&self) -> &'static [&'static str] {
        POLICY_FILES
    }

    fn preflight(&self, workspace: &Path) -> Result<AgentPreflight> {
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
            && exec_help.contains("--ignore-user-config")
            && exec_help.contains("--config")
            && exec_help.contains("--disable")
            && exec_help.contains("--strict-config")
            && exec_help.contains("--dangerously-bypass-approvals-and-sandbox");
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
                "full_access": true,
                "user_config_loaded": false,
                "mcp_enabled": false,
                "hooks_enabled": false
            }),
        })
    }

    fn command(&self, workspace: &Path, policy: &WorkspacePolicy) -> Result<AgentCommand> {
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
                "--ignore-user-config",
                "--config",
                "mcp_servers={}",
                "--disable",
                "hooks",
                "--strict-config",
                "--color",
                "never",
            ]
            .into_iter()
            .map(OsString::from),
        );
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
        Ok(AgentCommand {
            program: launcher.program,
            arguments,
            current_dir: workspace.to_owned(),
            environment_allowlist: ENVIRONMENT_ALLOWLIST,
        })
    }
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
