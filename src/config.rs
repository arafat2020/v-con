//! Settings, keymap overrides, and theme colours from `config.toml`.
//!
//! Every field is optional. A missing file is [`Config::default`]: 10 parallel
//! sessions, multiplexing on, 30 fps, and the cyan/green/yellow/red palette.
//! A file that exists must parse; unknown keys are rejected.

use crate::error::Error;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Top-level `config.toml`. Defaults match the documented example.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Maximum concurrent ssh processes for later exec and health commands.
    pub parallel: usize,
    /// Ask for confirmation when a command targets more hosts than this.
    pub confirm_threshold: usize,
    /// Tags that always require confirmation. Empty unless the file sets them.
    pub protected_tags: Vec<String>,
    /// Use ssh ControlMaster for the copied command and later sessions.
    pub multiplexing: bool,
    /// Terminal UI preferences.
    pub tui: TuiConfig,
    /// Health-screen refresh interval, history length, and thresholds.
    pub health: HealthConfig,
    /// Embedded-session preferences (prefix key and scrollback).
    pub sessions: SessionConfig,
    /// Keybinding overrides. Unknown action names are ignored so later keys do not break phase 1.
    pub keys: KeyConfig,
    /// Named colours: black, red, green, yellow, blue, magenta, cyan, gray, and white.
    pub theme: ThemeConfig,
}

/// `[tui]` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TuiConfig {
    /// Screen opened on launch. Phase 1 always starts on Hosts.
    pub start_screen: String,
    /// Capture mouse input when true.
    pub mouse: bool,
    /// Maximum redraw rate. `0` is treated as 1 by the event loop.
    pub fps: u16,
    /// Show the detail pane when the terminal is large enough.
    pub show_detail_pane: bool,
}

/// How often the health screen will refresh, and how many samples it keeps.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HealthConfig {
    /// Seconds between polls.
    pub refresh_seconds: u64,
    /// Sparkline points kept per host.
    pub history_samples: usize,
    /// Thresholds that paint a cell as a warning.
    pub warn: HealthThresholds,
    /// Thresholds that paint a cell as critical.
    pub crit: HealthThresholds,
}

/// Disk percent, memory percent, and load-per-CPU levels used by [`HealthConfig`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HealthThresholds {
    /// Disk used, percent.
    pub disk: f64,
    /// Memory used, percent.
    pub mem: f64,
    /// Load average divided by CPU count.
    pub load_per_cpu: f64,
}

/// `[sessions]` section. The prefix key returns focus from an embedded shell.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionConfig {
    /// Default is `ctrl-]`.
    pub prefix_key: String,
    /// Lines of scrollback kept per session.
    pub scrollback_lines: usize,
}

/// `[keys.hosts]` overrides. The map is action name to key, for example `connect = "c"`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct KeyConfig {
    /// Hosts-screen bindings. Unrecognised action names are skipped.
    pub hosts: BTreeMap<String, String>,
}

/// `[theme]` colour names: black, red, green, yellow, blue, magenta, cyan, gray, white.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    /// Selection and cursor colour.
    pub highlight: String,
    /// Colour for a healthy status.
    pub ok: String,
    /// Colour for warnings, including the validation badge.
    pub warn: String,
    /// Colour for errors and selector failures.
    pub crit: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            parallel: 10,
            confirm_threshold: 5,
            protected_tags: Vec::new(),
            multiplexing: true,
            tui: TuiConfig::default(),
            health: HealthConfig::default(),
            sessions: SessionConfig::default(),
            keys: KeyConfig::default(),
            theme: ThemeConfig::default(),
        }
    }
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            start_screen: "hosts".to_string(),
            mouse: false,
            fps: 30,
            show_detail_pane: true,
        }
    }
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            refresh_seconds: 30,
            history_samples: 60,
            warn: HealthThresholds {
                disk: 80.0,
                mem: 85.0,
                load_per_cpu: 1.0,
            },
            crit: HealthThresholds {
                disk: 90.0,
                mem: 95.0,
                load_per_cpu: 2.0,
            },
        }
    }
}

impl Default for HealthThresholds {
    fn default() -> Self {
        Self {
            disk: 80.0,
            mem: 85.0,
            load_per_cpu: 1.0,
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            prefix_key: "ctrl-]".to_string(),
            scrollback_lines: 5000,
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            highlight: "cyan".to_string(),
            ok: "green".to_string(),
            warn: "yellow".to_string(),
            crit: "red".to_string(),
        }
    }
}

/// Load `config.toml`. A missing file returns [`Config::default`].
pub fn load(path: &Path) -> Result<Config, Error> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    toml_edit::de::from_str(&text)
        .map_err(|error| Error::parse(path, &text, error.span(), error.message()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn missing_file_uses_defaults() {
        let path = std::env::temp_dir().join(format!("vcon-no-config-{}.toml", std::process::id()));
        let _ = fs::remove_file(&path);
        let config = load(&path).unwrap();
        assert_eq!(config.parallel, 10);
        assert!(config.multiplexing);
        assert_eq!(config.tui.fps, 30);
        assert_eq!(config.theme.highlight, "cyan");
    }

    #[test]
    fn overrides_theme_keymap_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
            parallel = 4
            multiplexing = false

            [tui]
            fps = 15
            show_detail_pane = false

            [keys.hosts]
            connect = "c"

            [theme]
            highlight = "magenta"
            "#,
        )
        .unwrap();
        let config = load(&path).unwrap();
        assert_eq!(config.parallel, 4);
        assert!(!config.multiplexing);
        assert_eq!(config.tui.fps, 15);
        assert!(!config.tui.show_detail_pane);
        assert_eq!(
            config.keys.hosts.get("connect").map(String::as_str),
            Some("c")
        );
        assert_eq!(config.theme.highlight, "magenta");
        assert_eq!(config.theme.ok, "green");
    }
}
