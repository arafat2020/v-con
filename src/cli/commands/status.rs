//! `vcon status`: one-shot health table.

use crate::config::Config;
use crate::domain::health::{HealthReport, HealthStatus, format_rate};
use crate::domain::selector::{evaluate_selector, parse_selector};
use crate::error::Error;
use crate::infra::ssh::system::SystemSsh;
use crate::paths::Paths;
use crate::services::health::{self, HealthSettings};
use crate::services::inventory::InventoryBundle;
use serde::Serialize;
use std::process::ExitCode;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Options for one `vcon status` invocation.
pub struct StatusRequest<'a> {
    /// Optional selector. Absent means every host.
    pub selector: Option<&'a str>,
    /// Sort key: host, status, cpu, load, mem, disk, net, up.
    pub sort: &'a str,
    /// Emit JSON instead of a table.
    pub json: bool,
    /// Take a second sample one second later for CPU % and net rates.
    pub sample_cpu: bool,
}

/// Run a single health poll and print the results.
pub fn run(
    bundle: &InventoryBundle,
    config: &Config,
    paths: &Paths,
    request: &StatusRequest<'_>,
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
            selection.hosts.iter().map(|host| (*host).clone()).collect()
        }
        None => bundle.hosts.clone(),
    };
    if hosts.is_empty() {
        return Err(Error::Message("no hosts matched".to_string()));
    }
    let program = crate::infra::ssh::system::find_ssh()?;
    let settings =
        HealthSettings::from_config(&config.health, config.parallel.max(1), program.clone());
    let targets = health::health_targets(
        &bundle.hosts,
        Some(
            &hosts
                .iter()
                .map(|host| host.name.clone())
                .collect::<Vec<_>>(),
        ),
        config.multiplexing,
        &paths.control_dir(),
    );
    let runner = Arc::new(SystemSsh::new(program));
    let reports = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Message(error.to_string()))?
        .block_on(health::poll_once(
            runner,
            targets,
            &settings,
            request.sample_cpu,
            CancellationToken::new(),
        ));

    let mut rows: Vec<(&str, &HealthReport)> = hosts
        .iter()
        .filter_map(|host| {
            reports
                .get(&host.name)
                .map(|report| (host.name.as_str(), report))
        })
        .collect();
    sort_rows(&mut rows, request.sort);

    if request.json {
        let payload: Vec<JsonRow> = rows
            .iter()
            .map(|(name, report)| JsonRow {
                host: (*name).to_string(),
                report: (*report).clone(),
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|error| Error::Message(error.to_string()))?
        );
    } else {
        println!(
            "{:<16} {:<3} {:>5} {:>6} {:>5} {:>5} {:>11} {:>5}",
            "HOST", "ST", "CPU", "LOAD", "MEM", "DISK", "NET ↓/↑", "UP"
        );
        for (name, report) in rows {
            println!("{}", format_row(name, report));
        }
    }

    let failed = reports
        .values()
        .any(|report| report.status == HealthStatus::Unreachable);
    Ok(if failed {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    })
}

#[derive(Serialize)]
struct JsonRow {
    host: String,
    #[serde(flatten)]
    report: HealthReport,
}

fn format_row(name: &str, report: &HealthReport) -> String {
    if !report.reachable {
        return format!(
            "{:<16} {:<3} {}",
            name,
            report.status.symbol(),
            report.error.as_deref().unwrap_or("unreachable")
        );
    }
    let net = match (report.net_rx_bps, report.net_tx_bps) {
        (Some(rx), Some(tx)) => format!("{}/{}", format_rate(rx), format_rate(tx)),
        _ => "—".to_string(),
    };
    format!(
        "{:<16} {:<3} {:>5} {:>6} {:>5} {:>5} {:>11} {:>5}",
        name,
        report.status.symbol(),
        pct(report.cpu_pct),
        load(report.load1),
        pct(report.mem_pct),
        pct(report.disk_pct),
        net,
        report.uptime.as_deref().unwrap_or("—"),
    )
}

fn pct(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:.0}%", value))
        .unwrap_or_else(|| "—".to_string())
}

fn load(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.2}"))
        .unwrap_or_else(|| "—".to_string())
}

fn sort_rows(rows: &mut [(&str, &HealthReport)], sort: &str) {
    rows.sort_by(|(left_name, left), (right_name, right)| {
        let order = match sort {
            "status" => status_rank(left).cmp(&status_rank(right)),
            "cpu" => cmp_opt(left.cpu_pct, right.cpu_pct),
            "load" => cmp_opt(left.load1, right.load1),
            "mem" => cmp_opt(left.mem_pct, right.mem_pct),
            "disk" => cmp_opt(left.disk_pct, right.disk_pct),
            "net" => cmp_opt(left.net_rx_bps, right.net_rx_bps),
            "up" => cmp_opt(left.uptime_secs, right.uptime_secs),
            _ => left_name.cmp(right_name),
        };
        order.then_with(|| left_name.cmp(right_name))
    });
}

fn status_rank(report: &HealthReport) -> u8 {
    match report.status {
        HealthStatus::Crit => 0,
        HealthStatus::Unreachable => 1,
        HealthStatus::Warn => 2,
        HealthStatus::Ok => 3,
    }
}

fn cmp_opt(left: Option<f64>, right: Option<f64>) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left
            .partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::health::unreachable_report;

    #[test]
    fn formats_unreachable_rows() {
        let report = unreachable_report(1.0, "connection timed out");
        let line = format_row("web-1", &report);
        assert!(line.contains("web-1"));
        assert!(line.contains("timed out"));
        assert!(line.contains("✖"));
    }
}
