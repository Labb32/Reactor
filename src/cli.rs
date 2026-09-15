use std::{process::ExitCode, thread, time::Duration};

use clap::{Args, Parser, Subcommand};
use serde::Serialize;

use crate::{
    agents::AgentKind,
    app::App,
    config::{ConcurrencyConfig, Config},
    error::{ReactorError, Result},
    input, output,
    output::{Envelope, OutputFormat},
    paths::ReactorPaths,
    store::AuditEvent,
    workspace::{WorkspacePolicy, canonicalize, fingerprint, validate_id},
};

#[derive(Debug, Parser)]
#[command(name = "reactor", version, about = "Schedule reviewed AI agent work")]
struct Cli {
    #[arg(long, global = true, value_enum, default_value_t)]
    output: OutputFormat,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize Reactor's configuration and local database.
    Init(InputArgs),
    /// Show version, instance, and capability information.
    Info,
    /// Show the current workspace, queue, and run state.
    Snapshot,
    /// Read audit events after a cursor.
    Events(EventsArgs),
    /// Register and prepare agent workspaces.
    Workspace(WorkspaceArgs),
}

#[derive(Debug, Args)]
struct WorkspaceArgs {
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

#[derive(Debug, Args)]
struct InputArgs {
    /// Read JSON or YAML from a file, or use - for stdin.
    #[arg(long, value_name = "PATH")]
    file: Option<String>,
}

#[derive(Debug, Args)]
struct EventsArgs {
    #[arg(long, default_value_t = 0, allow_hyphen_values = false)]
    after: i64,

    #[arg(long, default_value_t = 1_000, value_parser = clap::value_parser!(u32).range(1..=10_000))]
    limit: u32,

    #[arg(long)]
    follow: bool,
}

#[derive(Debug, Serialize)]
struct InitializationData {
    config_file: String,
    database_file: String,
    database_schema_version: u32,
}

#[derive(Debug, Serialize)]
struct InfoData {
    version: &'static str,
    database_schema_version: u32,
    capabilities: &'static [&'static str],
    concurrency: ConcurrencyConfig,
}

const CAPABILITIES: &[&str] = &[
    "versioned_json",
    "snapshot",
    "cursor_events",
    "workspace_registry",
    "codex_adapter",
];

fn execute(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Init(arguments) => initialize(cli.output, arguments),
        Command::Info => info(cli.output),
        Command::Snapshot => snapshot(cli.output),
        Command::Events(arguments) => events(cli.output, arguments),
        Command::Workspace(arguments) => workspace(cli.output, arguments),
    }
}

fn workspace(format: OutputFormat, arguments: &WorkspaceArgs) -> Result<()> {
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
    let workspace = canonicalize(std::path::Path::new(&record.canonical_path))?;
    let policy_fingerprint = fingerprint(&workspace, &record.policy)?;
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

fn initialize(format: OutputFormat, arguments: &InputArgs) -> Result<()> {
    let config = arguments
        .file
        .as_deref()
        .map(input::read_document::<Config>)
        .transpose()?;
    let initialized = App::initialize(ReactorPaths::discover()?, config)?;
    let data = InitializationData {
        config_file: initialized.config_file.display().to_string(),
        database_file: initialized.database_file.display().to_string(),
        database_schema_version: initialized.schema_version,
    };

    match format {
        OutputFormat::Text => {
            println!("Reactor initialized.");
            println!("config: {}", data.config_file);
            println!("database: {}", data.database_file);
            println!("schema: {}", data.database_schema_version);
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(initialized.instance_id, data))
        }
    }
}

fn info(format: OutputFormat) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let data = InfoData {
        version: env!("CARGO_PKG_VERSION"),
        database_schema_version: app.store.schema_version()?,
        capabilities: CAPABILITIES,
        concurrency: app.config.concurrency.clone(),
    };

    match format {
        OutputFormat::Text => {
            println!("Reactor {}", data.version);
            println!("instance: {}", app.instance_id);
            println!("database schema: {}", data.database_schema_version);
            println!("capabilities: {}", data.capabilities.join(", "));
            println!("global concurrency: {}", data.concurrency.global);
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, data))
        }
    }
}

fn snapshot(format: OutputFormat) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let snapshot = app.store.snapshot()?;

    match format {
        OutputFormat::Text => {
            println!("workspaces: {}", snapshot.workspaces.len());
            println!("active runs: {}", snapshot.runs.len());
            println!("queued runs: {}", snapshot.queued_runs);
            println!("event cursor: {}", snapshot.latest_event_cursor);
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, snapshot))
        }
    }
}

fn events(format: OutputFormat, arguments: &EventsArgs) -> Result<()> {
    if arguments.after < 0 {
        return Err(ReactorError::InvalidArgument(
            "--after must be zero or greater".to_owned(),
        ));
    }
    if arguments.follow && !matches!(format, OutputFormat::Jsonl) {
        return Err(ReactorError::InvalidArgument(
            "--follow requires --output jsonl".to_owned(),
        ));
    }

    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let mut cursor = arguments.after;

    loop {
        let events = app.store.events_after(cursor, arguments.limit)?;
        if matches!(format, OutputFormat::Json) {
            return output::write_json(&Envelope::new(app.instance_id.clone(), events));
        }

        let caught_up = events.is_empty();
        for event in events {
            cursor = event.cursor;
            write_event(format, &app.instance_id, event)?;
        }

        if !arguments.follow {
            return Ok(());
        }
        if caught_up {
            thread::sleep(Duration::from_millis(500));
        }
    }
}

fn write_event(format: OutputFormat, instance_id: &str, event: AuditEvent) -> Result<()> {
    match format {
        OutputFormat::Text => {
            println!(
                "{} {} {} {}",
                event.cursor, event.occurred_at, event.event_type, event.entity_id
            );
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(instance_id, event))
        }
    }
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
    match execute(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if output::write_error(cli.output, None, &error).is_err() {
                eprintln!("reactor: {error}");
            }
            ExitCode::from(error.exit_code())
        }
    }
}
