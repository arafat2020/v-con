//! `vcon log` — print the audit log with filters.

use crate::error::Error;
use crate::infra::audit_log::AuditEntry;
use crate::paths::Paths;
use crate::services::audit;
use chrono::{Duration, Utc};
use std::process::ExitCode;

/// Filters for one log listing.
pub struct LogRequest<'a> {
    /// Restrict to this host name.
    pub host: Option<&'a str>,
    /// Restrict to this action name.
    pub action: Option<&'a str>,
    /// Duration like `1d`, `12h`, `30m`.
    pub since: Option<&'a str>,
    /// Only entries with a non-zero result.
    pub failed: bool,
    /// Machine-readable output.
    pub json: bool,
}

/// Print matching audit entries.
pub fn run(paths: &Paths, request: &LogRequest<'_>) -> Result<ExitCode, Error> {
    let entries = audit::read_recent(&paths.audit_file(), 10_000)
        .map_err(Error::Message)?;
    let since = request
        .since
        .map(parse_since)
        .transpose()?
        .map(|duration| Utc::now() - duration);
    let filtered: Vec<&AuditEntry> = entries
        .iter()
        .filter(|entry| {
            if let Some(host) = request.host
                && !entry.hosts.iter().any(|name| name == host)
            {
                return false;
            }
            if let Some(action) = request.action
                && entry.action != action
            {
                return false;
            }
            if let Some(since) = since
                && entry.ts < since
            {
                return false;
            }
            if request.failed {
                let failed = entry
                    .results
                    .as_ref()
                    .is_some_and(|results| results.values().any(|code| *code != 0));
                if !failed {
                    return false;
                }
            }
            true
        })
        .collect();

    if request.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&filtered)
                .map_err(|error| Error::Message(error.to_string()))?
        );
    } else if filtered.is_empty() {
        println!("(no audit entries)");
    } else {
        for entry in &filtered {
            println!("{}", format_line(entry));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn format_line(entry: &AuditEntry) -> String {
    let hosts = entry.hosts.join(",");
    let command = entry.command.as_deref().unwrap_or("-");
    let result = entry
        .results
        .as_ref()
        .map(|results| {
            results
                .iter()
                .map(|(host, code)| format!("{host}={code}"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_else(|| "-".into());
    format!(
        "{}  {:<14}  {:<16}  {}  {}",
        entry.ts.format("%Y-%m-%dT%H:%M:%SZ"),
        entry.action,
        hosts,
        command,
        result
    )
}

/// Parse `1d` / `12h` / `30m` into a chrono duration.
pub fn parse_since(raw: &str) -> Result<Duration, Error> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Error::Message("empty --since value".into()));
    }
    let (number, unit) = raw.split_at(raw.len().saturating_sub(1));
    let amount: i64 = number.parse().map_err(|_| {
        Error::Message(format!(
            "invalid --since `{raw}`; expected a number with suffix d/h/m"
        ))
    })?;
    if amount < 0 {
        return Err(Error::Message("--since must be positive".into()));
    }
    match unit {
        "d" => Ok(Duration::days(amount)),
        "h" => Ok(Duration::hours(amount)),
        "m" => Ok(Duration::minutes(amount)),
        _ => Err(Error::Message(format!(
            "invalid --since `{raw}`; use suffix d, h, or m"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_day_hour_minute() {
        assert_eq!(parse_since("1d").unwrap(), Duration::days(1));
        assert_eq!(parse_since("12h").unwrap(), Duration::hours(12));
        assert_eq!(parse_since("30m").unwrap(), Duration::minutes(30));
    }
}
