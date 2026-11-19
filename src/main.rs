//! vcon browses an SSH host inventory from the terminal and from scripts.
//!
//! With no arguments, [`tui::run`] opens the Hosts screen. Subcommands share
//! the same inventory, config, and services as the TUI.

mod cli;
mod config;
mod domain;
mod error;
mod infra;
mod paths;
mod services;
mod tui;

#[cfg(test)]
#[path = "../qa/file_transfer_tests.rs"]
mod qa_file_transfer_tests;

use std::process::ExitCode;

fn main() -> ExitCode {
    cli::run()
}
