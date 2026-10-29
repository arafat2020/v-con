//! vcon browses an SSH host inventory from the terminal and from scripts.
//!
//! With no arguments, [`tui::run`] opens the Hosts screen. `vcon init`,
//! `vcon ls`, `vcon check`, `vcon connect`, `vcon import`, and `vcon exec` share one inventory.

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
