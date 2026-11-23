//! Build the argv for a system `ssh` invocation.
//!
//! Every later feature (connect, exec, sftp) should call [`build_ssh_command`]
//! so port, key, jump, and multiplexing stay consistent. The detail pane shows
//! [`command_line`] of that argv, and `y` copies it.

use crate::domain::host::{EffectiveHost, is_plain_jump};
use std::collections::HashSet;
use std::path::Path;

/// Flags that depend on the caller rather than the host record.
///
/// The detail pane uses multiplexing from config and leaves `batch_mode` off,
/// because that line is the interactive command. Non-interactive callers set
/// `batch_mode` and may override the timeout.
#[derive(Debug, Clone, Default)]
pub struct SshCommandOptions {
    /// Pass `-o BatchMode=yes` so ssh never prompts.
    pub batch_mode: bool,
    /// A password for this host is in the OS keychain. Omit batch mode and
    /// allow one password prompt, which the askpass helper answers.
    pub password_auth: bool,
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

    emit_jump(&mut args, host, hosts);

    if options.batch_mode && !options.password_auth {
        args.push("-o".to_string());
        args.push("BatchMode=yes".to_string());
    }
    if options.password_auth {
        args.push("-o".to_string());
        args.push("NumberOfPasswordPrompts=1".to_string());
        // keyboard-interactive is a second password prompt. One failure is enough.
        args.push("-o".to_string());
        args.push("KbdInteractiveAuthentication=no".to_string());
        args.push("-o".to_string());
        args.push("PreferredAuthentications=publickey,password".to_string());
    }

    if let Some(timeout) = options.connect_timeout.or(host.connect_timeout) {
        args.push("-o".to_string());
        args.push(format!("ConnectTimeout={timeout}"));
    }

    if options.multiplexing
        && let Some(dir) = &options.control_path_dir
    {
        // OpenSSH will not create the socket directory. A missing
        // `~/.cache/vcon/cm` fails every multiplexed command with
        // `unix_listener: cannot bind ... No such file or directory`.
        ensure_control_dir(dir);
        args.push("-o".to_string());
        args.push("ControlMaster=auto".to_string());
        args.push("-o".to_string());
        args.push(format!("ControlPath={dir}/%C"));
        args.push("-o".to_string());
        args.push("ControlPersist=10m".to_string());
        // A dead network otherwise leaves the master up, and the next
        // connection attaches to it and cannot authenticate again.
        if !host
            .ssh_options
            .keys()
            .any(|key| key.eq_ignore_ascii_case("ServerAliveInterval"))
        {
            args.push("-o".to_string());
            args.push("ServerAliveInterval=15".to_string());
            args.push("-o".to_string());
            args.push("ServerAliveCountMax=3".to_string());
        }
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
/// `$VARS`, pipes, and `&&`. BatchMode is on. `ConnectTimeout` uses
/// `connect_timeout` when it is set, otherwise the host default.
pub fn exec_arguments(
    host: &EffectiveHost,
    hosts: &[EffectiveHost],
    connect_timeout: Option<u64>,
    multiplexing: bool,
    control_dir: Option<&str>,
    remote_command: &str,
) -> Vec<String> {
    exec_arguments_auth(
        host,
        hosts,
        connect_timeout,
        multiplexing,
        control_dir,
        remote_command,
        false,
    )
}

/// [`exec_arguments`], with keychain password auth when `password_auth` is set.
///
/// Batch mode stays on unless a password was saved. The password itself is
/// not placed in the argument list.
pub fn exec_arguments_auth(
    host: &EffectiveHost,
    hosts: &[EffectiveHost],
    connect_timeout: Option<u64>,
    multiplexing: bool,
    control_dir: Option<&str>,
    remote_command: &str,
    password_auth: bool,
) -> Vec<String> {
    let options = SshCommandOptions {
        batch_mode: !password_auth,
        password_auth,
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

/// How to reach `host` through its jump chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JumpDirective {
    /// OpenSSH `-J` list, client first. No hop needs its own identity file.
    ProxyJump(String),
    /// Shell command for `-o ProxyCommand=`. Each hop can carry `-i`.
    ProxyCommand(String),
}

/// Add `-J` or a `ProxyCommand` that logs into each hop with its own key.
fn emit_jump(args: &mut Vec<String>, host: &EffectiveHost, hosts: &[EffectiveHost]) {
    let Some(directive) = jump_directive(host, hosts) else {
        return;
    };
    match directive {
        JumpDirective::ProxyJump(spec) => {
            args.push("-J".to_string());
            args.push(spec);
        }
        JumpDirective::ProxyCommand(command) => {
            args.push("-o".to_string());
            args.push(format!("ProxyCommand={command}"));
        }
    }
}

/// `-J` when every hop can authenticate without its own key. Otherwise a
/// `ProxyCommand` so each hop is started with `-i` and `IdentitiesOnly=yes`.
pub(crate) fn jump_directive(
    host: &EffectiveHost,
    hosts: &[EffectiveHost],
) -> Option<JumpDirective> {
    let hops = resolved_hops(host, hosts);
    if hops.is_empty() {
        return None;
    }
    let needs_key = hops
        .iter()
        .any(|hop| matches!(hop, Hop::Inventory(host) if host.identity_file.is_some()));
    if needs_key {
        Some(JumpDirective::ProxyCommand(proxy_command(&hops)))
    } else {
        let spec = hops
            .iter()
            .map(|hop| match hop {
                Hop::Inventory(host) => jump_destination(host),
                Hop::Raw(spec) => spec.clone(),
            })
            .collect::<Vec<_>>()
            .join(",");
        Some(JumpDirective::ProxyJump(spec))
    }
}

/// One hop between the client and the target, client first.
#[derive(Debug, Clone)]
enum Hop<'a> {
    Inventory(&'a EffectiveHost),
    Raw(String),
}

/// Jump hops for `host`, client first. A cycle or an unknown name stops the walk.
fn resolved_hops<'a>(host: &'a EffectiveHost, hosts: &'a [EffectiveHost]) -> Vec<Hop<'a>> {
    let mut chain = Vec::new();
    let mut seen = HashSet::from([host.name.clone()]);
    let mut next = host.jump.clone();
    while let Some(jump) = next {
        if is_plain_jump(&jump) {
            if !seen.insert(jump.clone()) {
                break;
            }
            if let Some(hop) = hosts.iter().find(|candidate| candidate.name == jump) {
                chain.push(Hop::Inventory(hop));
                next = hop.jump.clone();
                continue;
            }
        }
        chain.push(Hop::Raw(jump));
        break;
    }
    chain.reverse();
    chain
}

/// Shell command that tunnels to `%h:%p` through `hops` (client first).
///
/// Only the outermost `-W` uses `%h:%p`. Earlier hops use a literal address so
/// OpenSSH's percent expansion does not rewrite them to the final target.
fn proxy_command(hops: &[Hop<'_>]) -> String {
    fn build(hops: &[Hop<'_>], w_dest: &str) -> String {
        let hop = hops.last().expect("hop");
        let earlier = &hops[..hops.len() - 1];
        let mut parts = hop_ssh_args(hop);
        if !earlier.is_empty() {
            let via = literal_hostport(hop);
            let inner = build(earlier, &via);
            parts.push("-o".to_string());
            parts.push(format!("ProxyCommand={inner}"));
        }
        parts.push("-W".to_string());
        parts.push(w_dest.to_string());
        parts.push(hop_login(hop));
        parts
            .iter()
            .map(|part| shell_quote(part))
            .collect::<Vec<_>>()
            .join(" ")
    }
    build(hops, "%h:%p")
}

fn hop_ssh_args(hop: &Hop<'_>) -> Vec<String> {
    let mut parts = vec!["ssh".to_string()];
    if let Hop::Inventory(host) = hop
        && let Some(identity) = &host.identity_file
    {
        parts.push("-i".to_string());
        parts.push(shellexpand::tilde(identity).into_owned());
        parts.push("-o".to_string());
        parts.push("IdentitiesOnly=yes".to_string());
    }
    if let Hop::Inventory(host) = hop
        && host.port != 22
    {
        parts.push("-p".to_string());
        parts.push(host.port.to_string());
    }
    parts.push("-o".to_string());
    parts.push("BatchMode=yes".to_string());
    parts
}

fn hop_login(hop: &Hop<'_>) -> String {
    match hop {
        Hop::Inventory(host) => host.destination(),
        Hop::Raw(spec) => spec.clone(),
    }
}

fn literal_hostport(hop: &Hop<'_>) -> String {
    match hop {
        Hop::Inventory(host) => format!("{}:{}", host.address, host.port),
        Hop::Raw(spec) => raw_hostport(spec),
    }
}

/// `user@host[:port]` or `host[:port]` as `host:port`.
fn raw_hostport(spec: &str) -> String {
    let hostpart = spec.rsplit('@').next().unwrap_or(spec);
    if let Some((host, port)) = hostpart.rsplit_once(':')
        && !host.is_empty()
        && port.chars().all(|ch| ch.is_ascii_digit())
    {
        return format!("{host}:{port}");
    }
    format!("{hostpart}:22")
}

/// Resolved `-J` hops for `host`, client first.
///
/// Plain names are looked up in `hosts` and the walk follows each hop's own
/// jump. A raw `user@host[:port]` stops the walk. A repeated name stops it too,
/// so an invalid cycle cannot loop forever. When a hop has `identity_file`,
/// [`jump_directive`] emits a ProxyCommand instead of this list.
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

/// Create the ControlPath directory before ssh tries to bind the socket.
///
/// Mode is `0700` on unix. A failure is logged; ssh then reports the bind error.
fn ensure_control_dir(dir: &str) {
    if dir.is_empty() || dir == "none" {
        return;
    }
    let path = Path::new(dir);
    if let Err(error) = crate::infra::fsutil::create_dir_secure(path) {
        tracing::warn!(
            "could not create control directory {}: {error}",
            path.display()
        );
        return;
    }
    if let Err(error) = crate::infra::fsutil::set_mode(path, crate::infra::fsutil::DIR_MODE) {
        tracing::warn!(
            "could not set permissions on control directory {}: {error}",
            path.display()
        );
    }
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
                password_auth: false,
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
        let proxy = args
            .windows(2)
            .find(|pair| pair[0] == "-o" && pair[1].starts_with("ProxyCommand="))
            .map(|pair| pair[1].as_str())
            .expect("bastion identity_file needs a ProxyCommand");
        assert!(proxy.contains("id_ed25519"), "hop key missing from {proxy}");
        assert!(proxy.contains("IdentitiesOnly=yes"), "{proxy}");
        assert!(proxy.contains("-W"), "{proxy}");
        assert!(proxy.contains("%h:%p"), "{proxy}");
        assert!(proxy.contains("admin@198.51.100.5"), "{proxy}");
        assert!(!args.iter().any(|arg| arg == "-J"));
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
                    && !pair[1].starts_with("ProxyCommand=")
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
    fn jump_identity_is_shell_quoted_and_a_chain_keeps_literal_hops() {
        let mut edge = host("edge", "root", "198.51.100.1", 22);
        edge.identity_file = Some("/keys/edge key".into());
        let mut bastion = host("bastion", "admin", "198.51.100.5", 2222);
        bastion.identity_file = Some("/keys/bastion".into());
        bastion.jump = Some("edge".into());
        let mut web = host("web-1", "deploy", "10.0.0.8", 22);
        web.jump = Some("bastion".into());
        let args = build_ssh_command(
            &web,
            &[edge, bastion, web.clone()],
            &SshCommandOptions::default(),
        );
        let proxy = args
            .windows(2)
            .find(|pair| pair[0] == "-o" && pair[1].starts_with("ProxyCommand="))
            .unwrap()[1]
            .clone();
        assert!(
            proxy.contains("'/keys/edge key'") || proxy.contains("'/keys/edge key'"),
            "{proxy}"
        );
        assert!(proxy.contains("/keys/edge key"), "{proxy}");
        assert!(proxy.contains("198.51.100.5:2222"), "{proxy}");
        assert!(proxy.contains("%h:%p"), "{proxy}");
        assert!(!args.iter().any(|arg| arg == "-J"));
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

    #[test]
    fn saved_password_drops_batch_mode_and_limits_prompts() {
        let web = host("web-1", "deploy", "203.0.113.10", 22);
        let args = exec_arguments_auth(
            &web,
            std::slice::from_ref(&web),
            Some(15),
            false,
            None,
            "true",
            true,
        );
        assert!(!args.iter().any(|arg| arg == "BatchMode=yes"));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "NumberOfPasswordPrompts=1"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "KbdInteractiveAuthentication=no"])
        );
        assert!(!args.iter().any(|arg| arg.contains("s3cret")));
    }

    #[test]
    fn multiplexing_creates_the_control_directory() {
        let dir = tempfile::tempdir().unwrap();
        let control = dir.path().join("cm");
        assert!(!control.exists());
        let web = host("web-1", "deploy", "203.0.113.10", 22);
        let args = build_ssh_command(
            &web,
            std::slice::from_ref(&web),
            &SshCommandOptions {
                batch_mode: true,
                password_auth: false,
                connect_timeout: None,
                multiplexing: true,
                control_path_dir: Some(control.display().to_string()),
            },
        );
        assert!(control.is_dir(), "control directory should be created");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&control).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        assert!(args.windows(2).any(|pair| pair[0] == "-o"
            && pair[1] == format!("ControlPath={}/cm/%C", dir.path().display())));
        // A second build must tolerate the directory already existing.
        let again = build_ssh_command(
            &web,
            std::slice::from_ref(&web),
            &SshCommandOptions {
                batch_mode: true,
                password_auth: false,
                connect_timeout: None,
                multiplexing: true,
                control_path_dir: Some(control.display().to_string()),
            },
        );
        assert!(again.iter().any(|arg| arg.contains("ControlPath=")));
    }

    #[test]
    fn without_multiplexing_does_not_create_a_control_directory() {
        let dir = tempfile::tempdir().unwrap();
        let control = dir.path().join("cm");
        let web = host("web-1", "deploy", "203.0.113.10", 22);
        let args = build_ssh_command(
            &web,
            std::slice::from_ref(&web),
            &SshCommandOptions {
                batch_mode: true,
                password_auth: false,
                connect_timeout: None,
                multiplexing: false,
                control_path_dir: Some(control.display().to_string()),
            },
        );
        assert!(!control.exists());
        assert!(!args.iter().any(|arg| arg.contains("ControlPath")));
    }

    #[test]
    fn multiplexing_sends_keepalives_so_a_dead_link_ends() {
        let web = host("web-1", "deploy", "203.0.113.10", 22);
        let args = build_ssh_command(
            &web,
            std::slice::from_ref(&web),
            &SshCommandOptions {
                batch_mode: false,
                password_auth: false,
                connect_timeout: None,
                multiplexing: true,
                control_path_dir: Some("/tmp/vcon/cm".into()),
            },
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "ServerAliveInterval=15"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "ServerAliveCountMax=3"])
        );
    }
}
