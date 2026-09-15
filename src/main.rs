mod app;
mod config;
#[allow(dead_code)]
mod domain;
mod error;
mod paths;
mod store;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::{app::App, error::Result, paths::ReactorPaths};

#[derive(Debug, Parser)]
#[command(name = "reactor", version, about = "Schedule reviewed AI agent work")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize Reactor's configuration and local database.
    Init,
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Init => {
            let paths = ReactorPaths::discover()?;
            let initialized = App::initialize(paths)?;

            println!("Reactor initialized.");
            println!("config: {}", initialized.config_file.display());
            println!("database: {}", initialized.database_file.display());
            println!("schema: {}", initialized.schema_version);
        }
    }

    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("reactor: {error}");
            ExitCode::FAILURE
        }
    }
}
