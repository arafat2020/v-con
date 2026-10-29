//! Background SSH port forwards.
//!
//! [`TunnelManager`] spawns `ssh -N` with forward flags and
//! `ExitOnForwardFailure=yes`, tracks children, and emits [`TunnelEvent`]s when
//! they exit. Local and dynamic forwards refuse to start when the listen port
//! is already bound on `127.0.0.1`.

use crate::domain::host::EffectiveHost;
use crate::domain::tunnel::TunnelSpec;
use crate::error::Error;
use crate::infra::ssh::command::{SshCommandOptions, build_ssh_command, command_line};
use crate::services::connect;
use std::collections::HashMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tokio::sync::mpsc;

/// Opaque id for one running tunnel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TunnelId(u64);

impl std::fmt::Display for TunnelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Notifications from waiter threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelEvent {
    /// A tracked child exited.
    Exited {
        /// Tunnel that ended.
        id: TunnelId,
        /// Inventory host.
        host: String,
        /// Display name (saved tunnel name or compact spec).
        label: String,
        /// Process exit code when known.
        code: Option<i32>,
    },
}

/// One live forward.
#[derive(Debug, Clone)]
pub struct RunningTunnel {
    /// Manager id.
    pub id: TunnelId,
    /// Inventory host name.
    pub host: String,
    /// Saved name or compact spec.
    pub label: String,
    /// Parsed forward.
    #[allow(dead_code)]
    pub spec: TunnelSpec,
}

struct Live {
    info: RunningTunnel,
    child: Arc<Mutex<Option<Child>>>,
    waiter: Option<JoinHandle<()>>,
}

/// Owns ssh forward children for the TUI event loop.
pub struct TunnelManager {
    next_id: AtomicU64,
    program: PathBuf,
    events: mpsc::UnboundedSender<TunnelEvent>,
    live: Mutex<HashMap<TunnelId, Live>>,
}

impl TunnelManager {
    /// Create a manager that pushes exit events on `events`.
    pub fn new(program: PathBuf, events: mpsc::UnboundedSender<TunnelEvent>) -> Self {
        Self {
            next_id: AtomicU64::new(1),
            program,
            events,
            live: Mutex::new(HashMap::new()),
        }
    }

    /// Currently tracked tunnels.
    pub fn list(&self) -> Vec<RunningTunnel> {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .map(|live| live.info.clone())
            .collect()
    }

    /// Start a forward. Checks the local port for local/dynamic specs.
    pub fn start(
        &self,
        host: &EffectiveHost,
        hosts: &[EffectiveHost],
        label: impl Into<String>,
        spec: TunnelSpec,
        multiplexing: bool,
        control_dir: &Path,
    ) -> Result<TunnelId, Error> {
        if let Some(port) = spec.local_listen_port() {
            ensure_local_port_free(port)?;
        }
        let options = connect::session_options(multiplexing, control_dir);
        let args = plan_forward_args(host, hosts, &options, &spec);
        let label = label.into();
        let mut displayed = vec![self.program.display().to_string()];
        displayed.extend(args.iter().cloned());
        tracing::debug!(command = %command_line(&displayed), "starting tunnel");

        let child = Command::new(&self.program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| Error::Message(format!("failed to start tunnel: {error}")))?;

        let id = TunnelId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let info = RunningTunnel {
            id,
            host: host.name.clone(),
            label: label.clone(),
            spec,
        };
        let shared = Arc::new(Mutex::new(Some(child)));
        let events = self.events.clone();
        let host_name = host.name.clone();
        let waiter_label = label;
        let wait_shared = Arc::clone(&shared);
        let waiter = std::thread::Builder::new()
            .name(format!("vcon-tunnel-{id}"))
            .spawn(move || {
                let code = loop {
                    let status = {
                        let mut guard = wait_shared
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        match guard.as_mut() {
                            Some(child) => match child.try_wait() {
                                Ok(Some(status)) => {
                                    let _ = guard.take();
                                    Some(status.code())
                                }
                                Ok(None) => None,
                                Err(_) => {
                                    let _ = guard.take();
                                    Some(None)
                                }
                            },
                            None => Some(None),
                        }
                    };
                    if let Some(code) = status {
                        break code;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                };
                let _ = events.send(TunnelEvent::Exited {
                    id,
                    host: host_name,
                    label: waiter_label,
                    code,
                });
            })
            .map_err(|error| Error::Message(format!("tunnel waiter: {error}")))?;

        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                id,
                Live {
                    info,
                    child: shared,
                    waiter: Some(waiter),
                },
            );
        Ok(id)
    }

    /// Kill one tunnel and drop it from the table.
    pub fn stop(&self, id: TunnelId) -> Result<(), Error> {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(mut entry) = live.remove(&id) {
            if let Some(mut child) = entry
                .child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                let _ = child.kill();
                let _ = child.wait();
            }
            // Waiter observes the taken child and exits.
            if let Some(waiter) = entry.waiter.take() {
                let _ = waiter.join();
            }
        }
        Ok(())
    }

    /// Kill every tunnel (quit / Drop).
    pub fn stop_all(&self) {
        let ids: Vec<TunnelId> = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .copied()
            .collect();
        for id in ids {
            let _ = self.stop(id);
        }
    }

    /// Drop a finished tunnel after [`TunnelEvent::Exited`].
    pub fn forget(&self, id: TunnelId) {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(mut entry) = live.remove(&id)
            && let Some(waiter) = entry.waiter.take()
        {
            let _ = waiter.join();
        }
    }
}

impl Drop for TunnelManager {
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// Build ssh argv (without the binary) for a non-interactive forward.
pub fn plan_forward_args(
    host: &EffectiveHost,
    hosts: &[EffectiveHost],
    options: &SshCommandOptions,
    spec: &TunnelSpec,
) -> Vec<String> {
    let mut argv = build_ssh_command(host, hosts, options);
    let mut args: Vec<String> = argv.drain(1..).collect();
    let destination = args.pop().expect("ssh argv includes destination");
    args.push("-N".into());
    args.push("-o".into());
    args.push("ExitOnForwardFailure=yes".into());
    let flag = spec.to_ssh_flag();
    let mut parts = flag.split_whitespace();
    if let Some(flag_name) = parts.next() {
        args.push(flag_name.to_string());
        if let Some(value) = parts.next() {
            args.push(value.to_string());
        }
    }
    args.push(destination);
    args
}

/// Resolve a saved tunnel name or a raw spec for `host`.
pub fn resolve_spec(
    host: &EffectiveHost,
    name_or_spec: &str,
) -> Result<(String, TunnelSpec), Error> {
    if let Some(saved) = host
        .tunnels
        .iter()
        .find(|tunnel| tunnel.name == name_or_spec)
    {
        let spec = TunnelSpec::parse(&saved.spec).map_err(|error| Error::Message(error.message))?;
        return Ok((saved.name.clone(), spec));
    }
    let spec = TunnelSpec::parse(name_or_spec).map_err(|error| Error::Message(error.message))?;
    Ok((spec.to_compact(), spec))
}

/// Error when a local listen port is already in use.
pub fn ensure_local_port_free(port: u16) -> Result<(), Error> {
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(_) => Err(Error::Message(format!(
            "local port {port} is already in use"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::host::EffectiveHost;
    use std::collections::BTreeMap;

    fn host() -> EffectiveHost {
        EffectiveHost {
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
        }
    }

    #[test]
    fn plan_includes_n_and_forward_failure() {
        let host = host();
        let options = SshCommandOptions {
            batch_mode: false,
            connect_timeout: None,
            multiplexing: false,
            control_path_dir: None,
        };
        let spec = TunnelSpec::parse("L:3000:localhost:3000").unwrap();
        let args = plan_forward_args(&host, std::slice::from_ref(&host), &options, &spec);
        assert!(args.iter().any(|arg| arg == "-N"));
        assert!(args.iter().any(|arg| arg == "ExitOnForwardFailure=yes"));
        assert!(args.iter().any(|arg| arg == "-L"));
        assert!(args.iter().any(|arg| arg == "3000:localhost:3000"));
        assert_eq!(args.last().map(String::as_str), Some("root@10.0.0.1"));
    }

    #[test]
    fn busy_port_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let err = ensure_local_port_free(port).unwrap_err().to_string();
        assert!(err.contains("already in use"));
    }
}
