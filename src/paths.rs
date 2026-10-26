//! Directories vcon reads and writes.
//!
//! Config follows `--config-dir`, then `VCON_CONFIG_DIR`, then
//! `$XDG_CONFIG_HOME/vcon`, then `~/.config/vcon`. Data, state, and cache
//! follow the XDG variables and otherwise live under `~/.local` and `~/.cache`.
//! macOS uses the same `~/.config` layout rather than `~/Library`.

use std::path::{Path, PathBuf};

/// Resolved directories for one process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Directory that holds `hosts.toml` and `config.toml`.
    pub config_dir: PathBuf,
    /// `~/.local/share/vcon` unless `XDG_DATA_HOME` is set.
    pub data_dir: PathBuf,
    /// `~/.local/state/vcon` unless `XDG_STATE_HOME` is set. The debug log lives here.
    pub state_dir: PathBuf,
    /// `~/.cache/vcon` unless `XDG_CACHE_HOME` is set. Control sockets live in `cm/`.
    pub cache_dir: PathBuf,
}

impl Paths {
    /// `--config-dir`, then `VCON_CONFIG_DIR`, then `XDG_CONFIG_HOME/vcon`, then `~/.config/vcon`.
    pub fn resolve(config_dir_flag: Option<&Path>) -> Self {
        let home = home_dir();
        let config_dir = if let Some(dir) = config_dir_flag {
            dir.to_path_buf()
        } else if let Some(dir) = std::env::var_os("VCON_CONFIG_DIR") {
            PathBuf::from(dir)
        } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            PathBuf::from(xdg).join("vcon")
        } else {
            home.join(".config/vcon")
        };

        let data_dir = xdg_or_default("XDG_DATA_HOME", ".local/share").join("vcon");
        let state_dir = xdg_or_default("XDG_STATE_HOME", ".local/state").join("vcon");
        let cache_dir = xdg_or_default("XDG_CACHE_HOME", ".cache").join("vcon");

        Self {
            config_dir,
            data_dir,
            state_dir,
            cache_dir,
        }
    }

    /// `hosts.toml` inside the config directory.
    pub fn hosts_file(&self) -> PathBuf {
        self.config_dir.join("hosts.toml")
    }

    /// `config.toml` inside the config directory.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// Debug log path. The TUI cannot print to stderr once it owns the screen.
    #[allow(dead_code)]
    pub fn log_file(&self) -> PathBuf {
        self.state_dir.join("vcon.log")
    }

    /// Directory OpenSSH uses for `ControlPath`.
    pub fn control_dir(&self) -> PathBuf {
        self.cache_dir.join("cm")
    }

    /// Recents, favorites, and the last screen. vcon owns this file.
    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join("state.json")
    }
}

fn home_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn xdg_or_default(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(fallback))
}

/// Login name used when neither the host nor `[defaults]` sets `user`.
///
/// Reads `USER`, then `USERNAME`, and falls back to `root`.
pub fn local_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "root".to_string())
}
