use clap::{Args, Subcommand};

use crate::{
    app::App,
    error::Result,
    output,
    output::{Envelope, OutputFormat},
    paths::ReactorPaths,
    run::RunRecord,
};

#[derive(Debug, Args)]
pub(super) struct RunArgs {
    #[command(subcommand)]
    command: RunCommand,
}

#[derive(Debug, Subcommand)]
enum RunCommand {
    /// Execute the next eligible queued run.
    Next,
    /// Show a run and its latest attempt.
    Show(RunIdArgs),
    /// List runs.
    List,
    /// Cancel a queued or active run.
    Cancel(RunMutationArgs),
    /// Queue a new run from a retryable terminal run.
    Retry(RunMutationArgs),
}

#[derive(Debug, Args)]
struct RunIdArgs {
    run_id: String,
}

#[derive(Debug, Args)]
struct RunMutationArgs {
    run_id: String,

    #[arg(long)]
    idempotency_key: Option<String>,
}

pub(super) fn execute(format: OutputFormat, arguments: &RunArgs) -> Result<()> {
    match &arguments.command {
        RunCommand::Next => next(format),
        RunCommand::Show(arguments) => show(format, arguments),
        RunCommand::List => list(format),
        RunCommand::Cancel(arguments) => cancel(format, arguments),
        RunCommand::Retry(arguments) => retry(format, arguments),
    }
}

fn next(format: OutputFormat) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let run = crate::runner::execute_next(&paths, &mut app)?;
    match (format, run) {
        (OutputFormat::Text, Some(run)) => write_run_text(&run),
        (OutputFormat::Text, None) => {
            println!("No eligible run.");
            Ok(())
        }
        (OutputFormat::Json | OutputFormat::Jsonl, run) => {
            output::write_json(&Envelope::new(app.instance_id, run))
        }
    }
}

fn show(format: OutputFormat, arguments: &RunIdArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let run = app.store.get_run(&arguments.run_id)?;
    write_run(format, &app.instance_id, &run)
}

fn list(format: OutputFormat) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let runs = app.store.list_runs()?;
    match format {
        OutputFormat::Text => {
            for run in runs {
                println!("{} {} {}", run.id, run.state, run.workspace_id);
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, runs))
        }
    }
}

fn cancel(format: OutputFormat, arguments: &RunMutationArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let run = app
        .store
        .cancel_run(&arguments.run_id, arguments.idempotency_key.as_deref())?;
    write_run(format, &app.instance_id, &run)
}

fn retry(format: OutputFormat, arguments: &RunMutationArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let run = app
        .store
        .retry_run(&arguments.run_id, arguments.idempotency_key.as_deref())?;
    write_run(format, &app.instance_id, &run)
}

fn write_run(format: OutputFormat, instance_id: &str, run: &RunRecord) -> Result<()> {
    match format {
        OutputFormat::Text => write_run_text(run),
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(instance_id, run))
        }
    }
}

fn write_run_text(run: &RunRecord) -> Result<()> {
    println!("run: {}", run.id);
    println!("state: {}", run.state);
    println!("workspace: {}", run.workspace_id);
    println!("plan: {} v{}", run.plan_id, run.plan_version);
    if let Some(attempt) = &run.attempt {
        println!("attempt: {}", attempt.attempt_number);
        println!("log: {}", attempt.log_path);
    }
    Ok(())
}
