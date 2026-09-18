use clap::{Args, Subcommand};

use crate::{
    app::App,
    error::{ReactorError, Result},
    input, output,
    output::{Envelope, OutputFormat},
    paths::ReactorPaths,
    schedule::{DEFAULT_DEADLINE_SECONDS, ScheduleInput, ScheduleRecord, ScheduleRule, rule_name},
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
    /// Stop a schedule and its pending or active run.
    Stop(ScheduleMutationArgs),
    /// Delete a stopped schedule.
    Delete(ScheduleMutationArgs),
    /// Trigger a manual schedule.
    Trigger(ScheduleMutationArgs),
}

#[derive(Debug, Args)]
struct ScheduleAddArgs {
    plan_id: Option<String>,

    /// Use a custom schedule ID.
    #[arg(long, value_name = "ID")]
    name: Option<String>,

    /// Read a schedule document from JSON or YAML.
    #[arg(long, value_name = "PATH")]
    file: Option<String>,

    #[arg(long)]
    immediate: bool,

    #[arg(long)]
    manual: bool,

    #[arg(long, value_name = "LOCAL_TIME")]
    at: Option<String>,

    #[arg(long)]
    version: Option<u32>,

    #[arg(long)]
    start_deadline_seconds: Option<u64>,

    #[arg(long)]
    local_endpoint: Option<String>,

    #[arg(long)]
    after_quota_reset: bool,

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
        ScheduleCommand::Stop(arguments) => stop(format, arguments),
        ScheduleCommand::Delete(arguments) => delete(format, arguments),
        ScheduleCommand::Trigger(arguments) => trigger(format, arguments),
    }
}

fn add(format: OutputFormat, arguments: &ScheduleAddArgs) -> Result<()> {
    let input = schedule_input(arguments)?;
    let prepared = crate::schedule::prepare(&input)?;
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let schedule = app.store.create_schedule(
        arguments.name.as_deref(),
        &input,
        &prepared,
        arguments.idempotency_key.as_deref(),
    )?;
    write_schedule(format, &app.instance_id, &schedule)
}

fn schedule_input(arguments: &ScheduleAddArgs) -> Result<ScheduleInput> {
    if let Some(file) = &arguments.file {
        if arguments.plan_id.is_some()
            || arguments.immediate
            || arguments.manual
            || arguments.at.is_some()
            || arguments.version.is_some()
            || arguments.start_deadline_seconds.is_some()
            || arguments.local_endpoint.is_some()
            || arguments.after_quota_reset
        {
            return Err(ReactorError::InvalidArgument(
                "--file cannot be combined with simple Schedule arguments".to_owned(),
            ));
        }
        return input::read_document::<ScheduleInput>(file);
    }
    let plan_id = arguments.plan_id.clone().ok_or_else(|| {
        ReactorError::InvalidArgument("plan ID is required without --file".to_owned())
    })?;
    let rule_count = usize::from(arguments.immediate)
        + usize::from(arguments.manual)
        + usize::from(arguments.at.is_some());
    if rule_count != 1 {
        return Err(ReactorError::InvalidArgument(
            "choose exactly one of --immediate, --manual, or --at".to_owned(),
        ));
    }
    let rule = if arguments.immediate {
        ScheduleRule::Immediate
    } else if arguments.manual {
        ScheduleRule::Manual
    } else {
        ScheduleRule::At {
            local_time: arguments.at.clone().unwrap_or_default(),
        }
    };
    Ok(ScheduleInput {
        plan_id,
        plan_version: arguments.version,
        rule,
        start_deadline_seconds: arguments
            .start_deadline_seconds
            .unwrap_or(DEFAULT_DEADLINE_SECONDS),
        local_endpoint: arguments.local_endpoint.clone(),
        after_quota_reset: arguments.after_quota_reset,
    })
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
            let rows = schedules
                .into_iter()
                .map(|schedule| {
                    let occurrence = schedule.occurrence.as_ref();
                    vec![
                        schedule.id,
                        output::display_status(&schedule.status),
                        schedule.plan_id,
                        format!("v{}", schedule.plan_version),
                        rule_name(&schedule.rule).to_owned(),
                        format!("{}s", schedule.start_deadline_seconds),
                        occurrence.map_or_else(
                            || "-".to_owned(),
                            |value| output::display_status(&value.state),
                        ),
                        occurrence.map_or_else(
                            || "-".to_owned(),
                            |value| output::display_time(&value.scheduled_at),
                        ),
                    ]
                })
                .collect::<Vec<_>>();
            output::write_table(
                &[
                    "SCHEDULE ID",
                    "STATUS",
                    "PLAN",
                    "VERSION",
                    "RULE",
                    "DEADLINE",
                    "RUN STATUS",
                    "SCHEDULED",
                ],
                &rows,
            );
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, schedules))
        }
    }
}

fn stop(format: OutputFormat, arguments: &ScheduleMutationArgs) -> Result<()> {
    let mut app = open_refreshed()?;
    let schedule = app
        .store
        .stop_schedule(&arguments.schedule_id, arguments.idempotency_key.as_deref())?;
    write_schedule(format, &app.instance_id, &schedule)
}

fn delete(format: OutputFormat, arguments: &ScheduleMutationArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let schedule = app
        .store
        .delete_schedule(&arguments.schedule_id, arguments.idempotency_key.as_deref())?;
    match format {
        OutputFormat::Text => {
            println!("deleted schedule: {}", schedule.id);
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, schedule))
        }
    }
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
            println!(
                "schedule status: {}",
                output::display_status(&schedule.status)
            );
            println!("plan: {} v{}", schedule.plan_id, schedule.plan_version);
            println!("rule: {}", rule_name(&schedule.rule));
            if matches!(&schedule.rule, ScheduleRule::Manual) {
                println!(
                    "start deadline: {}s after trigger",
                    schedule.start_deadline_seconds
                );
            } else {
                println!("start deadline: {}s", schedule.start_deadline_seconds);
            }
            if let Some(occurrence) = &schedule.occurrence {
                println!("run status: {}", output::display_status(&occurrence.state));
                println!(
                    "scheduled: {}",
                    output::display_time(&occurrence.scheduled_at)
                );
                if let Some(run_id) = &occurrence.run_id {
                    println!("run: {run_id} ({})", output::short_id(run_id));
                }
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(instance_id, schedule))
        }
    }
}
