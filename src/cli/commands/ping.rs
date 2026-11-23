//! `vcon ping`: measure ssh latency with a remote `true`.

use crate::config::Config;
use crate::domain::selector::{evaluate_selector, parse_selector};
use crate::error::Error;
use crate::infra::secrets::{self, AskpassMode};
use crate::infra::ssh::command::SshCommandOptions;
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
    /// Emit one JSON object instead of the table.
    pub json: bool,
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
    let store = secrets::system_store();
    let options = SshCommandOptions {
        batch_mode: true,
        password_auth: false,
        connect_timeout: Some(timeout.as_secs()),
        multiplexing: config.multiplexing,
        control_path_dir: Some(control),
    };
    let targets: Vec<HostTarget> = hosts
        .iter()
        .map(|host| {
            job::authenticated_target(
                store.as_ref(),
                &paths.cache_dir,
                host,
                &bundle.hosts,
                "true",
                AskpassMode::Force,
                &options,
            )
        })
        .collect();

    let runner = Arc::new(SystemSsh::new(program));
    let (tx, mut rx) = mpsc::channel(128);
    let started = Instant::now();
    let json = request.json;
    let mut rows: Vec<PingRow> = Vec::new();
    let summary = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Message(error.to_string()))?
        .block_on(async {
            let cancel = CancellationToken::new();
            let join = tokio::spawn(job::run_job(runner, targets, timeout, parallel, cancel, tx));
            while let Some(event) = rx.recv().await {
                if let job::JobEvent::HostFinished { host, outcome } = event {
                    let row = PingRow::from_outcome(&host, &outcome);
                    if !json {
                        println!("{}", row.text());
                    }
                    rows.push(row);
                }
            }
            join.await
                .map_err(|error| Error::Message(error.to_string()))
        })?;

    if json {
        let payload = serde_json::json!({
            "ok": summary.ok,
            "failed": summary.failed + summary.skipped,
            "hosts": rows,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|error| Error::Message(error.to_string()))?
        );
    } else {
        eprintln!(
            "{} hosts in {:.1}s: {} ok, {} failed",
            hosts.len(),
            started.elapsed().as_secs_f64(),
            summary.ok,
            summary.failed + summary.skipped
        );
    }
    Ok(if summary.failed + summary.skipped > 0 {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    })
}

#[derive(Debug, serde::Serialize)]
struct PingRow {
    host: String,
    ok: bool,
    ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl PingRow {
    fn from_outcome(host: &str, outcome: &job::HostOutcome) -> Self {
        let ok = outcome.status == EndStatus::Ok;
        Self {
            host: host.to_string(),
            ok,
            ms: outcome.duration.as_secs_f64() * 1000.0,
            reason: if ok {
                None
            } else {
                Some(outcome.reason.clone())
            },
        }
    }

    fn text(&self) -> String {
        if self.ok {
            format!("{:<16} ok    {:>7.1} ms", self.host, self.ms)
        } else {
            format!(
                "{:<16} fail  {} ({:.1} ms)",
                self.host,
                self.reason.as_deref().unwrap_or("failed"),
                self.ms
            )
        }
    }
}
