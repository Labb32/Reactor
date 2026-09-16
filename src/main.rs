mod agents;
mod app;
mod cli;
mod config;
mod domain;
mod error;
mod ids;
mod input;
mod output;
mod paths;
mod plan;
mod store;
mod time;
mod workspace;

use std::process::ExitCode;

fn main() -> ExitCode {
    cli::run()
}
