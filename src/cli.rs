use std::{process::ExitCode, thread, time::Duration};

use clap::{Args, Parser, Subcommand};
use serde::Serialize;

mod daemon;
mod plans;
mod queue;
mod runs;
mod schedules;
mod services;
mod workspaces;

use crate::{
    app::App,
    config::{ConcurrencyConfig, Config},
    error::{ReactorError, Result},
    input, output,
    output::{Envelope, OutputFormat},
    paths::ReactorPaths,
    store::AuditEvent,
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
    Init(InitArgs),
    /// Run the background scheduler.
    Daemon(daemon::DaemonArgs),
    /// Manage user login startup.
    Service(services::ServiceArgs),
    /// Show version, instance, and capability information.
    Info,
    /// Show the current workspace, queue, and run state.
    Snapshot,
    /// Read audit events after a cursor.
    Events(EventsArgs),
    /// Register and prepare agent workspaces.
    Workspace(workspaces::WorkspaceArgs),
    /// Create, review, and approve immutable plan versions.
    Plan(plans::PlanArgs),
    /// Schedule approved plans.
    Schedule(schedules::ScheduleArgs),
    /// Inspect queued runs.
    Queue(queue::QueueArgs),
    /// Execute, inspect, and cancel runs.
    Run(runs::RunArgs),
}

#[derive(Debug, Args)]
struct InitArgs {
    /// Read JSON or YAML from a file, or use - for stdin.
    #[arg(long, value_name = "PATH")]
    file: Option<String>,

    /// Initialize data without registering automatic startup.
    #[arg(long)]
    no_autostart: bool,
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
    autostart: bool,
    service: Option<crate::service::ServiceStatus>,
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
    "qwen_adapter",
    "claude_adapter",
    "plan_versions",
    "named_plans",
    "single_schedules",
    "fair_queue",
    "run_execution",
    "run_cancellation",
    "run_results",
    "run_review",
    "workspace_diff",
    "interrupted_run_recovery",
    "recurring_schedules",
    "daemon",
    "user_autostart",
];

fn execute(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Init(arguments) => initialize(cli.output, arguments),
        Command::Daemon(arguments) => daemon::execute(arguments),
        Command::Service(arguments) => services::execute(cli.output, arguments),
        Command::Info => info(cli.output),
        Command::Snapshot => snapshot(cli.output),
        Command::Events(arguments) => events(cli.output, arguments),
        Command::Workspace(arguments) => workspaces::execute(cli.output, arguments),
        Command::Plan(arguments) => plans::execute(cli.output, arguments),
        Command::Schedule(arguments) => schedules::execute(cli.output, arguments),
        Command::Queue(arguments) => queue::execute(cli.output, arguments),
        Command::Run(arguments) => runs::execute(cli.output, arguments),
    }
}

fn initialize(format: OutputFormat, arguments: &InitArgs) -> Result<()> {
    let config = arguments
        .file
        .as_deref()
        .map(input::read_document::<Config>)
        .transpose()?;
    let paths = ReactorPaths::discover()?;
    let initialized = App::initialize(paths.clone(), config)?;
    let service = if arguments.no_autostart {
        None
    } else {
        let started = crate::service::install(&paths, &initialized.instance_id)
            .and_then(|_| crate::service::start(&paths, &initialized.instance_id))
            .map_err(|source| ReactorError::InitializationService {
                config_file: initialized.config_file.clone(),
                database_file: initialized.database_file.clone(),
                source: Box::new(source),
            })?;
        Some(started)
    };
    let data = InitializationData {
        config_file: initialized.config_file.display().to_string(),
        database_file: initialized.database_file.display().to_string(),
        database_schema_version: initialized.schema_version,
        autostart: !arguments.no_autostart,
        service,
    };

    match format {
        OutputFormat::Text => {
            println!("Reactor initialized.");
            println!("config: {}", data.config_file);
            println!("database: {}", data.database_file);
            println!("schema: {}", data.database_schema_version);
            println!("autostart: {}", data.autostart);
            if let Some(service) = &data.service {
                println!("service: {}", service.platform);
                println!("daemon running: {}", service.running);
            }
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
    let mut app = App::open(&paths)?;
    app.store.refresh_due_schedules()?;
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
                event.cursor,
                output::display_time(&event.occurred_at),
                event.event_type,
                event.entity_id
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
                eprintln!("reactor: {}", output::escape_control(&error.to_string()));
            }
            ExitCode::from(error.exit_code())
        }
    }
}
