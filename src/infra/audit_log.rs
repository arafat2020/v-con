//! Append-only JSONL audit log with size-based rotation and optional redaction.
//!
//! Entries never include command output — only the command string, hosts, and
//! per-host exit codes.

use crate::infra::fsutil::{self, FILE_MODE};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// One recorded action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// When the action finished (or started, for tunnels/sessions).
    pub ts: DateTime<Utc>,
    /// Action kind: `exec`, `snippet`, `transfer`, `delete_remote`, `tunnel_start`, `session_open`.
    pub action: String,
    /// Selector or host expression when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// Hosts involved.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Command or transfer path summary (may be redacted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Per-host exit codes when the action produced them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<BTreeMap<String, i32>>,
    /// Wall-clock duration in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Default rotation threshold: 10 MiB.
#[cfg(test)]
pub const DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Append `entry` to `path`, rotating to `audit.jsonl.1` when over `max_bytes`.
///
/// `redact` patterns are applied to [`AuditEntry::command`] before writing.
/// Failures are returned to the caller; the TUI treats them as non-fatal.
pub fn append(
    path: &Path,
    mut entry: AuditEntry,
    max_bytes: u64,
    redact: &[String],
) -> Result<(), String> {
    if let Some(command) = entry.command.as_mut() {
        *command = redact_command(command, redact);
    }
    if let Some(parent) = path.parent() {
        fsutil::create_dir_secure(parent).map_err(|error| error.to_string())?;
    }
    rotate_if_needed(path, max_bytes)?;
    let line = serde_json::to_string(&entry).map_err(|error| error.to_string())?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    writeln!(file, "{line}").map_err(|error| error.to_string())?;
    let _ = fsutil::set_mode(path, FILE_MODE);
    Ok(())
}

/// Read up to `limit` most recent entries (newest last in the returned vec).
pub fn read_recent(path: &Path, limit: usize) -> Result<Vec<AuditEntry>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let mut entries = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<AuditEntry>(line) {
            Ok(entry) => entries.push(entry),
            Err(error) => tracing::warn!("skipping bad audit line: {error}"),
        }
    }
    if entries.len() > limit {
        let skip = entries.len() - limit;
        entries = entries.into_iter().skip(skip).collect();
    }
    Ok(entries)
}

/// Apply each regex pattern, replacing matches with `***`.
pub fn redact_command(command: &str, patterns: &[String]) -> String {
    if patterns.is_empty() {
        return command.to_string();
    }
    let mut out = command.to_string();
    for pattern in patterns {
        match compile_regex(pattern) {
            Ok(re) => out = re.replace_all(&out, "***").into_owned(),
            Err(error) => tracing::warn!("invalid audit redact pattern `{pattern}`: {error}"),
        }
    }
    out
}

fn rotate_if_needed(path: &Path, max_bytes: u64) -> Result<(), String> {
    if max_bytes == 0 || !path.exists() {
        return Ok(());
    }
    let meta = fs::metadata(path).map_err(|error| error.to_string())?;
    if meta.len() < max_bytes {
        return Ok(());
    }
    let rotated = rotated_path(path);
    let _ = fs::remove_file(&rotated);
    fs::rename(path, &rotated).map_err(|error| error.to_string())?;
    Ok(())
}

fn rotated_path(path: &Path) -> PathBuf {
    let mut rotated = path.as_os_str().to_os_string();
    rotated.push(".1");
    PathBuf::from(rotated)
}

fn compile_regex(pattern: &str) -> Result<Regex, String> {
    static CACHE: OnceLock<Mutex<BTreeMap<String, Regex>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(re) = guard.get(pattern) {
        return Ok(re.clone());
    }
    let compiled = Regex::new(pattern).map_err(|error| error.to_string())?;
    guard.insert(pattern.to_string(), compiled.clone());
    Ok(compiled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn append_and_read_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let entry = AuditEntry {
            ts: Utc::now(),
            action: "exec".into(),
            selector: Some("@web".into()),
            hosts: vec!["web-1".into()],
            command: Some("uptime".into()),
            results: Some(BTreeMap::from([("web-1".into(), 0)])),
            duration_ms: Some(12),
        };
        append(&path, entry.clone(), DEFAULT_MAX_BYTES, &[]).unwrap();
        let loaded = read_recent(&path, 10).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].action, "exec");
        assert_eq!(loaded[0].command.as_deref(), Some("uptime"));
    }

    #[test]
    fn redacts_command_before_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let entry = AuditEntry {
            ts: Utc::now(),
            action: "exec".into(),
            selector: None,
            hosts: vec!["db".into()],
            command: Some("mysql -psecret password=hunter2".into()),
            results: None,
            duration_ms: None,
        };
        append(
            &path,
            entry,
            DEFAULT_MAX_BYTES,
            &["-p\\S+".into(), "password=\\S+".into()],
        )
        .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("secret"));
        assert!(!text.contains("hunter2"));
        assert!(text.contains("***"));
    }

    #[test]
    fn rotates_when_over_limit() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        fs::write(&path, "x".repeat(64)).unwrap();
        let entry = AuditEntry {
            ts: Utc::now(),
            action: "session_open".into(),
            selector: None,
            hosts: vec!["web-1".into()],
            command: None,
            results: None,
            duration_ms: None,
        };
        append(&path, entry, 32, &[]).unwrap();
        assert!(
            path.with_extension("jsonl.1").exists() || {
                // rotated_path appends ".1" to the full name
                PathBuf::from(format!("{}.1", path.display())).exists()
            }
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("session_open"));
    }
}
