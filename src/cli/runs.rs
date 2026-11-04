use clap::{Args, Subcommand};
use serde::Serialize;

use crate::{
    app::App,
    error::{ReactorError, Result},
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
    /// Show the current state and latest attempt of a run.
    Progress(RunIdArgs),
    /// Show the final result summary of a run.
    Result(RunIdArgs),
    /// Show the retained workspace diff for a run.
    Diff(RunIdArgs),
    /// Show details needed to review an interrupted run.
    Review(RunIdArgs),
    /// List recent runs.
    List(RunListArgs),
    /// Cancel a queued or active run.
    Cancel(RunMutationArgs),
    /// Queue a new run from a retryable terminal run.
    Retry(RunMutationArgs),
    /// Acknowledge Task warnings and continue the same Plan occurrence.
    Continue(RunIdArgs),
    /// Delete a completed run and its logs.
    Clear(RunClearArgs),
}

#[derive(Debug, Args)]
struct RunListArgs {
    /// Maximum number of Runs to show. Defaults to 50.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=10_000))]
    limit: Option<u32>,

    /// Show every matching Run.
    #[arg(long, conflicts_with = "limit")]
    all: bool,

    /// Filter by Run state.
    #[arg(long, value_name = "STATE")]
    status: Option<String>,

    /// Filter by Workspace ID.
    #[arg(long, value_name = "ID")]
    workspace: Option<String>,
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

#[derive(Debug, Args)]
struct RunClearArgs {
    run_id: Option<String>,

    /// Include Runs that require review.
    #[arg(short = 'f', long = "all")]
    all: bool,

    /// Clear completed Runs created before this UTC date.
    #[arg(long, value_name = "YYYY-MM-DD")]
    before: Option<String>,
}

#[derive(Serialize)]
struct RunDeletion {
    deleted_runs: Vec<String>,
}

pub(super) fn execute(format: OutputFormat, arguments: &RunArgs) -> Result<()> {
    match &arguments.command {
        RunCommand::Next => next(format),
        RunCommand::Show(arguments) => show(format, arguments),
        RunCommand::Progress(arguments) => show(format, arguments),
        RunCommand::Result(arguments) => result(format, arguments),
        RunCommand::Diff(arguments) => diff(format, arguments),
        RunCommand::Review(arguments) => review(format, arguments),
        RunCommand::List(arguments) => list(format, arguments),
        RunCommand::Cancel(arguments) => cancel(format, arguments),
        RunCommand::Retry(arguments) => retry(format, arguments),
        RunCommand::Continue(arguments) => {
            let paths = ReactorPaths::discover()?;
            let mut app = App::open(&paths)?;
            let run = resolve_run(&app, &arguments.run_id)?;
            let run = app.store.continue_tasks(&run.id)?;
            match format {
                OutputFormat::Text => { println!("{} {}", run.id, run.state); Ok(()) },
                OutputFormat::Json | OutputFormat::Jsonl => output::write_json(&Envelope::new(app.instance_id, run)),
            }
        },
        RunCommand::Clear(arguments) => clear(format, arguments),
    }
}

fn result(format: OutputFormat, arguments: &RunIdArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let run = resolve_run(&app, &arguments.run_id)?;
    let result = run.result.ok_or_else(|| {
        ReactorError::InvalidArgument(format!("run {} has no final result", arguments.run_id))
    })?;
    match format {
        OutputFormat::Text => {
            println!("state: {}", output::display_status(&result.state));
            println!("completed: {}", output::display_time(&result.completed_at));
            if let Some(reason) = &result.termination_reason {
                println!("reason: {reason}");
            }
            println!("changed files: {}", result.changed_files.len());
            if let Some(log) = &result.log_path {
                println!("log: {log}");
            }
            println!("diff: {}", result.diff_path);
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, result))
        }
    }
}

fn diff(format: OutputFormat, arguments: &RunIdArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let run = resolve_run(&app, &arguments.run_id)?;
    let result = run.result.ok_or_else(|| {
        ReactorError::InvalidArgument(format!("run {} has no final diff", arguments.run_id))
    })?;
    let mut report = crate::checkpoint::read_diff(&paths, &result)?;
    if report.run_id != run.id {
        return Err(ReactorError::InvalidArgument(format!(
            "run {} diff is no longer retained",
            run.id
        )));
    }
    report.token_usage = run.attempt.as_ref().and_then(|attempt| attempt.token_usage);
    match format {
        OutputFormat::Text => {
            if let Some(tokens) = report.token_usage {
                println!("tokens: {tokens}");
            }
            if report.changes.is_empty() {
                println!("No file changes.");
                return Ok(());
            }
            for change in report.changes {
                println!(
                    "{} {}:{}",
                    change.summary.change,
                    output::escape_control(&change.summary.root),
                    output::escape_control(&change.summary.path)
                );
                if let Some(diff) = change.unified_diff {
                    print!("{diff}");
                } else {
                    println!(
                        "  {} -> {} bytes",
                        change.summary.before_size.unwrap_or_default(),
                        change.summary.after_size.unwrap_or_default()
                    );
                }
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, report))
        }
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
    let run = resolve_run(&app, &arguments.run_id)?;
    write_run(format, &app.instance_id, &run)
}

fn review(format: OutputFormat, arguments: &RunIdArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let run = resolve_run(&app, &arguments.run_id)?;
    if run.state != "REVIEW_REQUIRED" {
        return Err(ReactorError::InvalidArgument(format!(
            "run {} is not waiting for review",
            arguments.run_id
        )));
    }
    match format {
        OutputFormat::Text => {
            write_run_text(&run)?;
            if let Some(attempt) = &run.attempt
                && let Some(reason) = &attempt.termination_reason
            {
                println!("reason: {reason}");
            }
            if let Some(result) = &run.result {
                println!("changed files: {}", result.changed_files.len());
                println!("diff: {}", result.diff_path);
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, run))
        }
    }
}

fn list(format: OutputFormat, arguments: &RunListArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let status = arguments.status.as_deref().map(str::to_ascii_uppercase);
    if status
        .as_deref()
        .is_some_and(|value| crate::domain::RunState::parse(value).is_none())
    {
        return Err(ReactorError::InvalidArgument(format!(
            "unknown Run state: {}",
            arguments.status.as_deref().unwrap_or_default()
        )));
    }
    let runs = app.store.list_runs_filtered(
        status.as_deref(),
        arguments.workspace.as_deref(),
        (!arguments.all).then_some(arguments.limit.unwrap_or(50)),
    )?;
    match format {
        OutputFormat::Text => {
            let rows = runs
                .into_iter()
                .map(|run| {
                    let started = run.attempt.as_ref().map_or_else(
                        || "-".to_owned(),
                        |attempt| output::display_time(&attempt.started_at),
                    );
                    let completed = run.result.as_ref().map_or_else(
                        || {
                            run.attempt
                                .as_ref()
                                .and_then(|attempt| attempt.ended_at.as_deref())
                                .map(output::display_time)
                                .unwrap_or_else(|| "-".to_owned())
                        },
                        |result| output::display_time(&result.completed_at),
                    );
                    vec![
                        output::short_id(&run.id).to_owned(),
                        run.id,
                        output::display_status(&run.state),
                        run.workspace_id,
                        started,
                        completed,
                    ]
                })
                .collect::<Vec<_>>();
            output::write_table(
                &[
                    "CODE",
                    "RUN ID",
                    "STATE",
                    "WORKSPACE",
                    "STARTED",
                    "COMPLETED",
                ],
                &rows,
            );
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
    let run_id = resolve_run(&app, &arguments.run_id)?.id;
    let run = app
        .store
        .cancel_run(&run_id, arguments.idempotency_key.as_deref())?;
    write_run(format, &app.instance_id, &run)
}

fn retry(format: OutputFormat, arguments: &RunMutationArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let run_id = resolve_run(&app, &arguments.run_id)?.id;
    let run = app
        .store
        .retry_run(&run_id, arguments.idempotency_key.as_deref())?;
    write_run(format, &app.instance_id, &run)
}

fn clear(format: OutputFormat, arguments: &RunClearArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    if arguments.run_id.is_some() && (arguments.all || arguments.before.is_some()) {
        return Err(ReactorError::InvalidArgument(
            "a Run ID cannot be combined with --all or --before".to_owned(),
        ));
    }
    let cutoff = arguments
        .before
        .as_deref()
        .map(|date| format!("{date}T00:00:00Z").parse::<jiff::Timestamp>())
        .transpose()
        .map_err(|error| {
            ReactorError::InvalidArgument(format!("invalid --before date: {error}"))
        })?;
    let runs = if let Some(reference) = &arguments.run_id {
        vec![resolve_run(&app, reference)?]
    } else {
        let mut selected = Vec::new();
        for run in app.store.list_runs()? {
            if !is_clearable(&run, arguments.all) {
                continue;
            }
            if let Some(cutoff) = &cutoff {
                let created = run.created_at.parse::<jiff::Timestamp>().map_err(|error| {
                    ReactorError::InvalidStoredData(format!(
                        "invalid Run creation time for {}: {error}",
                        run.id
                    ))
                })?;
                if created >= cutoff.clone() {
                    continue;
                }
            }
            selected.push(run);
        }
        selected
    };
    let mut deleted_runs = Vec::with_capacity(runs.len());
    for run in runs {
        if !is_clearable(&run, true) {
            return Err(ReactorError::InvalidArgument(
                "only a completed Run can be cleared".to_owned(),
            ));
        }
        clear_run_files(&paths, &run.id)?;
        deleted_runs.push(app.store.clear_run(&run.id)?.id);
    }
    let deletion = RunDeletion { deleted_runs };
    match format {
        OutputFormat::Text => {
            println!("cleared runs: {}", deletion.deleted_runs.len());
            for run_id in deletion.deleted_runs {
                println!("{run_id}");
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, deletion))
        }
    }
}

fn is_clearable(run: &RunRecord, include_review: bool) -> bool {
    crate::domain::RunState::parse(&run.state).is_some_and(|state| state.can_clear(include_review))
}

fn clear_run_files(paths: &ReactorPaths, run_id: &str) -> Result<()> {
    let log_root = paths.data_dir.join("runs");
    crate::run_log::remove_run_directory(&log_root, run_id)
        .map_err(|source| crate::error::io_error(&log_root, source))?;

    let pending = paths
        .data_dir
        .join("manifests")
        .join("pending")
        .join(format!("{run_id}.json"));
    match std::fs::symlink_metadata(&pending) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            std::fs::remove_file(&pending)
                .map_err(|source| crate::error::io_error(&pending, source))?;
        }
        Ok(_) => {
            return Err(ReactorError::InvalidStoredData(
                "pending Run manifest is not a regular file".to_owned(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(crate::error::io_error(&pending, source)),
    }
    Ok(())
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
    println!("code: {}", output::short_id(&run.id));
    println!("state: {}", output::display_status(&run.state));
    println!("workspace: {}", run.workspace_id);
    println!("plan: {} v{}", run.plan_id, run.plan_version);
    println!("created: {}", output::display_time(&run.created_at));
    println!("ready: {}", output::display_time(&run.ready_at));
    if let Some(attempt) = &run.attempt {
        println!("attempt: {}", attempt.attempt_number);
        println!("started: {}", output::display_time(&attempt.started_at));
        if let Some(ended_at) = &attempt.ended_at {
            println!("ended: {}", output::display_time(ended_at));
        }
        println!("log: {}", attempt.log_path);
    }
    Ok(())
}

pub(super) fn resolve_run(app: &App, reference: &str) -> Result<RunRecord> {
    match app.store.get_run(reference) {
        Ok(run) => return Ok(run),
        Err(ReactorError::RunNotFound(_)) => {}
        Err(error) => return Err(error),
    }
    if reference.len() < 4 || !reference.is_ascii() || reference.chars().any(char::is_control) {
        return Err(ReactorError::RunNotFound(reference.to_owned()));
    }
    let mut matches = app.store.find_runs_by_suffix(reference)?.into_iter();
    let Some(run) = matches.next() else {
        return Err(ReactorError::RunNotFound(reference.to_owned()));
    };
    if matches.next().is_some() {
        return Err(ReactorError::InvalidArgument(format!(
            "run code {reference} is ambiguous; use more trailing characters"
        )));
    }
    Ok(run)
}
