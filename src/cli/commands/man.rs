//! `vcon man`: print a troff man page to stdout.

use crate::cli::Cli;
use crate::error::Error;
use clap::CommandFactory;
use clap_mangen::Man;
use std::io;
use std::process::ExitCode;

/// Render the man page for the CLI definition.
pub fn run() -> Result<ExitCode, Error> {
    let command = Cli::command();
    let man = Man::new(command);
    man.render(&mut io::stdout())
        .map_err(|error| Error::Message(format!("could not write man page: {error}")))?;
    Ok(ExitCode::SUCCESS)
}
