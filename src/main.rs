//! vcon browses an SSH host inventory from the terminal and from scripts.
//!
//! With no arguments, [`tui::run`] opens the Hosts screen. `vcon init`,
//! `vcon ls`, and `vcon check` are the phase 1 subcommands. Both paths load
//! the same inventory through [`services::inventory`].

mod cli;
mod config;
mod domain;
mod error;
mod infra;
mod paths;
mod services;
mod tui;

use std::process::ExitCode;

fn main() -> ExitCode {
    cli::run()
}
