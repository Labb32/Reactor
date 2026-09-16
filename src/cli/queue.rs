use clap::{Args, Subcommand};

use crate::{
    app::App,
    error::Result,
    output,
    output::{Envelope, OutputFormat},
    paths::ReactorPaths,
};

#[derive(Debug, Args)]
pub(super) struct QueueArgs {
    #[command(subcommand)]
    command: QueueCommand,
}

#[derive(Debug, Subcommand)]
enum QueueCommand {
    /// List queued runs and current dispatch eligibility.
    List,
}

pub(super) fn execute(format: OutputFormat, arguments: &QueueArgs) -> Result<()> {
    match arguments.command {
        QueueCommand::List => list(format),
    }
}

fn list(format: OutputFormat) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    app.store.refresh_due_schedules()?;
    let queue = app.store.queue(&app.config.concurrency)?;
    match format {
        OutputFormat::Text => {
            for run in queue {
                let availability = if run.eligible {
                    "eligible"
                } else {
                    run.blocked_by.as_deref().unwrap_or("waiting")
                };
                println!(
                    "{} {} {} {}",
                    run.id, run.workspace_id, run.ready_at, availability
                );
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, queue))
        }
    }
}
