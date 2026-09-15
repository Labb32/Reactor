mod app;
mod config;
#[allow(dead_code)]
mod domain;
mod error;
mod ids;
mod input;
mod output;
mod paths;
mod store;
mod time;

use std::{process::ExitCode, thread, time::Duration};

use clap::{Args, Parser, Subcommand};
use serde::Serialize;

use crate::{
    app::App,
    config::Config,
    error::{ReactorError, Result},
    output::{Envelope, OutputFormat},
    paths::ReactorPaths,
    store::{AuditEvent, SUPPORTED_SCHEMA_VERSION},
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
}

const CAPABILITIES: &[&str] = &["versioned_json", "snapshot", "cursor_events"];

fn execute(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Init(arguments) => initialize(cli.output, arguments),
        Command::Info => info(cli.output),
        Command::Snapshot => snapshot(cli.output),
        Command::Events(arguments) => events(cli.output, arguments),
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
        database_schema_version: SUPPORTED_SCHEMA_VERSION,
        capabilities: CAPABILITIES,
    };

    match format {
        OutputFormat::Text => {
            println!("Reactor {}", data.version);
            println!("instance: {}", app.instance_id);
            println!("database schema: {}", data.database_schema_version);
            println!("capabilities: {}", data.capabilities.join(", "));
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

        for event in events {
            cursor = event.cursor;
            write_event(format, &app.instance_id, event)?;
        }

        if !arguments.follow {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(500));
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

fn main() -> ExitCode {
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
