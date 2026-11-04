mod agents;
mod app;
mod checkpoint;
mod cli;
mod config;
mod daemon;
mod domain;
mod error;
mod execution_control;
mod ids;
mod input;
mod interrupt;
mod output;
mod paths;
mod permissions;
mod plan;
mod plan_draft;
mod process_supervisor;
mod run;
mod run_log;
mod runner;
mod schedule;
mod service;
mod store;
mod time;
mod workspace;
mod workspace_execution;

use std::process::ExitCode;

fn main() -> ExitCode {
    cli::run()
}
