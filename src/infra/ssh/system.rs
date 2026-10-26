//! `SshBackend` implemented by spawning the system `ssh` binary.
//!
//! Interactive sessions inherit stdin, stdout, and stderr so the remote shell
//! owns the real terminal. The binary is located on `PATH` once; a missing
//! `ssh` is reported before anything is spawned.

use crate::error::Error;
use crate::infra::ssh::backend::SshBackend;
use crate::infra::ssh::command::command_line;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

/// A located OpenSSH client.
#[derive(Debug, Clone)]
pub struct SystemSsh {
    /// Absolute or `PATH`-relative path of the `ssh` binary.
    pub program: PathBuf,
}

impl SystemSsh {
    /// Use `program` as the ssh binary.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl SshBackend for SystemSsh {
    fn interactive(&self, args: &[String]) -> io::Result<ExitStatus> {
        let mut displayed = vec![self.program.display().to_string()];
        displayed.extend(args.iter().cloned());
        tracing::debug!(command = %command_line(&displayed), "starting ssh");
        Command::new(&self.program)
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
    }
}

/// Message shown when `ssh` cannot be found.
pub fn missing_ssh_message() -> &'static str {
    "ssh was not found on PATH. Install OpenSSH before connecting."
}

/// Find `ssh` on the process `PATH`.
pub fn find_ssh() -> Result<PathBuf, Error> {
    find_program("ssh", std::env::var_os("PATH").as_deref())
        .ok_or_else(|| Error::Message(missing_ssh_message().to_string()))
}

/// Search `path_var` for an executable named `name`.
pub fn find_program(name: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    let path_var = path_var?;
    for directory in std::env::split_paths(path_var) {
        let candidate = directory.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_returns_the_child_status() {
        let ssh = SystemSsh::new("true");
        let status = ssh.interactive(&[]).unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    fn find_program_reads_the_given_path() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("ssh");
        std::fs::write(&binary, b"").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let found = find_program("ssh", Some(dir.path().as_os_str())).unwrap();
        assert_eq!(found, binary);
        assert!(find_program("missing", Some(dir.path().as_os_str())).is_none());
    }
}
