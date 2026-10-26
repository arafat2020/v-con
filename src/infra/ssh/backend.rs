//! The seam between a service and a running `ssh`.
//!
//! [`SshBackend::interactive`] hands the terminal to ssh and waits. Exec, copy,
//! and port forwards stay unimplemented until their phases, so a caller that
//! reaches them gets a clear error instead of a partial session.

use crate::error::Error;
use std::io;
use std::process::ExitStatus;

/// Captured output of a non-interactive command. Phase 4 fills this in.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct ExecOutput {
    /// Remote exit code. `None` when the process was signalled.
    pub code: Option<i32>,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
}

/// Operations every SSH implementation must eventually provide.
///
/// `exec`, `copy`, and `forward` are part of the trait now and return an error
/// until their phases. The allow covers those unused defaults in the binary.
#[allow(dead_code)]
pub trait SshBackend {
    /// Run an interactive session with inherited stdin, stdout, and stderr.
    ///
    /// `args` are the arguments after the ssh binary (`-J`, `user@host`, …).
    fn interactive(&self, args: &[String]) -> io::Result<ExitStatus>;

    /// Run one remote command. Implemented in phase 4.
    fn exec(&self, _args: &[String], _remote_command: &str) -> Result<ExecOutput, Error> {
        Err(Error::Message("remote exec arrives in phase 4".to_string()))
    }

    /// Copy files to or from a host. Implemented in phase 6.
    fn copy(&self, _args: &[String]) -> Result<(), Error> {
        Err(Error::Message("file copy arrives in phase 6".to_string()))
    }

    /// Start a port forward. Implemented in phase 8.
    fn forward(&self, _args: &[String]) -> Result<(), Error> {
        Err(Error::Message(
            "port forwards arrive in phase 8".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub;

    impl SshBackend for Stub {
        fn interactive(&self, _args: &[String]) -> io::Result<ExitStatus> {
            Err(io::Error::other("not used"))
        }
    }

    #[test]
    fn later_operations_name_their_phase() {
        let stub = Stub;
        assert!(
            stub.exec(&[], "true")
                .unwrap_err()
                .to_string()
                .contains("phase 4")
        );
        assert!(stub.copy(&[]).unwrap_err().to_string().contains("phase 6"));
        assert!(
            stub.forward(&[])
                .unwrap_err()
                .to_string()
                .contains("phase 8")
        );
    }
}
