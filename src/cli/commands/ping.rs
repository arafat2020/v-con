//! `vcon ping`: measure ssh latency with a remote `true`.

use crate::config::Config;
use crate::domain::selector::{evaluate_selector, parse_selector};
use crate::error::Error;
use crate::infra::ssh::command::exec_arguments;
use crate::infra::ssh::system::SystemSsh;
use crate::paths::Paths;
use crate::services::exec::{self as job, EndStatus, HostTarget};
use crate::services::inventory::InventoryBundle;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Options for one `vcon ping` invocation.
pub struct PingRequest<'a> {
    /// Optional selector. Absent means every host.
    pub selector: Option<&'a str>,
    /// Max concurrent pings.
    pub parallel: Option<usize>,
    /// Per-host timeout in seconds.
    pub timeout: Option<u64>,
}

/// Connect with `true` and print round-trip time per host.
pub fn run(
    bundle: &InventoryBundle,
    config: &Config,
    paths: &Paths,
    request: &PingRequest<'_>,
) -> Result<ExitCode, Error> {
    let hosts = match request.selector {
        Some(selector) => {
            let terms = parse_selector(selector).map_err(Error::Message)?;
            let selection = evaluate_selector(&terms, &bundle.hosts);
            if let Some(error) = &selection.error {
                return Err(Error::Message(error.clone()));
            }
            for warning in &selection.warnings {
                eprintln!("{warning}");
            }
            selection
                .hosts
                .iter()
                .map(|host| (*host).clone())
                .collect::<Vec<_>>()
        }
        None => bundle.hosts.clone(),
    };
    if hosts.is_empty() {
        return Err(Error::Message("no hosts matched".to_string()));
    }
    let program = crate::infra::ssh::system::find_ssh()?;
    let timeout = Duration::from_secs(request.timeout.unwrap_or(10).max(1));
    let parallel = request.parallel.unwrap_or(config.parallel).max(1);
    let control = paths.control_dir().display().to_string();
    let targets: Vec<HostTarget> = hosts
        .iter()
        .map(|host| HostTarget {
            name: host.name.clone(),
            args: exec_arguments(
                host,
                &bundle.hosts,
                Some(timeout.as_secs()),
                config.multiplexing,
                Some(&control),
                "true",
            ),
        })
        .collect();

    let runner = Arc::new(SystemSsh::new(program));
    let (tx, mut rx) = mpsc::channel(128);
    let started = Instant::now();
    let summary = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Message(error.to_string()))?
        .block_on(async {
            let cancel = CancellationToken::new();
            let join = tokio::spawn(job::run_job(runner, targets, timeout, parallel, cancel, tx));
            while let Some(event) = rx.recv().await {
                if let job::JobEvent::HostFinished { host, outcome } = event {
                    match outcome.status {
                        EndStatus::Ok => {
                            println!(
                                "{:<16} ok    {:>7.1} ms",
                                host,
                                outcome.duration.as_secs_f64() * 1000.0
                            );
                        }
                        _ => {
                            println!(
                                "{:<16} fail  {} ({:.1} ms)",
                                host,
                                outcome.reason,
                                outcome.duration.as_secs_f64() * 1000.0
                            );
                        }
                    }
                }
            }
            join.await
                .map_err(|error| Error::Message(error.to_string()))
        })?;

    eprintln!(
        "{} hosts in {:.1}s: {} ok, {} failed",
        hosts.len(),
        started.elapsed().as_secs_f64(),
        summary.ok,
        summary.failed + summary.skipped
    );
    Ok(if summary.failed + summary.skipped > 0 {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    })
}
