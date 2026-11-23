//! Thin helpers around [`crate::infra::audit_log`].

use crate::config::AuditConfig;
use crate::infra::audit_log::{self, AuditEntry};
use chrono::Utc;
use std::collections::BTreeMap;
use std::path::Path;

/// Build a basic entry with a UTC timestamp.
pub fn entry(action: impl Into<String>, hosts: Vec<String>, command: Option<String>) -> AuditEntry {
    AuditEntry {
        ts: Utc::now(),
        action: action.into(),
        selector: None,
        hosts,
        command,
        direction: None,
        src: None,
        dst: None,
        results: None,
        duration_ms: None,
    }
}

/// Append one entry using config redaction and size limits.
///
/// Returns `Err` as a string so callers can toast without depending on [`crate::error::Error`].
pub fn append(path: &Path, entry: AuditEntry, config: &AuditConfig) -> Result<(), String> {
    audit_log::append(path, entry, config.max_bytes, &config.redact)
}

/// Fields for one finished exec/snippet audit line.
pub struct ExecAudit {
    /// Action name (`exec` or `snippet`).
    pub action: String,
    /// Selector when known.
    pub selector: Option<String>,
    /// Hosts involved.
    pub hosts: Vec<String>,
    /// Command text (may already include a snippet label).
    pub command: String,
    /// Per-host exit codes.
    pub results: BTreeMap<String, i32>,
    /// Duration in milliseconds.
    pub duration_ms: u64,
}

/// Convenience: record an exec or snippet run after it finishes.
pub fn record_exec(path: &Path, config: &AuditConfig, audit: ExecAudit) {
    let mut entry = entry(audit.action, audit.hosts, Some(audit.command));
    entry.selector = audit.selector;
    entry.results = Some(audit.results);
    entry.duration_ms = Some(audit.duration_ms);
    if let Err(error) = append(path, entry, config) {
        tracing::warn!("audit log write failed: {error}");
    }
}

/// One finished `vcon push` or `vcon pull` for a single host.
pub struct TransferAudit {
    /// `upload` for push, `download` for pull.
    pub direction: String,
    /// Selector text when the command had one.
    pub selector: Option<String>,
    /// Inventory host.
    pub host: String,
    /// Source path. Local for upload, remote for download.
    pub src: String,
    /// Destination path.
    pub dst: String,
    /// Exit status for this host. `0` is success.
    pub exit_code: i32,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
}

/// Record a push or pull. A log failure does not fail the transfer.
pub fn record_transfer(path: &Path, config: &AuditConfig, audit: TransferAudit) {
    let command = format!("{} {} → {}", audit.direction, audit.src, audit.dst);
    let mut entry = entry("transfer", vec![audit.host.clone()], Some(command));
    entry.selector = audit.selector;
    entry.direction = Some(audit.direction);
    entry.src = Some(audit.src);
    entry.dst = Some(audit.dst);
    entry.results = Some(BTreeMap::from([(audit.host, audit.exit_code)]));
    entry.duration_ms = Some(audit.duration_ms);
    if let Err(error) = append(path, entry, config) {
        tracing::warn!("audit log write failed: {error}");
    }
}

/// Read the newest entries for the Log screen / CLI.
pub fn read_recent(path: &Path, limit: usize) -> Result<Vec<AuditEntry>, String> {
    audit_log::read_recent(path, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_transfer_writes_direction_paths_and_status() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let config = AuditConfig::default();
        record_transfer(
            &path,
            &config,
            TransferAudit {
                direction: "upload".into(),
                selector: Some("@web".into()),
                host: "web-1".into(),
                src: "/tmp/site".into(),
                dst: "/var/www".into(),
                exit_code: 0,
                duration_ms: 15,
            },
        );
        record_transfer(
            &path,
            &config,
            TransferAudit {
                direction: "download".into(),
                selector: Some("web-1".into()),
                host: "web-1".into(),
                src: "/var/log/app.log".into(),
                dst: "./logs".into(),
                exit_code: 1,
                duration_ms: 4,
            },
        );
        let loaded = read_recent(&path, 10).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].action, "transfer");
        assert_eq!(loaded[0].selector.as_deref(), Some("@web"));
        assert_eq!(loaded[0].hosts, vec!["web-1".to_string()]);
        assert_eq!(
            loaded[0].command.as_deref(),
            Some("upload /tmp/site → /var/www")
        );
        assert_eq!(loaded[0].direction.as_deref(), Some("upload"));
        assert_eq!(loaded[0].src.as_deref(), Some("/tmp/site"));
        assert_eq!(loaded[0].dst.as_deref(), Some("/var/www"));
        assert_eq!(loaded[0].summary(), "upload /tmp/site → /var/www");
        assert_eq!(loaded[0].results.as_ref().unwrap().get("web-1"), Some(&0));
        assert_eq!(loaded[0].duration_ms, Some(15));
        assert_eq!(loaded[1].action, "transfer");
        assert_eq!(
            loaded[1].command.as_deref(),
            Some("download /var/log/app.log → ./logs")
        );
        assert_eq!(loaded[1].direction.as_deref(), Some("download"));
        assert_eq!(loaded[1].src.as_deref(), Some("/var/log/app.log"));
        assert_eq!(loaded[1].dst.as_deref(), Some("./logs"));
        assert_eq!(loaded[1].results.as_ref().unwrap().get("web-1"), Some(&1));
    }
}
