//! `vcon completions <shell>`: print a clap completion script.

use crate::cli::Cli;
use crate::error::Error;
use clap::CommandFactory;
use clap_complete::{Shell, generate};
use std::io;
use std::process::ExitCode;

/// Generate completions for `shell` on stdout.
pub fn run(shell: Shell) -> Result<ExitCode, Error> {
    let mut command = Cli::command();
    let name = command.get_name().to_string();
    generate(shell, &mut command, name, &mut io::stdout());
    Ok(ExitCode::SUCCESS)
}
