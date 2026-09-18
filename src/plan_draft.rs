use std::{
    fs,
    io::{Read, Write},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use crate::{
    agents::AgentKind,
    error::{ReactorError, Result, io_error},
    paths::ReactorPaths,
    plan::AgentPlanDraft,
    process_supervisor::ProcessSupervisor,
    workspace::WorkspaceRecord,
};

pub const DEFAULT_TIMEOUT_SECONDS: u64 = 300;
pub const MAX_TIMEOUT_SECONDS: u64 = 3_600;
const MAX_AGENT_ERROR_BYTES: usize = 16 * 1024;
const MAX_AGENT_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(3);
const DRAFT_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["title", "context", "requirements", "completion_criteria"],
  "properties": {
    "title": { "type": "string" },
    "context": { "type": "string" },
    "requirements": {
      "type": "object",
      "additionalProperties": false,
      "required": ["writable_roots", "network", "mcp", "hooks", "tools"],
      "properties": {
        "writable_roots": { "type": "array", "items": { "type": "string" } },
        "network": { "type": "boolean" },
        "mcp": { "type": "boolean" },
        "hooks": { "type": "boolean" },
        "tools": { "type": "array", "items": { "type": "string" } }
      }
    },
    "completion_criteria": { "type": "array", "items": { "type": "string" } }
  }
}"#;

pub fn generate(
    paths: &ReactorPaths,
    workspace: &WorkspaceRecord,
    agent: AgentKind,
    model: Option<&str>,
    context: &str,
    timeout_seconds: u64,
) -> Result<AgentPlanDraft> {
    let draft_dir = paths.data_dir.join("drafts");
    fs::create_dir_all(&draft_dir).map_err(|source| io_error(&draft_dir, source))?;
    let token = crate::ids::new_id();
    let schema_path = draft_dir.join(format!("{token}.schema.json"));
    let output_path = draft_dir.join(format!("{token}.output.json"));
    fs::write(&schema_path, DRAFT_SCHEMA).map_err(|source| io_error(&schema_path, source))?;

    let result = generate_inner(
        workspace,
        agent,
        model,
        context,
        timeout_seconds,
        &schema_path,
        &output_path,
    );
    let _ = fs::remove_file(&schema_path);
    let _ = fs::remove_file(&output_path);
    result
}

fn generate_inner(
    workspace: &WorkspaceRecord,
    agent: AgentKind,
    model: Option<&str>,
    context: &str,
    timeout_seconds: u64,
    schema_path: &std::path::Path,
    output_path: &std::path::Path,
) -> Result<AgentPlanDraft> {
    let command = agent.adapter().draft_command(
        std::path::Path::new(&workspace.canonical_path),
        schema_path,
        output_path,
        model,
    )?;
    let mut process = Command::new(&command.program);
    process
        .args(&command.arguments)
        .envs(command.environment.iter().map(|(key, value)| (key, value)))
        .current_dir(&command.current_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut supervisor = ProcessSupervisor::spawn(process)
        .map_err(|source| io_error(&command.current_dir, source))?;
    let (stderr_sender, stderr_receiver) = mpsc::sync_channel(1);
    if let Some(mut stderr) = supervisor.take_stderr() {
        thread::spawn(move || {
            let _ = stderr_sender.send(read_agent_error(&mut stderr));
        });
    }
    let (stdout_sender, stdout_receiver) = mpsc::sync_channel(1);
    if let Some(mut stdout) = supervisor.take_stdout() {
        thread::spawn(move || {
            let _ = stdout_sender.send(read_agent_output(&mut stdout));
        });
    }

    let prompt = format!(
        "Create an execution plan for the request below. Do not modify files or perform the task. Use `workspace` for the primary writable root and list only additional paths that are required. Return only the requested structured output.\n\n<request>\n{context}\n</request>"
    );
    let Some(mut stdin) = supervisor.take_stdin() else {
        return Err(ReactorError::AgentUnavailable {
            agent: agent.as_str().to_owned(),
            reason: "stdin_unavailable".to_owned(),
        });
    };
    let (stdin_sender, stdin_receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = stdin
            .write_all(prompt.as_bytes())
            .map_err(|source| io_error("agent stdin", source));
        let _ = stdin_sender.send(result);
    });

    let timeout = Duration::from_secs(timeout_seconds);
    let status = match supervisor
        .wait_for(timeout)
        .map_err(|source| io_error(&command.current_dir, source))?
    {
        Some(status) => status,
        None => {
            if let Err(source) = supervisor.stop(STOP_GRACE_PERIOD) {
                return Err(ReactorError::AgentUnavailable {
                    agent: agent.as_str().to_owned(),
                    reason: format!("plan draft timed out; cleanup failed: {source}"),
                });
            }
            return Err(ReactorError::AgentUnavailable {
                agent: agent.as_str().to_owned(),
                reason: "plan draft timed out".to_owned(),
            });
        }
    };
    let stdin_result = stdin_receiver.recv_timeout(Duration::from_secs(2));
    if !status.success() {
        let reason = stderr_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_else(|_| "Agent exited without an error message".to_owned());
        return Err(ReactorError::AgentUnavailable {
            agent: agent.as_str().to_owned(),
            reason,
        });
    }
    match stdin_result {
        Ok(result) => result?,
        Err(_) => {
            return Err(ReactorError::AgentUnavailable {
                agent: agent.as_str().to_owned(),
                reason: "Agent input did not close".to_owned(),
            });
        }
    }
    let stdout = stdout_receiver
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| ReactorError::AgentUnavailable {
            agent: agent.as_str().to_owned(),
            reason: "Agent output did not close".to_owned(),
        })?
        .map_err(|source| io_error("agent stdout", source))?;
    let bytes = agent.adapter().read_draft(&stdout, output_path)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| ReactorError::InvalidInput(format!("invalid Agent draft: {error}")))
}

fn read_agent_output(reader: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    let mut exceeded = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = MAX_AGENT_OUTPUT_BYTES.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        exceeded |= read > remaining;
    }
    if exceeded {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Agent output exceeds 4 MiB",
        ));
    }
    Ok(bytes)
}

fn read_agent_error(reader: &mut impl Read) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    while let Ok(read) = reader.read(&mut buffer) {
        if read == 0 {
            break;
        }
        let remaining = MAX_AGENT_ERROR_BYTES.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    let message = String::from_utf8_lossy(&bytes)
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .take(1_000)
        .collect::<String>()
        .trim()
        .to_owned();
    if message.is_empty() {
        "Agent exited without an error message".to_owned()
    } else {
        message
    }
}
