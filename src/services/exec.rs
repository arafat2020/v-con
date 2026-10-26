//! Run one remote command on many hosts.
//!
//! The service owns concurrency, cancellation, and the confirmation policy.
//! It does not draw. The TUI and `vcon exec` both call [`run_job`] and render
//! the [`JobEvent`] stream themselves. A runner that returns an error for one
//! host does not stop the others.

use crate::domain::host::EffectiveHost;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

/// Bytes of captured output kept per host. Older text is dropped.
pub const OUTPUT_CAP: usize = 1024 * 1024;

/// Per-host limit used when the caller does not set one.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// How many finished commands are kept in the state file.
pub const HISTORY_LIMIT: usize = 50;

/// Settings that decide whether a run must be confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecSettings {
    /// Ask when the target list is longer than this.
    pub confirm_threshold: usize,
    /// Any host carrying one of these tags must be confirmed.
    pub protected_tags: Vec<String>,
}

/// Why a run should ask before it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmReason {
    /// Hosts in the run.
    pub count: usize,
    /// Configured threshold. The count may still be under it when a tag matched.
    pub threshold: usize,
    /// Protected tags present on at least one target.
    pub tags: Vec<String>,
}

impl ConfirmReason {
    /// Sentence used by the confirm modal and the CLI prompt.
    pub fn message(&self) -> String {
        let count = format!("{} hosts", self.count);
        if self.tags.is_empty() {
            return format!("{count} is above {}", self.threshold);
        }
        let tags = self.tags.join(", ");
        if self.count > self.threshold {
            format!("{count} is above {}, including {tags}", self.threshold)
        } else {
            format!("{count}, including {tags}")
        }
    }
}

/// `Some` when the run is wider than the threshold or touches a protected tag.
pub fn needs_confirmation(
    hosts: &[EffectiveHost],
    settings: &ExecSettings,
) -> Option<ConfirmReason> {
    let mut tags = BTreeSet::new();
    for host in hosts {
        for tag in &host.tags {
            if settings
                .protected_tags
                .iter()
                .any(|protected| protected == tag)
            {
                tags.insert(tag.clone());
            }
        }
    }
    let too_many = hosts.len() > settings.confirm_threshold;
    if !too_many && tags.is_empty() {
        return None;
    }
    Some(ConfirmReason {
        count: hosts.len(),
        threshold: settings.confirm_threshold,
        tags: tags.into_iter().collect(),
    })
}

/// What to do with a required confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmGate {
    /// No confirmation was required, or `--yes` was passed.
    Run,
    /// A terminal is attached. The caller should prompt.
    Ask,
    /// Not a terminal and `--yes` was not passed.
    Refuse,
}

/// Decide whether to run, prompt, or refuse.
pub fn confirmation_gate(needed: bool, yes: bool, tty: bool) -> ConfirmGate {
    if !needed || yes {
        ConfirmGate::Run
    } else if tty {
        ConfirmGate::Ask
    } else {
        ConfirmGate::Refuse
    }
}

/// Remote command text. `sudo` uses `-n` so a password prompt cannot hang the run.
pub fn remote_command(command: &str, sudo: bool) -> String {
    let command = command.trim();
    if sudo {
        format!("sudo -n {command}")
    } else {
        command.to_string()
    }
}

/// Payload the event loop needs to spawn an exec job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecLaunch {
    /// ssh binary.
    pub program: std::path::PathBuf,
    /// One entry per host. `args` already include the remote command.
    pub targets: Vec<HostTarget>,
    /// Per-host timeout. Zero waits without a limit.
    pub timeout: Duration,
    /// Maximum concurrent ssh processes.
    pub parallel: usize,
}

/// One host the runner should execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostTarget {
    /// Inventory name, used in events.
    pub name: String,
    /// Arguments after the ssh binary, including the remote command as the last item.
    pub args: Vec<String>,
}

/// How a single host ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndStatus {
    /// Remote exit code 0.
    Ok,
    /// Remote command failed, or ssh exited with an error.
    Failed,
    /// The per-host timeout fired.
    Timeout,
    /// The job was cancelled while this host was running.
    Cancelled,
    /// The job was cancelled before this host started.
    Skipped,
    /// ssh could not be spawned.
    SpawnFailed,
}

/// Result of one host, without its output lines. Lines arrive as [`JobEvent::Line`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostOutcome {
    /// How the host ended.
    pub status: EndStatus,
    /// Remote or ssh exit code, when the process exited.
    pub code: Option<i32>,
    /// Short reason for the status line.
    pub reason: String,
    /// Time from start until the host finished.
    pub duration: Duration,
    /// Output was cut down to [`OUTPUT_CAP`].
    pub truncated: bool,
}

/// Counts printed after every host has finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Hosts whose remote command exited 0.
    pub ok: usize,
    /// Failures, timeouts, cancellations, and spawn errors.
    pub failed: usize,
    /// Hosts that never started.
    pub skipped: usize,
    /// Wall time of the whole job.
    pub duration: Duration,
}

/// Events in the order a host produces them. Hosts run in parallel, so events
/// from different hosts are interleaved. One host's lines stay in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEvent {
    /// The host acquired a concurrency slot and ssh is starting.
    HostStarted {
        /// Inventory name.
        host: String,
    },
    /// One line of output, without the trailing newline.
    Line {
        /// Inventory name.
        host: String,
        /// True when the line came from stderr.
        stderr: bool,
        /// Line text.
        text: String,
    },
    /// The host is done. No further lines will arrive for it.
    HostFinished {
        /// Inventory name.
        host: String,
        /// Status, exit code, and duration.
        outcome: HostOutcome,
    },
    /// Every host has finished or been skipped.
    JobFinished {
        /// Totals for the status line.
        summary: Summary,
    },
}

/// What the ssh runner returns after one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessResult {
    /// Process exit code. Empty when the process was killed.
    pub code: Option<i32>,
    /// The runner stopped because the timeout elapsed.
    pub timed_out: bool,
    /// The runner stopped because the job was cancelled.
    pub cancelled: bool,
    /// ssh could not be started.
    pub spawn_error: Option<String>,
    /// Captured output hit [`OUTPUT_CAP`].
    pub truncated: bool,
    /// Stderr text kept for ssh failure classification. Capped with the rest.
    pub stderr: String,
}

/// Spawns ssh for one host and streams its output.
///
/// Implementations must honor `timeout` and `cancel`, and must kill the child
/// when the future is dropped so a cancelled job does not leave `ssh` behind.
pub trait ExecRunner: Send + Sync + 'static {
    /// Run `args` with the ssh binary. Send each output line before returning.
    fn run(
        self: Arc<Self>,
        host: String,
        args: Vec<String>,
        timeout: Duration,
        cancel: CancellationToken,
        lines: mpsc::Sender<JobEvent>,
    ) -> impl std::future::Future<Output = ProcessResult> + Send;
}

/// Run `command` on every target. `parallel` is at least one.
///
/// `command` is the text the user typed. [`remote_command`] is applied when
/// `sudo` is set, and that string is appended as a single ssh argument by the
/// caller when it builds [`HostTarget::args`].
pub async fn run_job<R: ExecRunner>(
    runner: Arc<R>,
    targets: Vec<HostTarget>,
    timeout: Duration,
    parallel: usize,
    cancel: CancellationToken,
    events: mpsc::Sender<JobEvent>,
) -> Summary {
    let started = Instant::now();
    let slots = Arc::new(Semaphore::new(parallel.max(1)));
    let total = targets.len();
    let mut handles = Vec::with_capacity(total);
    for target in targets {
        let slots = Arc::clone(&slots);
        let runner = Arc::clone(&runner);
        let cancel = cancel.clone();
        let events = events.clone();
        handles.push(tokio::spawn(async move {
            run_one(runner, slots, target, timeout, cancel, events).await
        }));
    }
    let mut summary = Summary {
        ok: 0,
        failed: 0,
        skipped: 0,
        duration: Duration::ZERO,
    };
    for handle in handles {
        match handle.await {
            Ok(EndStatus::Ok) => summary.ok += 1,
            Ok(EndStatus::Skipped) => summary.skipped += 1,
            Ok(_) => summary.failed += 1,
            Err(_) => summary.failed += 1,
        }
    }
    summary.duration = started.elapsed();
    let _ = events.send(JobEvent::JobFinished { summary }).await;
    summary
}

async fn run_one<R: ExecRunner>(
    runner: Arc<R>,
    slots: Arc<Semaphore>,
    target: HostTarget,
    timeout: Duration,
    cancel: CancellationToken,
    events: mpsc::Sender<JobEvent>,
) -> EndStatus {
    if cancel.is_cancelled() {
        return finish_skipped(&target.name, &events).await;
    }
    let Ok(permit) = slots.acquire_owned().await else {
        return finish_skipped(&target.name, &events).await;
    };
    if cancel.is_cancelled() {
        drop(permit);
        return finish_skipped(&target.name, &events).await;
    }
    if events
        .send(JobEvent::HostStarted {
            host: target.name.clone(),
        })
        .await
        .is_err()
    {
        drop(permit);
        return EndStatus::Cancelled;
    }
    let started = Instant::now();
    let host_cancel = cancel.clone();
    let result = runner
        .run(
            target.name.clone(),
            target.args,
            timeout,
            host_cancel,
            events.clone(),
        )
        .await;
    drop(permit);
    let status = end_status(&result);
    let outcome = HostOutcome {
        status,
        code: result.code,
        reason: reason_for(&result),
        duration: started.elapsed(),
        truncated: result.truncated,
    };
    let _ = events
        .send(JobEvent::HostFinished {
            host: target.name,
            outcome,
        })
        .await;
    status
}

async fn finish_skipped(host: &str, events: &mpsc::Sender<JobEvent>) -> EndStatus {
    let _ = events
        .send(JobEvent::HostFinished {
            host: host.to_string(),
            outcome: HostOutcome {
                status: EndStatus::Skipped,
                code: None,
                reason: "skipped".to_string(),
                duration: Duration::ZERO,
                truncated: false,
            },
        })
        .await;
    EndStatus::Skipped
}

fn end_status(result: &ProcessResult) -> EndStatus {
    if result.spawn_error.is_some() {
        EndStatus::SpawnFailed
    } else if result.timed_out {
        EndStatus::Timeout
    } else if result.cancelled {
        EndStatus::Cancelled
    } else if result.code == Some(0) {
        EndStatus::Ok
    } else {
        EndStatus::Failed
    }
}

fn reason_for(result: &ProcessResult) -> String {
    if let Some(error) = &result.spawn_error {
        return error.clone();
    }
    if result.timed_out {
        return "timed out".to_string();
    }
    if result.cancelled {
        return "cancelled".to_string();
    }
    match classify_failure(result.code, &result.stderr) {
        FailureClass::Ok => "ok".to_string(),
        FailureClass::Remote(code) => format!("exit {code}"),
        FailureClass::ConnectionRefused => "connection refused".to_string(),
        FailureClass::TimedOut => "connection timed out".to_string(),
        FailureClass::AuthFailed => "auth failed".to_string(),
        FailureClass::HostKeyChanged => "host key changed".to_string(),
        FailureClass::Dns => "dns failure".to_string(),
        FailureClass::Ssh(code) => format!("ssh exited {code}"),
        FailureClass::Signalled => "ssh was killed".to_string(),
    }
}

/// Why a non-interactive ssh ended, based on the exit code and stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// Exit code 0.
    Ok,
    /// The remote command returned this non-zero code. ssh itself succeeded.
    Remote(i32),
    /// `Connection refused`.
    ConnectionRefused,
    /// ssh's own connect timeout, not the job timeout.
    TimedOut,
    /// Public key or password authentication failed.
    AuthFailed,
    /// `REMOTE HOST IDENTIFICATION HAS CHANGED` or a failed host-key check.
    HostKeyChanged,
    /// The address did not resolve.
    Dns,
    /// ssh exited 255 for another reason.
    Ssh(i32),
    /// The process was killed and left no exit code.
    Signalled,
}

/// Classify an ssh exit. Exit 255 is ssh itself; any other code is the remote command.
pub fn classify_failure(code: Option<i32>, stderr: &str) -> FailureClass {
    let Some(code) = code else {
        return FailureClass::Signalled;
    };
    if code == 0 {
        return FailureClass::Ok;
    }
    if code != 255 {
        return FailureClass::Remote(code);
    }
    let text = stderr.to_ascii_lowercase();
    if text.contains("identification has changed") || text.contains("host key verification failed")
    {
        return FailureClass::HostKeyChanged;
    }
    if text.contains("could not resolve")
        || text.contains("name or service not known")
        || text.contains("temporary failure in name resolution")
        || text.contains("nodename nor servname")
    {
        return FailureClass::Dns;
    }
    if text.contains("connection refused") {
        return FailureClass::ConnectionRefused;
    }
    if text.contains("timed out") || text.contains("timeout") {
        return FailureClass::TimedOut;
    }
    if text.contains("permission denied")
        || text.contains("authentication failed")
        || text.contains("publickey")
    {
        return FailureClass::AuthFailed;
    }
    FailureClass::Ssh(code)
}

/// Remember `command` at the end of the history, dropping the oldest past [`HISTORY_LIMIT`].
pub fn push_history(history: &mut Vec<String>, command: &str) {
    let command = command.trim();
    if command.is_empty() {
        return;
    }
    if history.last().map(String::as_str) == Some(command) {
        return;
    }
    history.push(command.to_string());
    if history.len() > HISTORY_LIMIT {
        let extra = history.len() - HISTORY_LIMIT;
        history.drain(..extra);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn host(name: &str, tags: &[&str]) -> EffectiveHost {
        EffectiveHost {
            name: name.to_string(),
            address: "203.0.113.10".to_string(),
            user: "deploy".to_string(),
            port: 22,
            identity_file: None,
            jump: None,
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            notes: None,
            ssh_options: std::collections::BTreeMap::new(),
            disabled: false,
            connect_timeout: None,
            tunnels: Vec::new(),
        }
    }

    struct Scripted {
        in_flight: AtomicUsize,
        max_seen: AtomicUsize,
        /// Hosts that exit 1. Everyone else exits 0 after a short pause.
        fail: Vec<String>,
        hang: Vec<String>,
    }

    impl ExecRunner for Scripted {
        async fn run(
            self: Arc<Self>,
            host: String,
            _args: Vec<String>,
            timeout: Duration,
            cancel: CancellationToken,
            lines: mpsc::Sender<JobEvent>,
        ) -> ProcessResult {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_seen.fetch_max(now, Ordering::SeqCst);
            let hanging = self.hang.iter().any(|name| name == &host);
            let failing = self.fail.iter().any(|name| name == &host);
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => ProcessResult {
                    code: None,
                    timed_out: false,
                    cancelled: true,
                    spawn_error: None,
                    truncated: false,
                    stderr: String::new(),
                },
                _ = tokio::time::sleep(timeout), if !timeout.is_zero() && hanging => ProcessResult {
                    code: None,
                    timed_out: true,
                    cancelled: false,
                    spawn_error: None,
                    truncated: false,
                    stderr: String::new(),
                },
                _ = tokio::time::sleep(Duration::from_millis(40)), if !hanging => {
                    if failing {
                        let _ = lines.send(JobEvent::Line {
                            host: host.clone(),
                            stderr: true,
                            text: "permission denied".to_string(),
                        }).await;
                        ProcessResult {
                            code: Some(1),
                            timed_out: false,
                            cancelled: false,
                            spawn_error: None,
                            truncated: false,
                            stderr: "permission denied".to_string(),
                        }
                    } else {
                        let _ = lines.send(JobEvent::Line {
                            host: host.clone(),
                            stderr: false,
                            text: "ok".to_string(),
                        }).await;
                        ProcessResult {
                            code: Some(0),
                            timed_out: false,
                            cancelled: false,
                            spawn_error: None,
                            truncated: false,
                            stderr: String::new(),
                        }
                    }
                }
            };
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            result
        }
    }

    fn target(name: &str) -> HostTarget {
        HostTarget {
            name: name.to_string(),
            args: vec![name.to_string()],
        }
    }

    async fn collect(
        scripted: Scripted,
        names: &[&str],
        timeout: Duration,
        parallel: usize,
        cancel: CancellationToken,
    ) -> Vec<JobEvent> {
        let (tx, mut rx) = mpsc::channel(64);
        let runner = Arc::new(scripted);
        let targets = names.iter().copied().map(target).collect();
        let task =
            tokio::spawn(
                async move { run_job(runner, targets, timeout, parallel, cancel, tx).await },
            );
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            let finished = matches!(event, JobEvent::JobFinished { .. });
            events.push(event);
            if finished {
                break;
            }
        }
        task.await.unwrap();
        events
    }

    #[test]
    fn confirmation_names_the_threshold_and_protected_tags() {
        let settings = ExecSettings {
            confirm_threshold: 1,
            protected_tags: vec!["prod".to_string()],
        };
        let one = needs_confirmation(&[host("web-1", &["web"])], &settings);
        assert!(one.is_none());
        let wide = needs_confirmation(
            &[host("web-1", &["web"]), host("web-2", &["prod"])],
            &settings,
        )
        .unwrap();
        assert!(wide.message().contains("above 1"));
        assert!(wide.message().contains("prod"));
        assert_eq!(confirmation_gate(true, false, false), ConfirmGate::Refuse);
        assert_eq!(confirmation_gate(true, true, false), ConfirmGate::Run);
        assert_eq!(confirmation_gate(true, false, true), ConfirmGate::Ask);
    }

    #[test]
    fn ssh_stderr_is_classified() {
        assert_eq!(
            classify_failure(
                Some(255),
                "connect to 203.0.113.10 port 22: Connection refused"
            ),
            FailureClass::ConnectionRefused
        );
        assert_eq!(
            classify_failure(Some(255), "Connection timed out"),
            FailureClass::TimedOut
        );
        assert_eq!(
            classify_failure(Some(255), "deploy@host: Permission denied (publickey)."),
            FailureClass::AuthFailed
        );
        assert_eq!(
            classify_failure(
                Some(255),
                "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!"
            ),
            FailureClass::HostKeyChanged
        );
        assert_eq!(
            classify_failure(Some(255), "Could not resolve hostname web.example"),
            FailureClass::Dns
        );
        assert_eq!(
            classify_failure(Some(3), "inactive"),
            FailureClass::Remote(3)
        );
        assert_eq!(remote_command("uptime", true), "sudo -n uptime");
        assert_eq!(
            remote_command("echo '$HOME' && true", false),
            "echo '$HOME' && true"
        );
    }

    #[tokio::test]
    async fn partial_failure_still_finishes_the_other_host() {
        let events = collect(
            Scripted {
                in_flight: AtomicUsize::new(0),
                max_seen: AtomicUsize::new(0),
                fail: vec!["web-2".to_string()],
                hang: Vec::new(),
            },
            &["web-1", "web-2"],
            Duration::from_secs(2),
            2,
            CancellationToken::new(),
        )
        .await;
        let finished: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                JobEvent::HostFinished { host, outcome } => Some((host.as_str(), outcome.status)),
                _ => None,
            })
            .collect();
        assert!(finished.contains(&("web-1", EndStatus::Ok)));
        assert!(finished.contains(&("web-2", EndStatus::Failed)));
        match events.last() {
            Some(JobEvent::JobFinished { summary }) => {
                assert_eq!(summary.ok, 1);
                assert_eq!(summary.failed, 1);
            }
            other => panic!("expected a summary, got {other:?}"),
        }
        let web1 = events
            .iter()
            .position(|event| matches!(event, JobEvent::HostStarted { host } if host == "web-1"));
        let web1_done = events.iter().position(
            |event| matches!(event, JobEvent::HostFinished { host, .. } if host == "web-1"),
        );
        let line = events.iter().position(|event| {
            matches!(event, JobEvent::Line { host, text, .. } if host == "web-1" && text == "ok")
        });
        assert!(web1 < line && line < web1_done);
    }

    #[tokio::test]
    async fn concurrency_stays_within_the_limit() {
        let (tx, mut rx) = mpsc::channel(32);
        let runner = Arc::new(Scripted {
            in_flight: AtomicUsize::new(0),
            max_seen: AtomicUsize::new(0),
            fail: Vec::new(),
            hang: Vec::new(),
        });
        let probe = Arc::clone(&runner);
        let targets = ["a", "b", "c", "d"].into_iter().map(target).collect();
        let task = tokio::spawn(run_job(
            runner,
            targets,
            Duration::from_secs(2),
            2,
            CancellationToken::new(),
            tx,
        ));
        while rx.recv().await.is_some() {}
        task.await.unwrap();
        assert!(probe.max_seen.load(Ordering::SeqCst) <= 2);
        assert!(probe.max_seen.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn timeout_and_cancel_do_not_hang() {
        let events = collect(
            Scripted {
                in_flight: AtomicUsize::new(0),
                max_seen: AtomicUsize::new(0),
                fail: Vec::new(),
                hang: vec!["slow".to_string()],
            },
            &["slow"],
            Duration::from_millis(30),
            1,
            CancellationToken::new(),
        )
        .await;
        assert!(events.iter().any(|event| {
            matches!(
                event,
                JobEvent::HostFinished { outcome, .. } if outcome.status == EndStatus::Timeout
            )
        }));

        let cancel = CancellationToken::new();
        let cancel_task = cancel.clone();
        let collect_task = tokio::spawn(async move {
            collect(
                Scripted {
                    in_flight: AtomicUsize::new(0),
                    max_seen: AtomicUsize::new(0),
                    fail: Vec::new(),
                    hang: vec!["a".to_string(), "b".to_string(), "c".to_string()],
                },
                &["a", "b", "c"],
                Duration::from_secs(30),
                1,
                cancel_task,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
        let events = tokio::time::timeout(Duration::from_secs(2), collect_task)
            .await
            .expect("cancel hung")
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, JobEvent::JobFinished { .. }))
        );
        let skipped = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    JobEvent::HostFinished { outcome, .. } if outcome.status == EndStatus::Skipped
                )
            })
            .count();
        let cancelled = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    JobEvent::HostFinished { outcome, .. }
                        if outcome.status == EndStatus::Cancelled
                )
            })
            .count();
        assert!(cancelled >= 1, "{events:?}");
        assert_eq!(skipped + cancelled, 3, "{events:?}");
    }
}
