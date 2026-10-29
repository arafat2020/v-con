//! Command-line entry point.
//!
//! No subcommand starts the TUI. `init`, `list` (`ls`), `check`, `connect`
//! (`c`), `import`, `exec` (`x`), `status` (`st`), `ping`, `doctor`,
//! `completions`, and `man` share [`crate::services::inventory`] where needed.

mod commands;

use crate::error::Error;
use crate::paths::{self, Paths};
use crate::services::inventory;
use clap::{Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;

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

/// Subcommands that are implemented. Later commands stay out until their phase.
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
    /// Open an interactive SSH session. `c` is an alias.
    #[command(alias = "c")]
    Connect {
        /// Host name, for example `web-1`.
        name: String,
    },
    /// Import hosts. The source is `ssh-config`.
    Import {
        /// Source name. Only `ssh-config` is supported.
        source: String,
        /// Config file to read. The default is `~/.ssh/config`.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Print the plan and do not write `hosts.toml`.
        #[arg(long)]
        dry_run: bool,
        /// Replace hosts that already use an imported name.
        #[arg(long)]
        overwrite: bool,
    },
    /// Run one command on many hosts. `x` is an alias.
    #[command(alias = "x")]
    Exec {
        /// Selector such as `web-1` or `@web`.
        selector: String,
        /// Remote command. Put it after `--`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
        /// Max concurrent hosts. The default is `parallel` from config.toml.
        #[arg(short = 'p', long)]
        parallel: Option<usize>,
        /// Per-host timeout in seconds. `0` waits without a limit. The default is 30.
        #[arg(short = 't', long)]
        timeout: Option<u64>,
        /// Print each line as it arrives.
        #[arg(long)]
        stream: bool,
        /// Grouped output is the default. Combining this with `--stream` is an error.
        #[arg(long)]
        grouped: bool,
        /// Hide output from hosts that succeeded.
        #[arg(long)]
        only_failed: bool,
        /// Print each ssh command and do not run it.
        #[arg(long)]
        dry_run: bool,
        /// Prefix the remote command with `sudo -n`.
        #[arg(long)]
        sudo: bool,
    },
    /// One-shot health table. `st` is an alias.
    #[command(alias = "st")]
    Status {
        /// Selector such as `web-1` or `@web`. Absent means every host.
        selector: Option<String>,
        /// Sort by host, status, cpu, load, mem, disk, net, or up.
        #[arg(long, default_value = "host")]
        sort: String,
        /// Skip the second sample. CPU % and network rates stay empty.
        #[arg(long)]
        no_cpu: bool,
    },
    /// Measure ssh latency with a remote `true`.
    Ping {
        /// Selector such as `web-1` or `@web`. Absent means every host.
        selector: Option<String>,
        /// Max concurrent hosts. The default is `parallel` from config.toml.
        #[arg(short = 'p', long)]
        parallel: Option<usize>,
        /// Per-host timeout in seconds. The default is 10.
        #[arg(short = 't', long)]
        timeout: Option<u64>,
    },
    /// Upload a local file or directory to matched hosts.
    Push {
        /// Selector such as `web-1` or `@web`.
        selector: String,
        /// Local file or directory path.
        local: PathBuf,
        /// Remote destination directory.
        remote: String,
    },
    /// Download from one host into a local directory.
    Pull {
        /// Host name.
        host: String,
        /// Remote file or directory path.
        remote: String,
        /// Local destination directory.
        local: PathBuf,
    },
    /// Foreground port forward. Ctrl+C stops.
    Tunnel {
        /// Host name.
        host: String,
        /// Compact spec (`L:…`) or a saved tunnel name on that host.
        spec: String,
    },
    /// List or run saved command snippets.
    Snippet {
        #[command(subcommand)]
        action: SnippetCommand,
    },
    /// Print the audit log.
    Log {
        /// Restrict to this host.
        #[arg(long)]
        host: Option<String>,
        /// Restrict to this action (`exec`, `snippet`, …).
        #[arg(long)]
        action: Option<String>,
        /// Only entries newer than this duration (`1d`, `12h`, `30m`).
        #[arg(long)]
        since: Option<String>,
        /// Only entries with a non-zero host result.
        #[arg(long)]
        failed: bool,
    },
    /// Check ssh, config, agent, control dir, and terminal.
    Doctor,
    /// Print a shell completion script.
    Completions {
        /// Shell to generate for: bash, zsh, fish, powershell, elvish.
        shell: Shell,
    },
    /// Print the man page (troff) to stdout.
    Man,
}

/// `vcon snippet` subcommands.
#[derive(Debug, Subcommand)]
pub enum SnippetCommand {
    /// Print saved snippets.
    List,
    /// Render and run a snippet on a selector.
    Run {
        /// Snippet name.
        name: String,
        /// Host selector.
        selector: String,
        /// Template variable `key=value`. Repeatable.
        #[arg(long = "var", value_name = "KEY=VALUE")]
        vars: Vec<String>,
    },
}

/// Parse argv and run the selected command. The process exit code is the return value.
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    init_logging(&Paths::resolve(cli.config_dir.as_deref()), cli.verbose);
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
        Some(Command::Connect { name }) => {
            let (bundle, config) = inventory::load(&paths, &paths::local_user())?;
            commands::connect::run(&bundle, &config, &paths, name)?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Import {
            source,
            file,
            dry_run,
            overwrite,
        }) => {
            if source != "ssh-config" {
                return Err(Error::Message(format!(
                    "unknown import source `{source}`; use ssh-config"
                )));
            }
            let config_file = file
                .clone()
                .unwrap_or_else(crate::infra::ssh_config::default_path);
            let result = commands::import::run(
                &paths.hosts_file(),
                &config_file,
                *dry_run,
                *overwrite,
                &paths::local_user(),
            )?;
            println!("{}", result.text);
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Exec {
            selector,
            command,
            parallel,
            timeout,
            stream,
            grouped,
            only_failed,
            dry_run,
            sudo,
        }) => {
            let (bundle, config) = inventory::load(&paths, &paths::local_user())?;
            commands::exec::run(
                &bundle,
                &config,
                &paths,
                &commands::exec::ExecRequest {
                    selector,
                    command,
                    parallel: *parallel,
                    timeout: *timeout,
                    stream: *stream,
                    grouped: *grouped,
                    only_failed: *only_failed,
                    dry_run: *dry_run,
                    sudo: *sudo,
                    json: cli.json,
                    yes: cli.yes,
                    audit_action: None,
                    audit_command: None,
                },
            )
        }
        Some(Command::Status {
            selector,
            sort,
            no_cpu,
        }) => {
            let (bundle, config) = inventory::load(&paths, &paths::local_user())?;
            commands::status::run(
                &bundle,
                &config,
                &paths,
                &commands::status::StatusRequest {
                    selector: selector.as_deref(),
                    sort,
                    json: cli.json,
                    sample_cpu: !*no_cpu,
                },
            )
        }
        Some(Command::Ping {
            selector,
            parallel,
            timeout,
        }) => {
            let (bundle, config) = inventory::load(&paths, &paths::local_user())?;
            commands::ping::run(
                &bundle,
                &config,
                &paths,
                &commands::ping::PingRequest {
                    selector: selector.as_deref(),
                    parallel: *parallel,
                    timeout: *timeout,
                },
            )
        }
        Some(Command::Push {
            selector,
            local,
            remote,
        }) => {
            let (bundle, config) = inventory::load(&paths, &paths::local_user())?;
            commands::push::run(
                &bundle,
                &config,
                &paths,
                &commands::push::PushRequest {
                    selector,
                    local,
                    remote,
                    yes: cli.yes,
                },
            )
        }
        Some(Command::Pull {
            host,
            remote,
            local,
        }) => {
            let (bundle, config) = inventory::load(&paths, &paths::local_user())?;
            commands::pull::run(
                &bundle,
                &config,
                &paths,
                &commands::pull::PullRequest {
                    host,
                    remote,
                    local,
                },
            )
        }
        Some(Command::Tunnel { host, spec }) => {
            let (bundle, config) = inventory::load(&paths, &paths::local_user())?;
            commands::tunnel::run(&bundle, &config, &paths, host, spec)?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Snippet { action }) => match action {
            SnippetCommand::List => commands::snippet::list(&paths, cli.json),
            SnippetCommand::Run {
                name,
                selector,
                vars,
            } => {
                let (bundle, config) = inventory::load(&paths, &paths::local_user())?;
                commands::snippet::run(
                    &bundle,
                    &config,
                    &paths,
                    &commands::snippet::RunRequest {
                        name,
                        selector,
                        vars,
                        yes: cli.yes,
                        json: cli.json,
                    },
                )
            }
        },
        Some(Command::Log {
            host,
            action,
            since,
            failed,
        }) => commands::log::run(
            &paths,
            &commands::log::LogRequest {
                host: host.as_deref(),
                action: action.as_deref(),
                since: since.as_deref(),
                failed: *failed,
                json: cli.json,
            },
        ),
        Some(Command::Doctor) => commands::doctor::run(&paths),
        Some(Command::Completions { shell }) => commands::completions::run(*shell),
        Some(Command::Man) => commands::man::run(),
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

/// Write logs to the state directory. `-v` is debug and `-vv` is trace.
///
/// With no `-v`, `VCON_LOG` selects the filter and the default is `info`.
/// A log file that cannot be opened is skipped so connecting still works.
fn init_logging(paths: &Paths, verbose: u8) {
    let _ = crate::infra::fsutil::create_dir_secure(&paths.state_dir);
    let directive = if verbose >= 2 {
        "trace".to_string()
    } else if verbose == 1 {
        "debug".to_string()
    } else {
        std::env::var("VCON_LOG").unwrap_or_else(|_| "info".to_string())
    };
    let Ok(filter) = tracing_subscriber::EnvFilter::try_new(directive) else {
        return;
    };
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_file())
    else {
        return;
    };
    let _ = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(filter)
        .with_writer(Mutex::new(file))
        .try_init();
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
