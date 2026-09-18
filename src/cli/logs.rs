use std::{fs, path::PathBuf};

use clap::{Args, Subcommand};
use serde::Serialize;

use crate::{
    app::App,
    error::{ReactorError, Result, io_error},
    output,
    output::{Envelope, OutputFormat},
    paths::ReactorPaths,
    run::RunRecord,
};

#[derive(Debug, Args)]
pub(super) struct LogArgs {
    #[command(subcommand)]
    command: LogCommand,
}

#[derive(Debug, Subcommand)]
enum LogCommand {
    /// List the latest log recorded for each Run.
    List,
    /// Print a Run's current log, optionally including rotated files.
    Show(LogShowArgs),
    /// Delete log files for a terminal Run.
    Delete(LogRunArgs),
    /// Delete log files older than the retention period.
    Prune,
}

#[derive(Debug, Args)]
struct LogRunArgs {
    run: String,
}

#[derive(Debug, Args)]
struct LogShowArgs {
    run: String,

    #[arg(long)]
    all: bool,
}

#[derive(Serialize)]
struct LogEntry {
    run_id: String,
    code: String,
    state: String,
    token_usage: Option<u64>,
    path: String,
}

#[derive(Serialize)]
struct LogContent {
    run_id: String,
    files: Vec<LogFile>,
}

#[derive(Serialize)]
struct LogFile {
    path: String,
    content: String,
}

#[derive(Serialize)]
struct LogDeletion {
    run_id: String,
    deleted_files: usize,
}

pub(super) fn execute(format: OutputFormat, arguments: &LogArgs) -> Result<()> {
    match &arguments.command {
        LogCommand::List => list(format),
        LogCommand::Show(arguments) => show(format, arguments),
        LogCommand::Delete(arguments) => delete(format, arguments),
        LogCommand::Prune => prune(format),
    }
}

fn list(format: OutputFormat) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let entries = app
        .store
        .list_runs()?
        .into_iter()
        .filter_map(|run| {
            let attempt = run.attempt?;
            let file_type = fs::symlink_metadata(&attempt.log_path).ok()?.file_type();
            if !file_type.is_file() || file_type.is_symlink() {
                return None;
            }
            Some(LogEntry {
                code: output::short_id(&run.id).to_owned(),
                run_id: run.id,
                state: run.state,
                token_usage: attempt.token_usage,
                path: attempt.log_path,
            })
        })
        .collect::<Vec<_>>();
    match format {
        OutputFormat::Text => {
            let rows = entries
                .into_iter()
                .map(|entry| {
                    vec![
                        entry.code,
                        entry.run_id,
                        output::display_status(&entry.state),
                        entry
                            .token_usage
                            .map_or_else(|| "-".to_owned(), |value| value.to_string()),
                        entry.path,
                    ]
                })
                .collect::<Vec<_>>();
            output::write_table(&["CODE", "RUN ID", "STATE", "TOKENS", "LOG"], &rows);
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, entries))
        }
    }
}

fn show(format: OutputFormat, arguments: &LogShowArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let run = super::runs::resolve_run(&app, &arguments.run)?;
    let files = selected_log_files(&paths, &run, arguments.all)?;
    let mut contents = Vec::with_capacity(files.len());
    for path in files {
        let bytes = fs::read(&path).map_err(|source| io_error(&path, source))?;
        contents.push(LogFile {
            path: path.to_string_lossy().into_owned(),
            content: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    match format {
        OutputFormat::Text => {
            for file in contents {
                if arguments.all {
                    println!("==> {} <==", file.path);
                }
                print!("{}", file.content);
                if !file.content.ends_with('\n') {
                    println!();
                }
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => output::write_json(&Envelope::new(
            app.instance_id,
            LogContent {
                run_id: run.id,
                files: contents,
            },
        )),
    }
}

fn delete(format: OutputFormat, arguments: &LogRunArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let run = super::runs::resolve_run(&app, &arguments.run)?;
    if matches!(run.state.as_str(), "PREPARING" | "RUNNING") {
        return Err(ReactorError::InvalidArgument(
            "logs for an active Run cannot be deleted".to_owned(),
        ));
    }
    let files = selected_log_files(&paths, &run, true)?;
    for path in &files {
        fs::remove_file(path).map_err(|source| io_error(path, source))?;
    }
    app.store.record_log_deletion(&run.id, files.len())?;
    let deletion = LogDeletion {
        run_id: run.id,
        deleted_files: files.len(),
    };
    match format {
        OutputFormat::Text => {
            println!("deleted log files: {}", deletion.deleted_files);
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, deletion))
        }
    }
}

fn prune(format: OutputFormat) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let root = paths.data_dir.join("runs");
    crate::run_log::prune(&root).map_err(|source| io_error(&root, source))?;
    match format {
        OutputFormat::Text => {
            println!("expired logs pruned");
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => output::write_json(&Envelope::new(
            app.instance_id,
            serde_json::json!({ "pruned": true }),
        )),
    }
}

fn selected_log_files(
    paths: &ReactorPaths,
    run: &RunRecord,
    include_rotated: bool,
) -> Result<Vec<PathBuf>> {
    let attempt = run.attempt.as_ref().ok_or_else(|| {
        ReactorError::InvalidArgument(format!("run {} has no attempt log", run.id))
    })?;
    let directory = paths.data_dir.join("runs").join(&run.id);
    let current = PathBuf::from(&attempt.log_path);
    if current.parent() != Some(directory.as_path()) {
        return Err(ReactorError::InvalidStoredData(
            "run log path is outside the Run log directory".to_owned(),
        ));
    }
    if !include_rotated {
        let file_type = fs::symlink_metadata(&current)
            .map_err(|source| io_error(&current, source))?
            .file_type();
        if !file_type.is_file() || file_type.is_symlink() {
            return Err(ReactorError::InvalidStoredData(
                "run log is not a regular file".to_owned(),
            ));
        }
        return Ok(vec![current]);
    }
    let current_name = current.file_name().ok_or_else(|| {
        ReactorError::InvalidStoredData("run log path has no file name".to_owned())
    })?;
    let rotated_prefix = format!("{}.", current_name.to_string_lossy());
    let mut files = Vec::new();
    for entry in fs::read_dir(&directory).map_err(|source| io_error(&directory, source))? {
        let entry = entry.map_err(|source| io_error(&directory, source))?;
        let file_type = entry
            .file_type()
            .map_err(|source| io_error(entry.path(), source))?;
        let name = entry.file_name();
        let rotated = name
            .to_string_lossy()
            .strip_prefix(&rotated_prefix)
            .is_some_and(|suffix| suffix.parse::<u8>().is_ok());
        if file_type.is_file()
            && !file_type.is_symlink()
            && (name.as_os_str() == current_name || rotated)
        {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}
