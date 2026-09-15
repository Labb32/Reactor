mod app;
mod cli;
mod config;
mod domain;
mod error;
mod ids;
mod input;
mod output;
mod paths;
mod store;
mod time;

use std::process::ExitCode;

fn main() -> ExitCode {
    cli::run()
}
