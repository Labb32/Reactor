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
    /// Install a Linux system service using an existing non-root account.
    InstallSystem { #[arg(long)] user: String },
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
    let instance_id = if matches!(&arguments.command, ServiceCommand::Install | ServiceCommand::InstallSystem { .. }) {
        App::open(&paths)?.instance_id
    } else {
        crate::service::registration(&paths).map(|record| record.instance_id)
            .or_else(|_| App::open(&paths).map(|app| app.instance_id))?
    };
    let status = match &arguments.command {
        ServiceCommand::Install => crate::service::install(&paths, &instance_id)?,
        ServiceCommand::InstallSystem { user } => crate::service::install_system(&paths, &instance_id, user)?,
        ServiceCommand::Start => crate::service::start(&paths, &instance_id)?,
        ServiceCommand::Stop => crate::service::stop(&paths, &instance_id)?,
        ServiceCommand::Status => crate::service::status(&paths, &instance_id)?,
        ServiceCommand::Uninstall => crate::service::uninstall(&paths, &instance_id)?,
    };
    write_status(format, &instance_id, status)
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
