//! Build the argv for a system `ssh` invocation.
//!
//! Every later feature (connect, exec, sftp) should call [`build_ssh_command`]
//! so port, key, jump, and multiplexing stay consistent. The detail pane shows
//! [`command_line`] of that argv, and `y` copies it.

use crate::domain::host::{EffectiveHost, is_plain_jump};
use std::collections::HashSet;

/// Flags that depend on the caller rather than the host record.
///
/// The detail pane uses multiplexing from config and leaves `batch_mode` off,
/// because that line is the interactive command. Non-interactive callers set
/// `batch_mode` and may override the timeout.
#[derive(Debug, Clone, Default)]
pub struct SshCommandOptions {
    /// Pass `-o BatchMode=yes` so ssh never prompts.
    pub batch_mode: bool,
    /// Overrides the host's `connect_timeout` when set.
    pub connect_timeout: Option<u64>,
    /// Emit `ControlMaster`, `ControlPath`, and `ControlPersist` when a directory is set.
    pub multiplexing: bool,
    /// Directory that holds the control socket. Usually `~/.cache/vcon/cm`.
    pub control_path_dir: Option<String>,
}

/// Argv beginning with `ssh`.
///
/// Port 22 is omitted. `~` in the identity path is expanded. A jump that names
/// another host becomes `user@address` (and `:port` when the port is not 22).
/// The hop closest to the client is first, which is what `-J` expects.
/// `ssh_options` are emitted in sorted key order.
pub fn build_ssh_command(
    host: &EffectiveHost,
    hosts: &[EffectiveHost],
    options: &SshCommandOptions,
) -> Vec<String> {
    let mut args = vec!["ssh".to_string()];

    if host.port != 22 {
        args.push("-p".to_string());
        args.push(host.port.to_string());
    }

    if let Some(identity) = &host.identity_file {
        args.push("-i".to_string());
        args.push(shellexpand::tilde(identity).into_owned());
    }

    let jumps = jump_chain(host, hosts);
    if !jumps.is_empty() {
        args.push("-J".to_string());
        args.push(jumps.join(","));
    }

    if options.batch_mode {
        args.push("-o".to_string());
        args.push("BatchMode=yes".to_string());
    }

    if let Some(timeout) = options.connect_timeout.or(host.connect_timeout) {
        args.push("-o".to_string());
        args.push(format!("ConnectTimeout={timeout}"));
    }

    if options.multiplexing
        && let Some(dir) = &options.control_path_dir
    {
        args.push("-o".to_string());
        args.push("ControlMaster=auto".to_string());
        args.push("-o".to_string());
        args.push(format!("ControlPath={dir}/%C"));
        args.push("-o".to_string());
        args.push("ControlPersist=10m".to_string());
    }

    for (key, value) in &host.ssh_options {
        args.push("-o".to_string());
        args.push(format!("{key}={value}"));
    }

    args.push(host.destination());
    args
}

/// Arguments after the ssh binary for a non-interactive command.
///
/// `remote_command` is one argv element, so the remote shell parses quotes,
/// `$VARS`, pipes, and `&&`. BatchMode is always on. `ConnectTimeout` uses
/// `connect_timeout` when it is set, otherwise the host default.
pub fn exec_arguments(
    host: &EffectiveHost,
    hosts: &[EffectiveHost],
    connect_timeout: Option<u64>,
    multiplexing: bool,
    control_dir: Option<&str>,
    remote_command: &str,
) -> Vec<String> {
    let options = SshCommandOptions {
        batch_mode: true,
        connect_timeout,
        multiplexing,
        control_path_dir: control_dir.map(str::to_string),
    };
    let mut args = build_ssh_command(host, hosts, &options);
    args.remove(0);
    args.push(remote_command.to_string());
    args
}

/// Shell-quote `args` into one line safe to display and paste.
pub fn command_line(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolved `-J` hops for `host`, client first.
///
/// Plain names are looked up in `hosts` and the walk follows each hop's own
/// jump. A raw `user@host[:port]` stops the walk. A repeated name stops it too,
/// so an invalid cycle cannot loop forever. The hop's identity file is not
/// included: ssh-agent or `~/.ssh/config` authenticates the bastion.
pub fn jump_chain(host: &EffectiveHost, hosts: &[EffectiveHost]) -> Vec<String> {
    let mut chain = Vec::new();
    let mut seen = HashSet::from([host.name.clone()]);
    let mut next = host.jump.clone();
    while let Some(jump) = next {
        if is_plain_jump(&jump) {
            if !seen.insert(jump.clone()) {
                break;
            }
            if let Some(hop) = hosts.iter().find(|candidate| candidate.name == jump) {
                chain.push(jump_destination(hop));
                next = hop.jump.clone();
                continue;
            }
        }
        chain.push(jump);
        break;
    }
    chain.reverse();
    chain
}

/// Jump names for the connect banner, client first.
///
/// A plain name stays a name (`bastion`). A raw `user@host[:port]` is kept as
/// written. The order matches [`jump_chain`].
pub fn jump_labels(host: &EffectiveHost, hosts: &[EffectiveHost]) -> Vec<String> {
    let mut chain = Vec::new();
    let mut seen = HashSet::from([host.name.clone()]);
    let mut next = host.jump.clone();
    while let Some(jump) = next {
        if is_plain_jump(&jump) {
            if !seen.insert(jump.clone()) {
                break;
            }
            if let Some(hop) = hosts.iter().find(|candidate| candidate.name == jump) {
                chain.push(jump.clone());
                next = hop.jump.clone();
                continue;
            }
        }
        chain.push(jump);
        break;
    }
    chain.reverse();
    chain
}

fn jump_destination(host: &EffectiveHost) -> String {
    if host.port == 22 {
        host.destination()
    } else {
        format!("{}@{}:{}", host.user, host.address, host.port)
    }
}

/// Quote an argument that contains shell metacharacters. Plain tokens are left as-is.
fn shell_quote(arg: &str) -> String {
    if arg.is_empty()
        || arg
            .chars()
            .any(|ch| ch.is_whitespace() || "\\\"'$#!&|;<>()*?[]{}".contains(ch))
    {
        format!("'{}'", arg.replace('\'', "'\\''"))
    } else {
        arg.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::host::EffectiveHost;
    use std::collections::BTreeMap;

    fn host(name: &str, user: &str, address: &str, port: u16) -> EffectiveHost {
        EffectiveHost {
            name: name.into(),
            address: address.into(),
            user: user.into(),
            port,
            identity_file: None,
            jump: None,
            tags: Vec::new(),
            notes: None,
            ssh_options: BTreeMap::new(),
            disabled: false,
            connect_timeout: None,
            tunnels: Vec::new(),
        }
    }

    #[test]
    fn includes_port_key_user_and_resolved_jump() {
        let mut bastion = host("bastion", "admin", "198.51.100.5", 22);
        bastion.identity_file = Some("~/.ssh/id_ed25519".into());
        let mut web = host("web-1", "deploy", "203.0.113.10", 2222);
        web.jump = Some("bastion".into());
        web.identity_file = Some("~/.ssh/db_key".into());
        let mut options = BTreeMap::new();
        options.insert("ServerAliveInterval".into(), "30".into());
        options.insert("StrictHostKeyChecking".into(), "accept-new".into());
        web.ssh_options = options;
        web.connect_timeout = Some(5);

        let args = build_ssh_command(
            &web,
            &[bastion.clone(), web.clone()],
            &SshCommandOptions {
                batch_mode: true,
                connect_timeout: None,
                multiplexing: true,
                control_path_dir: Some("/tmp/vcon/cm".into()),
            },
        );

        assert_eq!(args[0], "ssh");
        assert!(args.windows(2).any(|pair| pair == ["-p", "2222"]));
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-i" && pair[1].ends_with("/.ssh/db_key"))
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-J", "admin@198.51.100.5"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-o", "BatchMode=yes"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "ConnectTimeout=5"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "ControlMaster=auto"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "ControlPath=/tmp/vcon/cm/%C"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "ControlPersist=10m"])
        );
        let options: Vec<&str> = args
            .windows(2)
            .filter(|pair| {
                pair[0] == "-o"
                    && pair[1].contains('=')
                    && !pair[1].starts_with("Batch")
                    && !pair[1].starts_with("Connect")
                    && !pair[1].starts_with("Control")
            })
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(
            options,
            vec!["ServerAliveInterval=30", "StrictHostKeyChecking=accept-new"]
        );
        assert_eq!(args.last().map(String::as_str), Some("deploy@203.0.113.10"));
    }

    #[test]
    fn default_port_omits_port_flag_and_jump_chain_is_client_first() {
        let edge = host("edge", "root", "198.51.100.1", 22);
        let mut bastion = host("bastion", "admin", "198.51.100.5", 2222);
        bastion.jump = Some("edge".into());
        let mut web = host("web-1", "deploy", "10.0.0.8", 22);
        web.jump = Some("bastion".into());
        let fleet = [edge, bastion, web.clone()];
        let args = build_ssh_command(&web, &fleet, &SshCommandOptions::default());
        assert!(!args.iter().any(|arg| arg == "-p"));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-J", "root@198.51.100.1,admin@198.51.100.5:2222"])
        );
        assert_eq!(args.last().map(String::as_str), Some("deploy@10.0.0.8"));
    }

    #[test]
    fn raw_jump_is_passed_through() {
        let mut web = host("web-1", "deploy", "10.0.0.8", 22);
        web.jump = Some("jumpuser@bastion.example:2200".into());
        let args = build_ssh_command(&web, &[web.clone()], &SshCommandOptions::default());
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-J", "jumpuser@bastion.example:2200"])
        );
    }

    #[test]
    fn exec_keeps_the_remote_command_as_one_argument() {
        let web = host("web-1", "deploy", "203.0.113.10", 22);
        let command = "echo '$HOME' && true";
        let args = exec_arguments(
            &web,
            std::slice::from_ref(&web),
            Some(15),
            true,
            Some("/tmp/vcon/cm"),
            command,
        );
        assert!(args.windows(2).any(|pair| pair == ["-o", "BatchMode=yes"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "ConnectTimeout=15"])
        );
        assert_eq!(args.last().map(String::as_str), Some(command));
        assert!(!args.iter().any(|arg| arg == "ssh"));
    }
}
