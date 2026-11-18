//! `vcon tunnel <host> <spec or saved name>` — foreground forward until Ctrl+C.

use crate::config::Config;
use crate::domain::host::EffectiveHost;
use crate::domain::tunnel::TunnelSpec;
use crate::error::Error;
use crate::infra::ssh::command::command_line;
use crate::infra::ssh::system;
use crate::paths::Paths;
use crate::services::connect;
use crate::services::inventory::InventoryBundle;
use crate::services::tunnels::{ensure_local_port_free, plan_forward_args, resolve_spec};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};

/// Start a foreground tunnel. Blocks until the child exits or the process is interrupted.
pub fn run(
    bundle: &InventoryBundle,
    config: &Config,
    paths: &Paths,
    host_name: &str,
    spec_or_name: &str,
) -> Result<(), Error> {
    let host = connect::resolve_host(host_name, &bundle.hosts).map_err(Error::Message)?;
    let (label, spec) = resolve_spec(host, spec_or_name)?;
    if let Some(port) = spec.local_listen_port() {
        ensure_local_port_free(port)?;
    }
    let program = system::find_ssh()?;
    let args = forward_args(host, &bundle.hosts, &spec);
    let mut displayed = vec![program.display().to_string()];
    displayed.extend(args.iter().cloned());
    tracing::debug!(command = %command_line(&displayed), "tunnel");

    let mut entry = crate::services::audit::entry(
        "tunnel_start",
        vec![host.name.clone()],
        Some(format!("{label} ({})", spec.to_compact())),
    );
    entry.selector = Some(host.name.clone());
    if let Err(error) = crate::services::audit::append(&paths.audit_file(), entry, &config.audit) {
        eprintln!("warning: audit log: {error}");
    }

    println!("Tunnel {label} on {} — Ctrl+C to stop", host.name);
    let _ = io::stdout().flush();

    let mut child = ForwardChild::spawn(&program, &args)?;
    let status = wait_for_forward(&mut child.0)?;

    if status.success() || status.code().is_none() {
        // Signal (Ctrl+C) ends with no code — treat as a clean stop.
        Ok(())
    } else {
        Err(Error::Message(format!(
            "tunnel exited with {}",
            status.code().unwrap_or(-1)
        )))
    }
}

/// Argv for a tunnel that owns its own listener.
///
/// `ControlMaster=auto` plus `ControlPersist` parks the forward on a
/// background master, so the local port stays open after vcon exits.
/// `ControlMaster=no` and `ControlPath=none` come first: OpenSSH keeps the
/// first value, ahead of host `ssh_options` and the user's ssh config.
pub(crate) fn forward_args(
    host: &EffectiveHost,
    hosts: &[EffectiveHost],
    spec: &TunnelSpec,
) -> Vec<String> {
    let options = connect::session_options(false, Path::new(""));
    let mut args = plan_forward_args(host, hosts, &options, spec);
    let mut isolated = vec![
        "-o".to_string(),
        "ControlMaster=no".to_string(),
        "-o".to_string(),
        "ControlPath=none".to_string(),
    ];
    // Keepalives are added for multiplexed sessions only. Tunnels turn
    // multiplexing off, so install the same defaults here unless the host
    // record already set an interval (OpenSSH keeps the first value).
    if !host
        .ssh_options
        .keys()
        .any(|key| key.eq_ignore_ascii_case("ServerAliveInterval"))
    {
        isolated.push("-o".to_string());
        isolated.push("ServerAliveInterval=15".to_string());
        isolated.push("-o".to_string());
        isolated.push("ServerAliveCountMax=3".to_string());
    }
    isolated.append(&mut args);
    isolated
}

/// ssh child. Drop kills and reaps it so a return path cannot leave the port open.
struct ForwardChild(Child);

impl ForwardChild {
    fn spawn(program: &Path, args: &[String]) -> Result<Self, Error> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        #[cfg(unix)]
        isolate_tunnel_child(&mut command);
        let child = command
            .spawn()
            .map_err(|error| Error::Message(format!("failed to start tunnel: {error}")))?;
        Ok(Self(child))
    }
}

/// Keep the ssh child from surviving `kill -9` of vcon.
///
/// On Linux the child asks to be sent SIGTERM when its parent dies, and it
/// runs in its own process group.
#[cfg(unix)]
fn isolate_tunnel_child(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    #[cfg(target_os = "linux")]
    {
        let parent = std::process::id();
        // SAFETY: `pre_exec` runs in the forked child before exec. The armed
        // path calls `prctl`, `getppid`, and, if the parent is already gone,
        // `raise` and `_exit`.
        unsafe {
            command.pre_exec(move || arm_parent_death_signal(parent));
        }
    }
    command.process_group(0);
}

/// `PR_SET_PDEATHSIG(SIGTERM)`, then exit if the parent is already gone.
///
/// The signal is not delivered for a parent that died before `prctl`, so
/// compare `getppid` with the pid captured before the spawn.
#[cfg(target_os = "linux")]
fn arm_parent_death_signal(parent: u32) -> io::Result<()> {
    let rc = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::getppid() } != parent as libc::pid_t {
        unsafe {
            libc::raise(libc::SIGTERM);
            libc::_exit(1);
        }
    }
    Ok(())
}

impl Drop for ForwardChild {
    fn drop(&mut self) {
        let _ = terminate_child(&mut self.0);
    }
}

/// Kill `child` if it is still running, then wait so it is not left as a zombie.
pub(crate) fn terminate_child(child: &mut Child) -> io::Result<ExitStatus> {
    if let Some(status) = child.try_wait()? {
        return Ok(status);
    }
    let _ = child.kill();
    child.wait()
}

/// Wait until ssh exits. SIGINT and SIGTERM kill the child and reap it.
fn wait_for_forward(child: &mut Child) -> Result<ExitStatus, Error> {
    #[cfg(unix)]
    {
        wait_for_forward_unix(child)
    }
    #[cfg(not(unix))]
    {
        child
            .wait()
            .map_err(|error| Error::Message(format!("tunnel: {error}")))
    }
}

#[cfg(unix)]
fn wait_for_forward_unix(child: &mut Child) -> Result<ExitStatus, Error> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Message(format!("tunnel runtime: {error}")))?;
    runtime.block_on(async {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigint = signal(SignalKind::interrupt())
            .map_err(|error| Error::Message(format!("could not watch SIGINT: {error}")))?;
        let mut sigterm = signal(SignalKind::terminate())
            .map_err(|error| Error::Message(format!("could not watch SIGTERM: {error}")))?;
        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|error| Error::Message(format!("tunnel: {error}")))?
            {
                return Ok(status);
            }
            tokio::select! {
                _ = sigint.recv() => {
                    return terminate_child(child)
                        .map_err(|error| Error::Message(format!("tunnel: {error}")));
                }
                _ = sigterm.recv() => {
                    return terminate_child(child)
                        .map_err(|error| Error::Message(format!("tunnel: {error}")));
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn host() -> EffectiveHost {
        let mut host = EffectiveHost {
            name: "web-1".into(),
            address: "10.0.0.1".into(),
            user: "root".into(),
            port: 22,
            identity_file: None,
            jump: None,
            tags: Vec::new(),
            notes: None,
            ssh_options: BTreeMap::new(),
            disabled: false,
            connect_timeout: None,
            tunnels: Vec::new(),
        };
        host.ssh_options
            .insert("ControlMaster".into(), "auto".into());
        host.ssh_options
            .insert("ControlPath".into(), "/tmp/user-cm/%C".into());
        host.ssh_options
            .insert("ControlPersist".into(), "10m".into());
        host
    }

    #[test]
    fn tunnel_args_disable_multiplexing_before_host_options() {
        let host = host();
        let spec = TunnelSpec::parse("L:3000:localhost:3000").unwrap();
        let args = forward_args(&host, std::slice::from_ref(&host), &spec);
        let master_no = args
            .iter()
            .position(|arg| arg == "ControlMaster=no")
            .expect("ControlMaster=no");
        let path_none = args
            .iter()
            .position(|arg| arg == "ControlPath=none")
            .expect("ControlPath=none");
        assert!(args[..master_no].contains(&"-o".to_string()));
        assert!(master_no < path_none);
        for later in [
            "ControlMaster=auto",
            "ControlPath=/tmp/user-cm/%C",
            "ControlPersist=10m",
        ] {
            let position = args.iter().position(|arg| arg == later).unwrap();
            assert!(
                master_no < position && path_none < position,
                "{later} must not override the tunnel isolation flags"
            );
        }
        assert!(args.iter().any(|arg| arg == "-N"));
        assert!(args.iter().any(|arg| arg == "-L"));
        assert_eq!(args.last().map(String::as_str), Some("root@10.0.0.1"));
    }

    #[test]
    fn tunnel_args_send_keepalives_without_multiplexing() {
        let host = host();
        let spec = TunnelSpec::parse("L:3000:localhost:3000").unwrap();
        let args = forward_args(&host, std::slice::from_ref(&host), &spec);
        let path_none = args
            .iter()
            .position(|arg| arg == "ControlPath=none")
            .expect("ControlPath=none");
        let interval = args
            .iter()
            .position(|arg| arg == "ServerAliveInterval=15")
            .expect("ServerAliveInterval=15");
        let count = args
            .iter()
            .position(|arg| arg == "ServerAliveCountMax=3")
            .expect("ServerAliveCountMax=3");
        let host_master = args
            .iter()
            .position(|arg| arg == "ControlMaster=auto")
            .expect("host ControlMaster");
        assert!(path_none < interval);
        assert!(interval < count);
        assert!(count < host_master);
    }

    #[cfg(unix)]
    #[test]
    fn terminate_child_kills_and_reaps() {
        let mut child = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let status = terminate_child(&mut child).unwrap();
        assert!(!status.success());
        assert!(child.try_wait().unwrap().is_some());
        let probe = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!probe.success(), "sleep {pid} should be gone");
    }
}
