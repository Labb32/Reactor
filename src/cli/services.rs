use clap::{Args, Subcommand};

use crate::{
    app::App,
    error::Result,
    output::{self, Envelope, OutputFormat},
    paths::ReactorPaths,
    service::ServiceStatus,
};

#[derive(Debug, Args)]
pub(super) struct ServiceArgs {
    #[command(subcommand)]
    command: ServiceCommand,
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// Register the current executable for user login.
    Install,
    /// Start the registered daemon.
    Start,
    /// Stop the registered daemon.
    Stop,
    /// Show registration and daemon state.
    Status,
    /// Remove automatic startup without removing Reactor data.
    Uninstall,
}

pub(super) fn execute(format: OutputFormat, arguments: &ServiceArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let status = match &arguments.command {
        ServiceCommand::Install => crate::service::install(&paths, &app.instance_id)?,
        ServiceCommand::Start => crate::service::start(&paths, &app.instance_id)?,
        ServiceCommand::Stop => crate::service::stop(&paths, &app.instance_id)?,
        ServiceCommand::Status => crate::service::status(&paths, &app.instance_id)?,
        ServiceCommand::Uninstall => crate::service::uninstall(&paths, &app.instance_id)?,
    };
    write_status(format, &app.instance_id, status)
}

fn write_status(format: OutputFormat, instance_id: &str, status: ServiceStatus) -> Result<()> {
    match format {
        OutputFormat::Text => {
            println!("platform: {}", status.platform);
            println!("installed: {}", status.installed);
            println!("running: {}", status.running);
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(instance_id, status))
        }
    }
}
