//! Thin helpers around [`crate::infra::audit_log`].

use crate::config::AuditConfig;
use crate::infra::audit_log::{self, AuditEntry};
use chrono::Utc;
use std::collections::BTreeMap;
use std::path::Path;

/// Build a basic entry with a UTC timestamp.
pub fn entry(
    action: impl Into<String>,
    hosts: Vec<String>,
    command: Option<String>,
) -> AuditEntry {
    AuditEntry {
        ts: Utc::now(),
        action: action.into(),
        selector: None,
        hosts,
        command,
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

/// Read the newest entries for the Log screen / CLI.
pub fn read_recent(path: &Path, limit: usize) -> Result<Vec<AuditEntry>, String> {
    audit_log::read_recent(path, limit)
}
