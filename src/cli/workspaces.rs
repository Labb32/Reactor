use clap::{Args, Subcommand};

use crate::{
    agents::{AgentKind, AgentPreflight},
    app::App,
    error::{ReactorError, Result},
    input, output,
    output::{Envelope, OutputFormat},
    paths::ReactorPaths,
    workspace::{
        CanonicalWorkspace, WorkspacePolicy, WorkspaceRecord, canonicalize, fingerprint,
        normalize_policy, validate_id, validate_storage_isolation,
    },
};

struct WorkspaceInspection {
    workspace: CanonicalWorkspace,
    policy: WorkspacePolicy,
    fingerprint: String,
}

#[derive(Clone, Copy)]
enum InspectionFailure {
    PathUnavailable,
    PolicyUnreadable,
    StorageOverlap,
}

impl InspectionFailure {
    const fn reason(self) -> &'static str {
        match self {
            Self::PathUnavailable => "path_unavailable",
            Self::PolicyUnreadable => "policy_unreadable",
            Self::StorageOverlap => "storage_overlap",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::PathUnavailable => "workspace path is unavailable",
            Self::PolicyUnreadable => "workspace policy is unreadable",
            Self::StorageOverlap => "workspace write scope overlaps Reactor storage",
        }
    }
}

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
    let policy = normalize_policy(&workspace, &policy)?;
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    validate_storage_isolation(&workspace, &policy, &paths)?;
    let policy_fingerprint = fingerprint(&workspace, &policy, &[])?;
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
    let inspection = match inspect_workspace(&paths, &record) {
        Ok(inspection) => inspection,
        Err(failure) => {
            let record = app
                .store
                .mark_workspace_degraded(&arguments.workspace_id, failure.reason())?;
            return write_workspace(format, &app.instance_id, &record);
        }
    };
    if inspection.fingerprint != record.policy_fingerprint {
        if record.policy_fingerprint.is_empty() {
            app.store.adopt_migrated_workspace_policy(
                &arguments.workspace_id,
                &inspection.fingerprint,
            )?;
        } else {
            let record = app
                .store
                .mark_workspace_degraded(&arguments.workspace_id, "policy_changed")?;
            return write_workspace(format, &app.instance_id, &record);
        }
    }
    let adapter = arguments.agent.adapter();
    let agent_fingerprint = match fingerprint(
        &inspection.workspace,
        &inspection.policy,
        adapter.policy_files(),
    ) {
        Ok(fingerprint) => fingerprint,
        Err(_) => {
            let preflight = AgentPreflight {
                ready: false,
                reason: Some("agent_policy_unreadable".to_owned()),
                capabilities: serde_json::json!({}),
            };
            let record = app.store.record_agent_preflight(
                &arguments.workspace_id,
                arguments.agent.as_str(),
                &preflight,
                &inspection.fingerprint,
                arguments.idempotency_key.as_deref(),
            )?;
            return write_workspace(format, &app.instance_id, &record);
        }
    };
    let result = adapter.preflight(&inspection.workspace.path, &inspection.policy)?;
    let record = app.store.record_agent_preflight(
        &arguments.workspace_id,
        arguments.agent.as_str(),
        &result,
        &agent_fingerprint,
        arguments.idempotency_key.as_deref(),
    )?;
    write_workspace(format, &app.instance_id, &record)
}

fn workspace_status(format: OutputFormat, arguments: &WorkspaceIdArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let record = app.store.get_workspace(&arguments.workspace_id)?;
    let inspection = match inspect_workspace(&paths, &record) {
        Ok(inspection) => inspection,
        Err(failure) => {
            let record = app
                .store
                .mark_workspace_degraded(&arguments.workspace_id, failure.reason())?;
            return write_workspace(format, &app.instance_id, &record);
        }
    };
    if inspection.fingerprint != record.policy_fingerprint {
        let record = app
            .store
            .mark_workspace_degraded(&arguments.workspace_id, "policy_changed")?;
        return write_workspace(format, &app.instance_id, &record);
    }

    let mut refreshed = record.clone();
    for agent in &record.agents {
        if agent.status != "READY" {
            continue;
        }
        let Some(kind) = AgentKind::parse(&agent.agent_kind) else {
            refreshed = app.store.mark_workspace_agent_degraded(
                &arguments.workspace_id,
                &agent.agent_kind,
                "adapter_unavailable",
            )?;
            continue;
        };
        let adapter = kind.adapter();
        let changed = match fingerprint(
            &inspection.workspace,
            &inspection.policy,
            adapter.policy_files(),
        ) {
            Ok(current) => Some(current.as_str()) != agent.policy_fingerprint.as_deref(),
            Err(_) => true,
        };
        if changed {
            refreshed = app.store.mark_workspace_agent_degraded(
                &arguments.workspace_id,
                &agent.agent_kind,
                "agent_policy_changed",
            )?;
        }
    }
    write_workspace(format, &app.instance_id, &refreshed)
}

fn workspace_list(format: OutputFormat) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let workspaces = app.store.list_workspaces()?;
    match format {
        OutputFormat::Text => {
            let rows = workspaces
                .into_iter()
                .map(|workspace| {
                    vec![
                        workspace.id,
                        output::display_status(&workspace.status),
                        workspace.canonical_path,
                    ]
                })
                .collect::<Vec<_>>();
            output::write_table(&["ID", "STATUS", "PATH"], &rows);
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, workspaces))
        }
    }
}

pub(super) fn require_ready(
    paths: &ReactorPaths,
    app: &mut App,
    workspace_id: &str,
    agent: AgentKind,
) -> Result<WorkspaceRecord> {
    let record = app.store.get_workspace(workspace_id)?;
    let inspection = match inspect_workspace(paths, &record) {
        Ok(inspection) => inspection,
        Err(failure) => {
            app.store
                .mark_workspace_degraded(workspace_id, failure.reason())?;
            return Err(agent_not_ready(workspace_id, agent, failure.message()));
        }
    };
    if inspection.fingerprint != record.policy_fingerprint {
        app.store
            .mark_workspace_degraded(workspace_id, "policy_changed")?;
        return Err(agent_not_ready(
            workspace_id,
            agent,
            "workspace policy changed",
        ));
    }

    let adapter = agent.adapter();
    let agent_fingerprint = match fingerprint(
        &inspection.workspace,
        &inspection.policy,
        adapter.policy_files(),
    ) {
        Ok(fingerprint) => fingerprint,
        Err(_) => {
            app.store.mark_workspace_agent_degraded(
                workspace_id,
                agent.as_str(),
                "agent_policy_unreadable",
            )?;
            return Err(agent_not_ready(
                workspace_id,
                agent,
                "agent policy is unreadable",
            ));
        }
    };
    let ready = record.agents.iter().any(|prepared| {
        prepared.agent_kind == agent.as_str()
            && prepared.status == "READY"
            && prepared.policy_fingerprint.as_deref() == Some(agent_fingerprint.as_str())
    });
    if record.status != "READY" || !ready {
        app.store.mark_workspace_agent_degraded(
            workspace_id,
            agent.as_str(),
            "agent_policy_changed",
        )?;
        return Err(agent_not_ready(
            workspace_id,
            agent,
            "run workspace prepare again",
        ));
    }
    Ok(record)
}

fn inspect_workspace(
    paths: &ReactorPaths,
    record: &WorkspaceRecord,
) -> std::result::Result<WorkspaceInspection, InspectionFailure> {
    let workspace = canonicalize(std::path::Path::new(&record.canonical_path))
        .map_err(|_| InspectionFailure::PathUnavailable)?;
    let policy = normalize_policy(&workspace, &record.policy)
        .map_err(|_| InspectionFailure::PolicyUnreadable)?;
    validate_storage_isolation(&workspace, &policy, paths)
        .map_err(|_| InspectionFailure::StorageOverlap)?;
    let fingerprint =
        fingerprint(&workspace, &policy, &[]).map_err(|_| InspectionFailure::PolicyUnreadable)?;
    Ok(WorkspaceInspection {
        workspace,
        policy,
        fingerprint,
    })
}

fn agent_not_ready(workspace_id: &str, agent: AgentKind, reason: &str) -> ReactorError {
    ReactorError::AgentNotReady {
        workspace_id: workspace_id.to_owned(),
        agent: agent.as_str().to_owned(),
        reason: reason.to_owned(),
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
            println!("status: {}", output::display_status(&workspace.status));
            for agent in &workspace.agents {
                println!(
                    "agent {}: {}",
                    agent.agent_kind,
                    output::display_status(&agent.status)
                );
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(instance_id, workspace))
        }
    }
}
