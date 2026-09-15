use clap::{Args, Subcommand};

use crate::{
    agents::AgentKind,
    app::App,
    error::Result,
    input, output,
    output::{Envelope, OutputFormat},
    paths::ReactorPaths,
    workspace::{WorkspacePolicy, canonicalize, fingerprint, validate_id},
};

#[derive(Debug, Args)]
pub(super) struct WorkspaceArgs {
    #[command(subcommand)]
    command: WorkspaceCommand,
}

#[derive(Debug, Subcommand)]
enum WorkspaceCommand {
    /// Register a local directory.
    Add(WorkspaceAddArgs),
    /// Run agent readiness checks for a workspace.
    Prepare(WorkspacePrepareArgs),
    /// Show a workspace and refresh its policy state.
    Status(WorkspaceIdArgs),
    /// List registered workspaces.
    List,
}

#[derive(Debug, Args)]
struct WorkspaceAddArgs {
    #[arg(value_name = "PATH")]
    path: String,

    #[arg(long)]
    id: String,

    /// Read workspace policy from JSON or YAML.
    #[arg(long, value_name = "PATH")]
    file: Option<String>,

    #[arg(long)]
    idempotency_key: Option<String>,
}

#[derive(Debug, Args)]
struct WorkspacePrepareArgs {
    workspace_id: String,

    #[arg(long, value_enum, default_value = "codex")]
    agent: AgentKind,

    #[arg(long)]
    idempotency_key: Option<String>,
}

#[derive(Debug, Args)]
struct WorkspaceIdArgs {
    workspace_id: String,
}

pub(super) fn execute(format: OutputFormat, arguments: &WorkspaceArgs) -> Result<()> {
    match &arguments.command {
        WorkspaceCommand::Add(arguments) => workspace_add(format, arguments),
        WorkspaceCommand::Prepare(arguments) => workspace_prepare(format, arguments),
        WorkspaceCommand::Status(arguments) => workspace_status(format, arguments),
        WorkspaceCommand::List => workspace_list(format),
    }
}

fn workspace_add(format: OutputFormat, arguments: &WorkspaceAddArgs) -> Result<()> {
    validate_id(&arguments.id)?;
    let workspace = canonicalize(std::path::Path::new(&arguments.path))?;
    let policy = arguments
        .file
        .as_deref()
        .map(input::read_document::<WorkspacePolicy>)
        .transpose()?
        .unwrap_or_default();
    let policy_fingerprint = fingerprint(&workspace, &policy)?;
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let record = app.store.register_workspace(
        &arguments.id,
        &workspace,
        &policy,
        &policy_fingerprint,
        arguments.idempotency_key.as_deref(),
    )?;
    write_workspace(format, &app.instance_id, &record)
}

fn workspace_prepare(format: OutputFormat, arguments: &WorkspacePrepareArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let record = app.store.get_workspace(&arguments.workspace_id)?;
    let workspace = match canonicalize(std::path::Path::new(&record.canonical_path)) {
        Ok(workspace) => workspace,
        Err(_) => {
            let record = app
                .store
                .mark_workspace_degraded(&arguments.workspace_id, "path_unavailable")?;
            return write_workspace(format, &app.instance_id, &record);
        }
    };
    let policy_fingerprint = match fingerprint(&workspace, &record.policy) {
        Ok(fingerprint) => fingerprint,
        Err(_) => {
            let record = app
                .store
                .mark_workspace_degraded(&arguments.workspace_id, "policy_unreadable")?;
            return write_workspace(format, &app.instance_id, &record);
        }
    };
    let result = arguments.agent.adapter().preflight(&workspace.path)?;
    let record = app.store.record_agent_preflight(
        &arguments.workspace_id,
        arguments.agent.as_str(),
        &result,
        &policy_fingerprint,
        arguments.idempotency_key.as_deref(),
    )?;
    write_workspace(format, &app.instance_id, &record)
}

fn workspace_status(format: OutputFormat, arguments: &WorkspaceIdArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let record = app.store.get_workspace(&arguments.workspace_id)?;
    let refreshed = match canonicalize(std::path::Path::new(&record.canonical_path)) {
        Ok(workspace) => match fingerprint(&workspace, &record.policy) {
            Ok(current) if current == record.policy_fingerprint => record,
            Ok(_) => app
                .store
                .mark_workspace_degraded(&arguments.workspace_id, "policy_changed")?,
            Err(_) => app
                .store
                .mark_workspace_degraded(&arguments.workspace_id, "policy_unreadable")?,
        },
        Err(_) => app
            .store
            .mark_workspace_degraded(&arguments.workspace_id, "path_unavailable")?,
    };
    write_workspace(format, &app.instance_id, &refreshed)
}

fn workspace_list(format: OutputFormat) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let workspaces = app.store.list_workspaces()?;
    match format {
        OutputFormat::Text => {
            for workspace in workspaces {
                println!(
                    "{} {} {}",
                    workspace.id, workspace.status, workspace.canonical_path
                );
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, workspaces))
        }
    }
}

fn write_workspace(
    format: OutputFormat,
    instance_id: &str,
    workspace: &crate::workspace::WorkspaceRecord,
) -> Result<()> {
    match format {
        OutputFormat::Text => {
            println!("workspace: {}", workspace.id);
            println!("path: {}", workspace.canonical_path);
            println!("status: {}", workspace.status);
            for agent in &workspace.agents {
                println!("agent {}: {}", agent.agent_kind, agent.status);
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(instance_id, workspace))
        }
    }
}
