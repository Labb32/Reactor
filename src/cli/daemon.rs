use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::{
    error::{ReactorError, Result},
    paths::ReactorPaths,
};

#[derive(Debug, Args)]
pub(super) struct DaemonArgs {
    #[command(subcommand)]
    command: DaemonCommand,
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Run the scheduler until the process is stopped.
    Run(DaemonRunArgs),
}

#[derive(Debug, Args)]
struct DaemonRunArgs {
    #[arg(long, value_name = "PATH")]
    config_dir: Option<PathBuf>,

    #[arg(long, value_name = "PATH")]
    data_dir: Option<PathBuf>,
}

pub(super) fn execute(arguments: &DaemonArgs) -> Result<()> {
    match &arguments.command {
        DaemonCommand::Run(arguments) => crate::daemon::run(paths(arguments)?),
    }
}

fn paths(arguments: &DaemonRunArgs) -> Result<ReactorPaths> {
    match (&arguments.config_dir, &arguments.data_dir) {
        (None, None) => ReactorPaths::discover(),
        (Some(config_dir), Some(data_dir)) => Ok(ReactorPaths {
            config_dir: config_dir.clone(),
            data_dir: data_dir.clone(),
        }),
        _ => Err(ReactorError::InvalidArgument(
            "--config-dir and --data-dir must be used together".to_owned(),
        )),
    }
}
