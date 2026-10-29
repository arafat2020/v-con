//! `vcon tunnel <host> <spec or saved name>` — foreground forward until Ctrl+C.

use crate::config::Config;
use crate::error::Error;
use crate::infra::ssh::command::command_line;
use crate::infra::ssh::system;
use crate::paths::Paths;
use crate::services::connect;
use crate::services::inventory::InventoryBundle;
use crate::services::tunnels::{ensure_local_port_free, plan_forward_args, resolve_spec};
use std::io::{self, Write};
use std::process::{Command, Stdio};

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
    let options = connect::session_options(config.multiplexing, &paths.control_dir());
    let args = plan_forward_args(host, &bundle.hosts, &options, &spec);
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

    let status = Command::new(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| Error::Message(format!("failed to start tunnel: {error}")))?;

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
