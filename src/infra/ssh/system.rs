//! `SshBackend` implemented by spawning the system `ssh` binary.
//!
//! Interactive sessions inherit stdin, stdout, and stderr so the remote shell
//! owns the real terminal. The binary is located on `PATH` once; a missing
//! `ssh` is reported before anything is spawned.

use crate::error::Error;
use crate::infra::ssh::backend::SshBackend;
use crate::infra::ssh::command::command_line;
use crate::services::exec::{ExecRunner, JobEvent, ProcessResult};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Captured output is kept to this many bytes per host. The tail is what remains.
const OUTPUT_CAP: usize = 1024 * 1024;

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

    fn forward(&self, args: &[String]) -> Result<(), Error> {
        let mut displayed = vec![self.program.display().to_string()];
        displayed.extend(args.iter().cloned());
        tracing::debug!(command = %command_line(&displayed), "starting tunnel");
        let status = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|error| Error::Message(format!("ssh forward failed: {error}")))?;
        if status.success() || status.code().is_none() {
            Ok(())
        } else {
            Err(Error::Message(format!(
                "ssh forward exited with {}",
                status.code().unwrap_or(-1)
            )))
        }
    }
}

impl ExecRunner for SystemSsh {
    async fn run(
        self: Arc<Self>,
        host: String,
        args: Vec<String>,
        timeout: Duration,
        cancel: CancellationToken,
        lines: mpsc::Sender<JobEvent>,
    ) -> ProcessResult {
        self.exec_lines(&host, &args, timeout, &cancel, &lines)
            .await
    }
}

impl SystemSsh {
    /// Run one non-interactive ssh. Stdin is null so a prompt cannot eat keystrokes.
    ///
    /// Stdout and stderr are read at the same time. The timeout and the cancel
    /// token both kill the child. Dropping this future also kills it.
    pub async fn exec_lines(
        &self,
        host: &str,
        args: &[String],
        timeout: Duration,
        cancel: &CancellationToken,
        lines: &mpsc::Sender<JobEvent>,
    ) -> ProcessResult {
        let mut displayed = vec![self.program.display().to_string()];
        displayed.extend(args.iter().cloned());
        tracing::debug!(command = %command_line(&displayed), "starting ssh exec");
        let mut child = match TokioCommand::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                return ProcessResult {
                    code: None,
                    timed_out: false,
                    cancelled: false,
                    spawn_error: Some(error.to_string()),
                    truncated: false,
                    stderr: String::new(),
                };
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let truncated = Arc::new(AtomicBool::new(false));
        let bytes = Arc::new(AtomicUsize::new(0));
        let mut stderr_text = String::new();
        let mut out_done = stdout.is_none();
        let mut err_done = stderr.is_none();
        let mut stdout = stdout.map(BufReader::new);
        let mut stderr = stderr.map(BufReader::new);
        let mut out_buf = Vec::new();
        let mut err_buf = Vec::new();
        let timeout_at = (!timeout.is_zero()).then(|| tokio::time::Instant::now() + timeout);
        let status = loop {
            if out_done && err_done {
                break child.wait().await;
            }
            let sleep = async {
                if let Some(deadline) = timeout_at {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let _ = child.start_kill();
                    return ProcessResult {
                        code: None,
                        timed_out: false,
                        cancelled: true,
                        spawn_error: None,
                        truncated: truncated.load(Ordering::Relaxed),
                        stderr: stderr_text,
                    };
                }
                _ = sleep => {
                    let _ = child.start_kill();
                    return ProcessResult {
                        code: None,
                        timed_out: true,
                        cancelled: false,
                        spawn_error: None,
                        truncated: truncated.load(Ordering::Relaxed),
                        stderr: stderr_text,
                    };
                }
                line = read_line(stdout.as_mut(), &mut out_buf), if !out_done => {
                    match line {
                        None => out_done = true,
                        Some(text) => {
                            if !forward_line(host, false, &text, lines, &bytes, &truncated).await {
                                let _ = child.start_kill();
                                return ProcessResult {
                                    code: None,
                                    timed_out: false,
                                    cancelled: true,
                                    spawn_error: None,
                                    truncated: truncated.load(Ordering::Relaxed),
                                    stderr: stderr_text,
                                };
                            }
                        }
                    }
                }
                line = read_line(stderr.as_mut(), &mut err_buf), if !err_done => {
                    match line {
                        None => err_done = true,
                        Some(text) => {
                            note_stderr(&mut stderr_text, &text);
                            if !forward_line(host, true, &text, lines, &bytes, &truncated).await {
                                let _ = child.start_kill();
                                return ProcessResult {
                                    code: None,
                                    timed_out: false,
                                    cancelled: true,
                                    spawn_error: None,
                                    truncated: truncated.load(Ordering::Relaxed),
                                    stderr: stderr_text,
                                };
                            }
                        }
                    }
                }
            }
        };
        let code = status.ok().and_then(|status| status.code());
        ProcessResult {
            code,
            timed_out: false,
            cancelled: false,
            spawn_error: None,
            truncated: truncated.load(Ordering::Relaxed),
            stderr: stderr_text,
        }
    }
}

async fn read_line<R>(reader: Option<&mut BufReader<R>>, buf: &mut Vec<u8>) -> Option<String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let reader = reader?;
    buf.clear();
    match reader.read_until(b'\n', buf).await {
        Ok(0) => None,
        Ok(_) => Some(decode_line(buf)),
        Err(_) => None,
    }
}

fn decode_line(buf: &[u8]) -> String {
    let mut end = buf.len();
    if end > 0 && buf[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && buf[end - 1] == b'\r' {
        end -= 1;
    }
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

fn note_stderr(stderr: &mut String, text: &str) {
    if stderr.len() >= OUTPUT_CAP {
        return;
    }
    stderr.push_str(text);
    stderr.push('\n');
    if stderr.len() > OUTPUT_CAP {
        let extra = stderr.len() - OUTPUT_CAP;
        let mut cut = extra;
        while !stderr.is_char_boundary(cut) {
            cut += 1;
        }
        stderr.drain(..cut);
    }
}

async fn forward_line(
    host: &str,
    stderr: bool,
    text: &str,
    lines: &mpsc::Sender<JobEvent>,
    bytes: &AtomicUsize,
    truncated: &AtomicBool,
) -> bool {
    let added = text.len() + 1;
    let previous = bytes.fetch_add(added, Ordering::Relaxed);
    if previous >= OUTPUT_CAP {
        bytes.fetch_sub(added, Ordering::Relaxed);
        if !truncated.swap(true, Ordering::Relaxed) {
            let _ = lines
                .send(JobEvent::Line {
                    host: host.to_string(),
                    stderr,
                    text: "[truncated]".to_string(),
                })
                .await;
        }
        return true;
    }
    lines
        .send(JobEvent::Line {
            host: host.to_string(),
            stderr,
            text: text.to_string(),
        })
        .await
        .is_ok()
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

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_streams_the_remote_command_and_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("ssh");
        std::fs::write(
            &program,
            "#!/bin/sh\nlast=\nfor arg in \"$@\"; do last=$arg; done\nprintf '%s\\n' \"$last\"\necho err-line >&2\nexit 0\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let ssh = SystemSsh::new(program);
        let result = ssh
            .exec_lines(
                "web-1",
                &["echo '$HOME' && true".to_string()],
                std::time::Duration::from_secs(2),
                &tokio_util::sync::CancellationToken::new(),
                &tx,
            )
            .await;
        drop(tx);
        let mut lines = Vec::new();
        while let Some(event) = rx.recv().await {
            lines.push(event);
        }
        assert_eq!(result.code, Some(0), "{result:?}");
        assert!(lines.iter().any(|event| matches!(
            event,
            crate::services::exec::JobEvent::Line { stderr: false, text, .. } if text == "echo '$HOME' && true"
        )));
        assert!(lines.iter().any(|event| matches!(
            event,
            crate::services::exec::JobEvent::Line { stderr: true, text, .. } if text == "err-line"
        )));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_kills_a_hung_command_at_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("ssh");
        std::fs::write(&program, "#!/bin/sh\nsleep 30\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let ssh = SystemSsh::new(program);
        let started = std::time::Instant::now();
        let result = ssh
            .exec_lines(
                "web-1",
                &[],
                std::time::Duration::from_millis(200),
                &tokio_util::sync::CancellationToken::new(),
                &tx,
            )
            .await;
        assert!(result.timed_out, "{result:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
