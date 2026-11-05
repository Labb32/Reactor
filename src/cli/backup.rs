use std::path::PathBuf;
use clap::{Args, Subcommand};
use crate::{error::Result, paths::ReactorPaths, output::{self, Envelope, OutputFormat}};

#[derive(Debug, Args)]
pub(super) struct BackupArgs { #[command(subcommand)] command: BackupCommand }
#[derive(Debug, Subcommand)]
enum BackupCommand {
    /// Back up Reactor data to a new directory while offline and in maintenance.
    Create { directory: PathBuf },
    /// Restore a checked backup; leave maintenance enabled and require preparation.
    Restore { directory: PathBuf, #[arg(long)] replace: bool },
}

pub(super) fn execute(format: OutputFormat, arguments: &BackupArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let result = match &arguments.command {
        BackupCommand::Create { directory } => serde_json::json!({"backup": directory, "manifest": crate::backup::create(&paths, directory)?}),
        BackupCommand::Restore { directory, replace } => crate::backup::restore(&paths, directory, *replace)?,
    };
    match format {
        OutputFormat::Text => { println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default()); Ok(()) },
        _ => output::write_json(&Envelope::new("local".to_owned(), result)),
    }
}
