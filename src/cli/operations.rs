use clap::{Args, Subcommand};
use std::time::Duration;
use crate::{error::Result, paths::ReactorPaths, output::{self, Envelope, OutputFormat}};

#[derive(Debug, Args)]
pub(super) struct MaintenanceArgs { #[command(subcommand)] command: MaintenanceCommand }
#[derive(Debug, Subcommand)]
enum MaintenanceCommand {
    /// Block new work and drain active execution; timeout requests cancellation.
    Enter { #[arg(long, default_value_t = 1800)] timeout_seconds: u64 },
    /// Explicitly allow new work after maintenance.
    Leave,
    Status,
}

pub(super) fn maintenance(format: OutputFormat, args: &MaintenanceArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    match &args.command {
        MaintenanceCommand::Enter { timeout_seconds } => {
            let _ = crate::interrupt::install("maintenance interrupt")?;
            crate::operations::drain(&paths, Duration::from_secs(*timeout_seconds))?;
        },
        MaintenanceCommand::Leave => crate::operations::set_maintenance(&paths, false)?,
        MaintenanceCommand::Status => {},
    }
    status(format)
}

pub(super) fn status(format: OutputFormat) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let status = crate::operations::status(&paths)?;
    match format {
        OutputFormat::Text => { println!("{}", serde_json::to_string_pretty(&status).unwrap_or_default()); Ok(()) },
        _ => output::write_json(&Envelope::new("local".to_owned(), status)),
    }
}
