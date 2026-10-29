//! Open an interactive SSH session.
//!
//! The TUI and `vcon connect` both build the argv here, so the jump chain and
//! the multiplexing flags match the command copied with `y`. The CLI records
//! the attempt and then replaces the process. The TUI records it and suspends
//! instead, because it has to come back.

use crate::domain::host::EffectiveHost;
use crate::domain::selector::{Term, evaluate_selector, parse_selector};
use crate::error::Error;
use crate::infra::ssh::command::{SshCommandOptions, build_ssh_command, command_line, jump_labels};
use crate::infra::state_store;
use chrono::Utc;
use std::path::{Path, PathBuf};

/// Everything needed to spawn ssh for one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectPlan {
    /// Inventory name that was recorded in the state file.
    pub host_name: String,
    /// One line printed before ssh starts.
    pub banner: String,
    /// Arguments after the ssh binary.
    pub args: Vec<String>,
    /// Full command, for the debug log.
    pub command_line: String,
}

/// Resolve a single host name. An unknown name includes a close suggestion.
pub fn resolve_host<'a>(
    name: &str,
    hosts: &'a [EffectiveHost],
) -> Result<&'a EffectiveHost, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("empty host name".to_string());
    }
    let terms = parse_selector(name)?;
    let [Term::Exact(_)] = terms.as_slice() else {
        return Err(format!("`{name}` is not a host name"));
    };
    let selection = evaluate_selector(&terms, hosts);
    match selection.hosts.as_slice() {
        [host] => Ok(*host),
        _ => Err(selection
            .error
            .unwrap_or_else(|| format!("no host `{name}`"))),
    }
}

/// Argv and banner for `host`. Does not touch the state file.
pub fn plan(
    host: &EffectiveHost,
    hosts: &[EffectiveHost],
    options: &SshCommandOptions,
) -> ConnectPlan {
    let argv = build_ssh_command(host, hosts, options);
    let args = argv.iter().skip(1).cloned().collect();
    ConnectPlan {
        host_name: host.name.clone(),
        banner: banner(host, hosts),
        command_line: command_line(&argv),
        args,
    }
}

/// Options shared by the TUI handoff and `vcon connect`.
///
/// Batch mode stays off so the user can answer a password or host-key prompt.
pub fn session_options(multiplexing: bool, control_dir: &Path) -> SshCommandOptions {
    SshCommandOptions {
        batch_mode: false,
        connect_timeout: None,
        multiplexing,
        control_path_dir: multiplexing.then(|| control_dir.display().to_string()),
    }
}

/// Resolve `name`, record the attempt, and return the plan.
///
/// A state file that cannot be read or written is logged and does not stop
/// the connection.
pub fn prepare(
    hosts: &[EffectiveHost],
    state_path: &Path,
    options: &SshCommandOptions,
    name: &str,
) -> Result<ConnectPlan, Error> {
    let host = resolve_host(name, hosts).map_err(Error::Message)?;
    let plan = plan(host, hosts, options);
    let (mut state, warning) = state_store::load(state_path);
    if let Some(warning) = warning {
        tracing::warn!("{warning}");
    }
    state.record_connect(&plan.host_name, Utc::now());
    if let Err(error) = state_store::save(state_path, &state) {
        tracing::warn!("could not save connection history: {error}");
        eprintln!("warning: could not save connection history: {error}");
    }
    tracing::debug!(command = %plan.command_line, "ssh command");
    Ok(plan)
}

/// Program and arguments for `$EDITOR` (or `vi`) opened on `path`.
///
/// `EDITOR` is split on whitespace, so `code --wait` keeps its flag. `VISUAL`
/// is used when `EDITOR` is unset.
pub fn editor_command(path: &Path) -> (PathBuf, Vec<String>) {
    let spec = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());
    editor_command_from(&spec, path)
}

/// Split `spec` the way [`editor_command`] splits `$EDITOR`.
pub fn editor_command_from(spec: &str, path: &Path) -> (PathBuf, Vec<String>) {
    let mut parts: Vec<String> = spec.split_whitespace().map(str::to_string).collect();
    if parts.is_empty() {
        parts.push("vi".to_string());
    }
    let program = PathBuf::from(parts.remove(0));
    parts.push(path.display().to_string());
    (program, parts)
}

/// Replace this process with ssh. Only returns when `exec` fails.
#[cfg(unix)]
pub fn replace_process(program: &Path, args: &[String]) -> Result<(), Error> {
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    let error = Command::new(program).args(args).exec();
    Err(Error::Message(format!(
        "failed to exec {}: {error}",
        program.display()
    )))
}

/// Replace this process with ssh. Only Unix can do that.
#[cfg(not(unix))]
pub fn replace_process(program: &Path, _args: &[String]) -> Result<(), Error> {
    Err(Error::Message(format!(
        "vcon connect cannot replace the process on this system ({})",
        program.display()
    )))
}

fn banner(host: &EffectiveHost, hosts: &[EffectiveHost]) -> String {
    let labels = jump_labels(host, hosts);
    if labels.is_empty() {
        format!("Connecting to {} ({})…", host.name, host.destination())
    } else {
        format!(
            "Connecting to {} ({} via {})…",
            host.name,
            host.destination(),
            labels.join(" → ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::host::{Defaults, Host, resolve};
    use crate::infra::ssh::command::SshCommandOptions;

    fn host(name: &str, address: &str, user: &str, jump: Option<&str>) -> EffectiveHost {
        resolve(
            &Host {
                name: name.into(),
                address: address.into(),
                user: Some(user.into()),
                jump: jump.map(str::to_string),
                ..Host::default()
            },
            &Defaults::default(),
            "ada",
        )
    }

    fn fleet() -> Vec<EffectiveHost> {
        vec![
            host("bastion", "198.51.100.5", "admin", None),
            host("web-1", "203.0.113.10", "deploy", Some("bastion")),
            host("web-2", "203.0.113.11", "deploy", Some("bastion")),
        ]
    }

    #[test]
    fn unknown_name_suggests_a_close_host() {
        let hosts = fleet();
        let error = resolve_host("wbe-1", &hosts).unwrap_err();
        assert!(error.contains("did you mean `web-1`"), "{error}");
    }

    #[test]
    fn banner_names_the_bastion_and_argv_uses_jump() {
        let hosts = fleet();
        let web = resolve_host("web-1", &hosts).unwrap();
        let plan = plan(web, &hosts, &SshCommandOptions::default());
        assert_eq!(
            plan.banner,
            "Connecting to web-1 (deploy@203.0.113.10 via bastion)…"
        );
        assert!(
            plan.args
                .windows(2)
                .any(|pair| pair == ["-J", "admin@198.51.100.5"])
        );
        assert_eq!(
            plan.args.last().map(String::as_str),
            Some("deploy@203.0.113.10")
        );
    }

    #[test]
    fn prepare_records_the_attempt_before_ssh_starts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let hosts = fleet();
        let plan = prepare(&hosts, &path, &SshCommandOptions::default(), "web-1").unwrap();
        assert_eq!(plan.host_name, "web-1");
        let (state, warning) = state_store::load(&path);
        assert!(warning.is_none());
        assert_eq!(state.hosts["web-1"].connect_count, 1);
        assert!(state.hosts["web-1"].last_connected.is_some());
    }

    #[test]
    fn editor_appends_the_file_and_keeps_flags() {
        let (program, args) = editor_command_from("code --wait", Path::new("/tmp/hosts.toml"));
        assert_eq!(program, PathBuf::from("code"));
        assert_eq!(
            args,
            vec!["--wait".to_string(), "/tmp/hosts.toml".to_string()]
        );
        let (program, args) = editor_command_from("   ", Path::new("hosts.toml"));
        assert_eq!(program, PathBuf::from("vi"));
        assert_eq!(args, vec!["hosts.toml".to_string()]);
    }
}
