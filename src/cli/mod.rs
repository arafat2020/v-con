//! Command-line entry point.
//!
//! No subcommand starts the TUI. `init`, `list` (`ls`), and `check` are the
//! phase 1 commands. They share [`crate::services::inventory`] with the Hosts screen.

mod commands;

use crate::error::Error;
use crate::paths::{self, Paths};
use crate::services::inventory;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

/// Global flags and the optional subcommand. No subcommand opens the TUI.
#[derive(Debug, Parser)]
#[command(name = "vcon", about = "Browse an SSH host inventory")]
pub struct Cli {
    /// More logging. Repeat for debug (`-vv`).
    #[arg(short = 'v', long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,
    /// Config directory. Overrides `VCON_CONFIG_DIR`.
    #[arg(long, global = true)]
    pub config_dir: Option<PathBuf>,
    /// Machine-readable output where a command supports it.
    #[arg(long, global = true)]
    pub json: bool,
    /// Disable colours. Also set by `NO_COLOR`.
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Skip confirmation prompts.
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,
    /// Subcommand. Absent means open the TUI.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Phase 1 subcommands. Later commands stay out of this enum until their phase.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create the config directory and a commented example `hosts.toml`.
    Init,
    /// Print hosts. `ls` is an alias.
    #[command(alias = "ls")]
    List {
        /// Selector such as `web-1`, `@prod`, or `@web,!web-3`.
        selector: Option<String>,
    },
    /// Validate the inventory. Exits non-zero when any problem is found.
    Check,
}

/// Parse argv and run the selected command. The process exit code is the return value.
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    let _ = cli.yes;
    match dispatch(&cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn dispatch(cli: &Cli) -> Result<ExitCode, Error> {
    let paths = Paths::resolve(cli.config_dir.as_deref());
    let no_color = cli.no_color || std::env::var_os("NO_COLOR").is_some();
    match &cli.command {
        Some(Command::Init) => {
            println!("{}", commands::init::run(&paths.config_dir)?);
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::List { selector }) => {
            let bundle = inventory::load_hosts(&paths.hosts_file(), &paths::local_user())?;
            let result = commands::list::run(&bundle, selector.as_deref(), cli.json)?;
            if !result.stderr.is_empty() {
                eprintln!("{}", result.stderr);
            }
            if !result.stdout.is_empty() {
                println!("{}", result.stdout);
            }
            Ok(result.code)
        }
        Some(Command::Check) => {
            match inventory::load_hosts(&paths.hosts_file(), &paths::local_user()) {
                Ok(bundle) => {
                    let result = commands::check::run(&bundle);
                    if result.code == ExitCode::SUCCESS {
                        println!("{}", result.text);
                    } else {
                        eprintln!("{}", result.text);
                    }
                    Ok(result.code)
                }
                Err(error) => {
                    let result = commands::check::load_error(&error);
                    eprintln!("{}", result.text);
                    Ok(result.code)
                }
            }
        }
        None => {
            if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                eprintln!("vcon's TUI needs a terminal; use subcommands for scripting");
                return Ok(ExitCode::from(1));
            }
            let (bundle, config) = inventory::load(&paths, &paths::local_user())?;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|error| Error::Message(error.to_string()))?;
            runtime.block_on(crate::tui::run(bundle, config, paths, no_color))?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
