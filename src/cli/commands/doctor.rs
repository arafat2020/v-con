//! `vcon doctor`: check ssh, config, agent, control dir, and terminal.
//!
//! Each check prints `OK`, `WARN`, or `FAIL`. Any `FAIL` yields a non-zero exit.

use crate::config;
use crate::error::Error;
use crate::infra::fsutil;
use crate::infra::ssh::system::find_program;
use crate::paths::Paths;
use crate::services::inventory;
use std::io::IsTerminal;
use std::process::{Command, ExitCode};

/// One printed check line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

/// Run every check and print results. Exit code 1 when any check failed.
pub fn run(paths: &Paths) -> Result<ExitCode, Error> {
    let mut failed = false;
    let mut warn = |name: &str, detail: String| {
        println!("[{}] {name}: {detail}", Level::Warn.label());
    };
    let mut ok = |name: &str, detail: String| {
        println!("[{}] {name}: {detail}", Level::Ok.label());
    };
    let mut fail = |name: &str, detail: String| {
        failed = true;
        println!("[{}] {name}: {detail}", Level::Fail.label());
    };

    check_program("ssh", &mut ok, &mut fail);
    check_program("rsync", &mut ok, &mut warn);

    match config::load(&paths.config_file()) {
        Ok(_) if !paths.config_file().exists() => {
            ok(
                "config.toml",
                format!(
                    "missing; using defaults ({})",
                    paths.config_file().display()
                ),
            );
        }
        Ok(_) => ok(
            "config.toml",
            format!("valid ({})", paths.config_file().display()),
        ),
        Err(error) => fail("config.toml", error.to_string()),
    }

    match inventory::load_hosts(&paths.hosts_file(), &crate::paths::local_user()) {
        Ok(bundle) if bundle.problems.is_empty() => {
            let note = if paths.hosts_file().exists() {
                format!(
                    "{} hosts ({})",
                    bundle.inventory.hosts.len(),
                    paths.hosts_file().display()
                )
            } else {
                format!(
                    "missing; empty inventory ({})",
                    paths.hosts_file().display()
                )
            };
            ok("hosts.toml", note);
        }
        Ok(bundle) => {
            let mut detail = format!(
                "{} validation problem(s) in {}",
                bundle.problems.len(),
                paths.hosts_file().display()
            );
            for problem in bundle.problems.iter().take(5) {
                detail.push_str("\n         - ");
                detail.push_str(&problem.to_string());
            }
            if bundle.problems.len() > 5 {
                detail.push_str("\n         - …");
            }
            fail("hosts.toml", detail);
        }
        Err(error) => fail("hosts.toml", error.to_string()),
    }

    check_agent(&mut ok, &mut warn, &mut fail);

    let control = paths.control_dir();
    if fsutil::dir_is_writable(&control) {
        ok("control dir", format!("writable ({})", control.display()));
    } else {
        fail(
            "control dir",
            format!("not creatable/writable ({})", control.display()),
        );
    }

    check_terminal(&mut ok, &mut warn);

    if failed {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn check_program(name: &str, ok: &mut dyn FnMut(&str, String), bad: &mut dyn FnMut(&str, String)) {
    match find_program(name, std::env::var_os("PATH").as_deref()) {
        Some(path) => ok(name, format!("found ({})", path.display())),
        None => bad(name, "not found on PATH".to_string()),
    }
}

fn check_agent(
    ok: &mut dyn FnMut(&str, String),
    warn: &mut dyn FnMut(&str, String),
    fail: &mut dyn FnMut(&str, String),
) {
    let Some(sock) = std::env::var_os("SSH_AUTH_SOCK") else {
        warn(
            "ssh-agent",
            "SSH_AUTH_SOCK is unset; agent-forwarded keys will not work".to_string(),
        );
        return;
    };
    if sock.is_empty() {
        warn("ssh-agent", "SSH_AUTH_SOCK is empty".to_string());
        return;
    }
    let output = Command::new("ssh-add").arg("-l").output();
    match output {
        Ok(output) if output.status.success() => {
            let count = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count();
            ok(
                "ssh-agent",
                format!("{count} key(s) via {}", PathDisplay(&sock)),
            );
        }
        Ok(output) if output.status.code() == Some(1) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = if stderr.to_ascii_lowercase().contains("no identit")
                || stderr.to_ascii_lowercase().contains("no keys")
            {
                format!("agent running but no keys loaded ({})", PathDisplay(&sock))
            } else {
                format!("ssh-add -l exited 1 ({})", stderr.trim().replace('\n', " "))
            };
            warn("ssh-agent", detail);
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            fail(
                "ssh-agent",
                format!("ssh-add -l failed ({})", stderr.trim().replace('\n', " ")),
            );
        }
        Err(error) => fail("ssh-agent", format!("could not run ssh-add: {error}")),
    }
}

fn check_terminal(ok: &mut dyn FnMut(&str, String), warn: &mut dyn FnMut(&str, String)) {
    let tty = std::io::stdout().is_terminal();
    let size = crossterm::terminal::size().ok();
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let colorterm = std::env::var("COLORTERM").ok();
    let mut parts = Vec::new();
    if tty {
        parts.push("tty".to_string());
    } else {
        parts.push("not a tty".to_string());
    }
    match size {
        Some((cols, rows)) => parts.push(format!("{cols}x{rows}")),
        None => parts.push("size unknown".to_string()),
    }
    if no_color {
        parts.push("NO_COLOR set".to_string());
    } else if let Some(value) = colorterm {
        parts.push(format!("COLORTERM={value}"));
    } else {
        parts.push("colours assumed".to_string());
    }
    let detail = parts.join(", ");
    if tty {
        ok("terminal", detail);
    } else {
        warn("terminal", detail);
    }
}

struct PathDisplay<'a>(&'a std::ffi::OsStr);

impl std::fmt::Display for PathDisplay<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.to_string_lossy())
    }
}
