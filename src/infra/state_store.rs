//! Recents, favorites, connect counts, and the last screen.
//!
//! The file is `state.json` under the data directory. It has a `version` so a
//! later format can be detected. A missing file, a corrupt file, or an unknown
//! version starts from an empty state and never blocks connecting.

use crate::domain::activity::{self, RECENT_LIMIT};
use crate::error::Error;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Current `state.json` schema. Any other version is discarded.
pub const VERSION: u32 = 1;

/// The whole state file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppState {
    /// Schema version. Missing values load as [`VERSION`].
    #[serde(default = "current_version")]
    pub version: u32,
    /// Keyed by host name.
    #[serde(default)]
    pub hosts: BTreeMap<String, HostRecord>,
    /// Last screen and search box, restored when `start_screen` is `last`.
    #[serde(default)]
    pub ui: UiState,
}

/// One host's connection history. Health samples arrive in a later phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HostRecord {
    /// RFC3339 time of the last connect attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_connected: Option<String>,
    /// How many times a session was started.
    #[serde(default)]
    pub connect_count: u64,
    /// Shown in the favorites group.
    #[serde(default)]
    pub favorite: bool,
}

/// UI fields that survive a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UiState {
    /// `hosts`, `health`, `files`, `sessions`, or `log`.
    #[serde(default)]
    pub last_screen: String,
    /// Search text without the leading `/`.
    #[serde(default)]
    pub last_filter: String,
}

fn current_version() -> u32 {
    VERSION
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            version: VERSION,
            hosts: BTreeMap::new(),
            ui: UiState::default(),
        }
    }
}

impl AppState {
    /// Remember a connection attempt and bump the count.
    pub fn record_connect(&mut self, name: &str, now: DateTime<Utc>) {
        let record = self.hosts.entry(name.to_string()).or_default();
        record.last_connected = Some(now.to_rfc3339_opts(SecondsFormat::Secs, true));
        record.connect_count = record.connect_count.saturating_add(1);
    }

    /// Flip the favorite flag. Returns the new value.
    pub fn toggle_favorite(&mut self, name: &str) -> bool {
        let record = self.hosts.entry(name.to_string()).or_default();
        record.favorite = !record.favorite;
        record.favorite
    }

    /// Newest connections first, capped at [`RECENT_LIMIT`].
    pub fn recent_names(&self) -> Vec<String> {
        let mut rows: Vec<(&str, &str)> = self
            .hosts
            .iter()
            .filter_map(|(name, record)| {
                record
                    .last_connected
                    .as_deref()
                    .map(|stamp| (stamp, name.as_str()))
            })
            .collect();
        rows.sort_by(|left, right| right.0.cmp(left.0).then(left.1.cmp(right.1)));
        rows.into_iter()
            .take(RECENT_LIMIT)
            .map(|(_, name)| name.to_string())
            .collect()
    }

    /// Favorite host names. The table still shows them in inventory order.
    pub fn favorite_names(&self) -> Vec<String> {
        self.hosts
            .iter()
            .filter(|(_, record)| record.favorite)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Frecency for `name` at `now`. Unknown hosts score 0.
    pub fn score(&self, name: &str, now: DateTime<Utc>) -> u64 {
        let Some(record) = self.hosts.get(name) else {
            return 0;
        };
        let last = record
            .last_connected
            .as_deref()
            .and_then(activity::parse_timestamp);
        activity::frecency(record.connect_count, last, now)
    }

    /// Short last-connected label, when this host has been opened.
    pub fn age_label(&self, name: &str, now: DateTime<Utc>) -> Option<String> {
        let stamp = self.hosts.get(name)?.last_connected.as_deref()?;
        let then = activity::parse_timestamp(stamp)?;
        Some(activity::compact_age(then, now))
    }

    /// Drop a host's history after it is deleted.
    pub fn forget_host(&mut self, name: &str) {
        self.hosts.remove(name);
    }

    /// Move history when a host is renamed. Jump references in `hosts.toml` are updated separately.
    pub fn rename_host(&mut self, old_name: &str, new_name: &str) {
        if old_name == new_name {
            return;
        }
        if let Some(record) = self.hosts.remove(old_name) {
            self.hosts.insert(new_name.to_string(), record);
        }
    }

    /// Store the screen name and the current search text.
    pub fn remember_ui(&mut self, screen: &str, filter: &str) {
        self.ui.last_screen = screen.to_string();
        self.ui.last_filter = filter.to_string();
    }
}

/// Read `path`. Failure returns a fresh state plus a warning for the log and the toast.
pub fn load(path: &Path) -> (AppState, Option<String>) {
    if !path.exists() {
        return (AppState::default(), None);
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            let warning = format!(
                "could not read {path}: {error}; starting with empty history",
                path = path.display()
            );
            tracing::warn!("{warning}");
            return (AppState::default(), Some(warning));
        }
    };
    match serde_json::from_str::<AppState>(&text) {
        Ok(state) if state.version == VERSION => (state, None),
        Ok(state) => {
            let warning = format!(
                "state file {} has version {}, which this vcon does not understand; starting fresh",
                path.display(),
                state.version
            );
            tracing::warn!("{warning}");
            (AppState::default(), Some(warning))
        }
        Err(error) => {
            let warning = format!(
                "state file {} is unreadable ({error}); starting fresh",
                path.display()
            );
            tracing::warn!("{warning}");
            (AppState::default(), Some(warning))
        }
    }
}

/// Write `state` atomically. The parent directory is created when needed.
pub fn save(path: &Path, state: &AppState) -> Result<(), Error> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|source| Error::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let text = serde_json::to_string_pretty(state)
        .map_err(|error| Error::Message(format!("could not encode state: {error}")))?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text).map_err(|source| Error::Write {
        path: temporary.clone(),
        source,
    })?;
    std::fs::rename(&temporary, path).map_err(|source| Error::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        activity::parse_timestamp("2026-10-26T12:00:00Z").unwrap()
    }

    #[test]
    fn records_recents_favorites_and_scores() {
        let mut state = AppState::default();
        state.record_connect("web-1", now());
        state.record_connect("db-1", now() + chrono::TimeDelta::minutes(5));
        state.record_connect("web-1", now() + chrono::TimeDelta::minutes(10));
        assert!(state.toggle_favorite("db-1"));
        assert!(!state.toggle_favorite("db-1"));
        assert!(state.toggle_favorite("web-1"));
        assert_eq!(
            state.recent_names(),
            vec!["web-1".to_string(), "db-1".to_string()]
        );
        assert_eq!(state.hosts["web-1"].connect_count, 2);
        assert!(state.score("web-1", now()) > state.score("missing", now()));
        assert_eq!(
            state
                .age_label(
                    "db-1",
                    now() + chrono::TimeDelta::minutes(5) + chrono::TimeDelta::hours(2)
                )
                .as_deref(),
            Some("2h")
        );
        assert_eq!(state.favorite_names(), vec!["web-1".to_string()]);
    }

    #[test]
    fn missing_corrupt_and_future_versions_start_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let (state, warning) = load(&path);
        assert!(warning.is_none());
        assert!(state.hosts.is_empty());

        std::fs::write(&path, "{").unwrap();
        let (state, warning) = load(&path);
        assert!(state.hosts.is_empty());
        assert!(warning.unwrap().contains("unreadable"));

        std::fs::write(&path, r#"{"version": 9}"#).unwrap();
        let (state, warning) = load(&path);
        assert_eq!(state.version, VERSION);
        assert!(warning.unwrap().contains("version 9"));
    }

    #[test]
    fn round_trip_keeps_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("state.json");
        let mut state = AppState::default();
        state.record_connect("web-1", now());
        state.remember_ui("hosts", "@prod");
        save(&path, &state).unwrap();
        let (loaded, warning) = load(&path);
        assert!(warning.is_none());
        assert_eq!(loaded.hosts["web-1"].connect_count, 1);
        assert_eq!(loaded.ui.last_filter, "@prod");
        assert_eq!(loaded.version, VERSION);
    }
}
