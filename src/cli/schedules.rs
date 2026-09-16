use clap::{Args, Subcommand};

use crate::{
    app::App,
    error::Result,
    input, output,
    output::{Envelope, OutputFormat},
    paths::ReactorPaths,
    schedule::{ScheduleInput, ScheduleRecord},
};

#[derive(Debug, Args)]
pub(super) struct ScheduleArgs {
    #[command(subcommand)]
    command: ScheduleCommand,
}

#[derive(Debug, Subcommand)]
enum ScheduleCommand {
    /// Schedule an approved plan version.
    Add(ScheduleAddArgs),
    /// Show a schedule and its occurrence.
    Show(ScheduleIdArgs),
    /// List schedules.
    List,
    /// Pause a pending schedule.
    Pause(ScheduleMutationArgs),
    /// Resume a paused schedule.
    Resume(ScheduleMutationArgs),
    /// Trigger a manual schedule.
    Trigger(ScheduleMutationArgs),
}

#[derive(Debug, Args)]
struct ScheduleAddArgs {
    /// Read a schedule document from JSON or YAML.
    #[arg(long, value_name = "PATH")]
    file: String,

    #[arg(long)]
    idempotency_key: Option<String>,
}

#[derive(Debug, Args)]
struct ScheduleIdArgs {
    schedule_id: String,
}

#[derive(Debug, Args)]
struct ScheduleMutationArgs {
    schedule_id: String,

    #[arg(long)]
    idempotency_key: Option<String>,
}

pub(super) fn execute(format: OutputFormat, arguments: &ScheduleArgs) -> Result<()> {
    match &arguments.command {
        ScheduleCommand::Add(arguments) => add(format, arguments),
        ScheduleCommand::Show(arguments) => show(format, arguments),
        ScheduleCommand::List => list(format),
        ScheduleCommand::Pause(arguments) => pause(format, arguments),
        ScheduleCommand::Resume(arguments) => resume(format, arguments),
        ScheduleCommand::Trigger(arguments) => trigger(format, arguments),
    }
}

fn add(format: OutputFormat, arguments: &ScheduleAddArgs) -> Result<()> {
    let input = input::read_document::<ScheduleInput>(&arguments.file)?;
    let prepared = crate::schedule::prepare(&input)?;
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let schedule =
        app.store
            .create_schedule(&input, &prepared, arguments.idempotency_key.as_deref())?;
    write_schedule(format, &app.instance_id, &schedule)
}

fn show(format: OutputFormat, arguments: &ScheduleIdArgs) -> Result<()> {
    let app = open_refreshed()?;
    let schedule = app.store.get_schedule(&arguments.schedule_id)?;
    write_schedule(format, &app.instance_id, &schedule)
}

fn list(format: OutputFormat) -> Result<()> {
    let app = open_refreshed()?;
    let schedules = app.store.list_schedules()?;
    match format {
        OutputFormat::Text => {
            for schedule in schedules {
                println!(
                    "{} {} {} v{}",
                    schedule.id, schedule.status, schedule.plan_id, schedule.plan_version
                );
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, schedules))
        }
    }
}

fn pause(format: OutputFormat, arguments: &ScheduleMutationArgs) -> Result<()> {
    let mut app = open_refreshed()?;
    let schedule = app
        .store
        .pause_schedule(&arguments.schedule_id, arguments.idempotency_key.as_deref())?;
    write_schedule(format, &app.instance_id, &schedule)
}

fn resume(format: OutputFormat, arguments: &ScheduleMutationArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    app.store
        .resume_schedule(&arguments.schedule_id, arguments.idempotency_key.as_deref())?;
    app.store.refresh_due_schedules()?;
    let schedule = app.store.get_schedule(&arguments.schedule_id)?;
    write_schedule(format, &app.instance_id, &schedule)
}

fn trigger(format: OutputFormat, arguments: &ScheduleMutationArgs) -> Result<()> {
    let mut app = open_refreshed()?;
    let schedule = app
        .store
        .trigger_schedule(&arguments.schedule_id, arguments.idempotency_key.as_deref())?;
    write_schedule(format, &app.instance_id, &schedule)
}

fn open_refreshed() -> Result<App> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    app.store.refresh_due_schedules()?;
    Ok(app)
}

fn write_schedule(
    format: OutputFormat,
    instance_id: &str,
    schedule: &ScheduleRecord,
) -> Result<()> {
    match format {
        OutputFormat::Text => {
            println!("schedule: {}", schedule.id);
            println!("status: {}", schedule.status);
            println!("plan: {} v{}", schedule.plan_id, schedule.plan_version);
            if let Some(occurrence) = &schedule.occurrence {
                println!("occurrence: {}", occurrence.state);
                println!("scheduled at: {}", occurrence.scheduled_at);
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(instance_id, schedule))
        }
    }
}
