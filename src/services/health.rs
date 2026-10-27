//! Poll host health and keep a short history.
//!
//! The scheduler reuses [`crate::services::exec::run_job`] so health polls share
//! the same semaphore and `ExecRunner` as remote exec. CPU % and network rates
//! need two samples; the first poll leaves those fields empty. Unreachable
//! hosts back off to two minutes before the next attempt.

use crate::config::{HealthConfig, HealthThresholds};
use crate::domain::health::{
    self, HealthReport, HealthStatus, HistoryPoint, RawSample, Thresholds, history_point, now_secs,
};
use crate::domain::host::EffectiveHost;
use crate::infra::health::SCRIPT;
use crate::infra::ssh::command::exec_arguments;
use crate::services::exec::{
    self as job, EndStatus, ExecRunner, HostOutcome, HostTarget, JobEvent,
};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior, sleep};
use tokio_util::sync::CancellationToken;

/// How long an unreachable host waits before the next poll.
pub const BACKOFF: Duration = Duration::from_secs(120);

/// Per-host connect timeout for a health poll.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(20);

/// Pause / resume / refresh commands from the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthControl {
    /// Stop scheduling new polls.
    Pause,
    /// Resume after [`HealthControl::Pause`].
    Resume,
    /// Poll these hosts immediately. Empty means every known host.
    Refresh(Vec<String>),
    /// Replace the host list after an inventory reload.
    #[allow(dead_code)]
    SetHosts(Vec<HostTarget>),
}

/// One update the TUI folds into its table.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthEvent {
    /// Inventory host name.
    pub host: String,
    /// Derived report (reachable or not).
    pub report: HealthReport,
    /// Sparkline / chart point when the host was reachable.
    pub point: Option<HistoryPoint>,
}

/// Settings the scheduler needs from config and paths.
#[derive(Debug, Clone)]
pub struct HealthSettings {
    /// Seconds between successful polls.
    pub refresh: Duration,
    /// Ring buffer length.
    pub history_samples: usize,
    /// Shared ssh concurrency cap.
    pub parallel: usize,
    /// Warn thresholds.
    pub warn: Thresholds,
    /// Crit thresholds.
    pub crit: Thresholds,
    /// ssh binary.
    pub program: PathBuf,
}

impl HealthSettings {
    /// Build settings from config and the ssh binary.
    pub fn from_config(config: &HealthConfig, parallel: usize, program: PathBuf) -> Self {
        Self {
            refresh: Duration::from_secs(config.refresh_seconds.max(5)),
            history_samples: config.history_samples.max(2),
            parallel: parallel.max(1),
            warn: thresholds(&config.warn),
            crit: thresholds(&config.crit),
            program,
        }
    }
}

/// Per-host cache kept by the scheduler and mirrored in the TUI.
#[derive(Debug, Clone, Default)]
pub struct HostHealth {
    /// Latest derived report.
    pub report: Option<HealthReport>,
    /// Previous raw counters for delta math.
    pub previous: Option<RawSample>,
    /// Ring buffer of sparkline points, oldest first.
    pub history: VecDeque<HistoryPoint>,
    /// When the next poll may run.
    pub next_due: Option<Instant>,
    /// Whether the last poll used the unreachable backoff.
    pub backing_off: bool,
}

impl HostHealth {
    /// Push a history point, trimming to `limit`.
    pub fn push_history(&mut self, point: HistoryPoint, limit: usize) {
        self.history.push_back(point);
        while self.history.len() > limit {
            self.history.pop_front();
        }
    }
}

/// Build ssh targets that run the embedded health script.
pub fn health_targets(
    hosts: &[EffectiveHost],
    names: Option<&[String]>,
    multiplexing: bool,
    control_dir: &Path,
) -> Vec<HostTarget> {
    let selected: Vec<&EffectiveHost> = match names {
        Some(names) if !names.is_empty() => names
            .iter()
            .filter_map(|name| hosts.iter().find(|host| host.name == *name))
            .collect(),
        _ => hosts.iter().collect(),
    };
    selected
        .into_iter()
        .map(|host| HostTarget {
            name: host.name.clone(),
            args: exec_arguments(
                host,
                hosts,
                Some(POLL_TIMEOUT.as_secs()),
                multiplexing,
                Some(&control_dir.display().to_string()),
                SCRIPT,
            ),
        })
        .collect()
}

/// Parse stdout from a successful poll into a report.
pub fn report_from_output(
    stdout: &str,
    previous: Option<&RawSample>,
    warn: &Thresholds,
    crit: &Thresholds,
) -> (RawSample, HealthReport) {
    let sample = health::parse_sample(stdout, now_secs());
    let report = health::build_report(&sample, previous, warn, crit);
    (sample, report)
}

/// Turn a failed ssh outcome into an unreachable report.
pub fn report_from_failure(outcome: &HostOutcome) -> HealthReport {
    let reason = if outcome.reason.is_empty() {
        failure_reason_fallback(outcome.status)
    } else {
        outcome.reason.clone()
    };
    health::unreachable_report(now_secs(), reason)
}

/// Mark cached reports stale when older than `limit`.
pub fn mark_stale(report: &mut HealthReport, now: f64, limit: Duration) {
    if !report.reachable {
        report.stale = true;
        return;
    }
    report.stale = now - report.checked_at > limit.as_secs_f64();
}

/// Count ok / warn / crit / unreachable among reports.
pub fn status_counts<'a>(
    reports: impl IntoIterator<Item = &'a HealthReport>,
) -> (usize, usize, usize, usize) {
    let mut ok = 0;
    let mut warn = 0;
    let mut crit = 0;
    let mut down = 0;
    for report in reports {
        match report.status {
            HealthStatus::Ok => ok += 1,
            HealthStatus::Warn => warn += 1,
            HealthStatus::Crit => crit += 1,
            HealthStatus::Unreachable => down += 1,
        }
    }
    (ok, warn, crit, down)
}

/// Run the background poll loop until `cancel` fires.
pub async fn run_scheduler<R: ExecRunner>(
    runner: Arc<R>,
    mut targets: Vec<HostTarget>,
    settings: HealthSettings,
    cancel: CancellationToken,
    mut control: mpsc::Receiver<HealthControl>,
    events: mpsc::Sender<HealthEvent>,
    mut cache: BTreeMap<String, HostHealth>,
) {
    let mut paused = false;
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    stagger_due(&mut cache, &targets, settings.refresh);

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            command = control.recv() => {
                match command {
                    None => break,
                    Some(HealthControl::Pause) => paused = true,
                    Some(HealthControl::Resume) => paused = false,
                    Some(HealthControl::Refresh(names)) => {
                        let now = Instant::now();
                        if names.is_empty() {
                            for host in cache.values_mut() {
                                host.next_due = Some(now);
                                host.backing_off = false;
                            }
                        } else {
                            for name in names {
                                let entry = cache.entry(name).or_default();
                                entry.next_due = Some(now);
                                entry.backing_off = false;
                            }
                        }
                    }
                    Some(HealthControl::SetHosts(next)) => {
                        targets = next;
                        stagger_due(&mut cache, &targets, settings.refresh);
                    }
                }
            }
            _ = tick.tick(), if !paused => {
                let due = due_names(&cache, &targets, Instant::now());
                if due.is_empty() {
                    continue;
                }
                let batch: Vec<HostTarget> = targets
                    .iter()
                    .filter(|target| due.iter().any(|name| name == &target.name))
                    .cloned()
                    .collect();
                if batch.is_empty() {
                    continue;
                }
                // Mark due hosts as in-flight so the next tick does not re-queue them.
                let inflight_until = Instant::now() + settings.refresh;
                for name in &due {
                    if let Some(entry) = cache.get_mut(name) {
                        entry.next_due = Some(inflight_until);
                    }
                }
                let results = run_poll_batch(
                    Arc::clone(&runner),
                    batch,
                    settings.parallel,
                    cancel.child_token(),
                )
                .await;
                for (host, (outcome, stdout)) in results {
                    let entry = cache.entry(host.clone()).or_default();
                    let (report, point) = if outcome.status == EndStatus::Ok {
                        let (sample, report) = report_from_output(
                            &stdout,
                            entry.previous.as_ref(),
                            &settings.warn,
                            &settings.crit,
                        );
                        entry.previous = Some(sample);
                        entry.backing_off = false;
                        entry.next_due = Some(Instant::now() + settings.refresh);
                        let point = history_point(&report);
                        entry.push_history(point, settings.history_samples);
                        (report, Some(point))
                    } else {
                        let report = report_from_failure(&outcome);
                        entry.backing_off = true;
                        entry.next_due = Some(Instant::now() + BACKOFF);
                        (report, None)
                    };
                    entry.report = Some(report.clone());
                    let _ = events
                        .send(HealthEvent {
                            host,
                            report,
                            point,
                        })
                        .await;
                }
            }
        }
    }
}

/// One-shot poll used by `vcon status`. When `second_sample` is true, waits one
/// second and polls again so CPU % and network rates are available.
pub async fn poll_once<R: ExecRunner>(
    runner: Arc<R>,
    targets: Vec<HostTarget>,
    settings: &HealthSettings,
    second_sample: bool,
    cancel: CancellationToken,
) -> BTreeMap<String, HealthReport> {
    let mut previous: BTreeMap<String, RawSample> = BTreeMap::new();
    let first = collect_poll(
        Arc::clone(&runner),
        targets.clone(),
        settings,
        &mut previous,
        cancel.child_token(),
    )
    .await;
    if !second_sample {
        return first;
    }
    sleep(Duration::from_secs(1)).await;
    if cancel.is_cancelled() {
        return first;
    }
    collect_poll(runner, targets, settings, &mut previous, cancel).await
}

async fn collect_poll<R: ExecRunner>(
    runner: Arc<R>,
    targets: Vec<HostTarget>,
    settings: &HealthSettings,
    previous: &mut BTreeMap<String, RawSample>,
    cancel: CancellationToken,
) -> BTreeMap<String, HealthReport> {
    let results = run_poll_batch(runner, targets, settings.parallel, cancel).await;
    let mut reports = BTreeMap::new();
    for (host, (outcome, stdout)) in results {
        let report = if outcome.status == EndStatus::Ok {
            let prior = previous.get(&host).cloned();
            let (sample, report) =
                report_from_output(&stdout, prior.as_ref(), &settings.warn, &settings.crit);
            previous.insert(host.clone(), sample);
            report
        } else {
            report_from_failure(&outcome)
        };
        reports.insert(host, report);
    }
    reports
}

async fn run_poll_batch<R: ExecRunner>(
    runner: Arc<R>,
    targets: Vec<HostTarget>,
    parallel: usize,
    cancel: CancellationToken,
) -> Vec<(String, (HostOutcome, String))> {
    let (tx, mut rx) = mpsc::channel(256);
    let join = tokio::spawn(async move {
        job::run_job(runner, targets, POLL_TIMEOUT, parallel, cancel, tx).await
    });
    let mut stdout: HashMap<String, String> = HashMap::new();
    let mut finished: Vec<(String, HostOutcome)> = Vec::new();
    while let Some(event) = rx.recv().await {
        match event {
            JobEvent::Line {
                host,
                stderr: false,
                text,
            } => {
                let body = stdout.entry(host).or_default();
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(&text);
            }
            JobEvent::HostFinished { host, outcome } => {
                finished.push((host, outcome));
            }
            JobEvent::JobFinished { .. } | JobEvent::HostStarted { .. } | JobEvent::Line { .. } => {
            }
        }
    }
    let _ = join.await;
    finished
        .into_iter()
        .map(|(host, outcome)| {
            let body = stdout.remove(&host).unwrap_or_default();
            (host, (outcome, body))
        })
        .collect()
}

fn thresholds(value: &HealthThresholds) -> Thresholds {
    Thresholds {
        disk: value.disk,
        mem: value.mem,
        load_per_cpu: value.load_per_cpu,
    }
}

fn stagger_due(
    cache: &mut BTreeMap<String, HostHealth>,
    targets: &[HostTarget],
    refresh: Duration,
) {
    let now = Instant::now();
    let n = targets.len().max(1);
    let step = refresh.as_millis() as u64 / n as u64;
    for (index, target) in targets.iter().enumerate() {
        let entry = cache.entry(target.name.clone()).or_default();
        if entry.next_due.is_none() {
            entry.next_due = Some(now + Duration::from_millis(step.saturating_mul(index as u64)));
        }
    }
    cache.retain(|name, _| targets.iter().any(|target| target.name == *name));
}

fn due_names(
    cache: &BTreeMap<String, HostHealth>,
    targets: &[HostTarget],
    now: Instant,
) -> Vec<String> {
    targets
        .iter()
        .filter_map(|target| {
            let due = cache
                .get(&target.name)
                .and_then(|host| host.next_due)
                .unwrap_or(now);
            (due <= now).then(|| target.name.clone())
        })
        .collect()
}

fn failure_reason_fallback(status: EndStatus) -> String {
    match status {
        EndStatus::Ok => "ok".to_string(),
        EndStatus::Failed => "failed".to_string(),
        EndStatus::Timeout => "connection timed out".to_string(),
        EndStatus::Cancelled => "cancelled".to_string(),
        EndStatus::Skipped => "skipped".to_string(),
        EndStatus::SpawnFailed => "could not start ssh".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::exec::ProcessResult;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scripted {
        body: String,
        fail: Vec<String>,
        calls: AtomicUsize,
    }

    impl ExecRunner for Scripted {
        async fn run(
            self: Arc<Self>,
            host: String,
            _args: Vec<String>,
            _timeout: Duration,
            _cancel: CancellationToken,
            lines: mpsc::Sender<JobEvent>,
        ) -> ProcessResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.iter().any(|name| name == &host) {
                return ProcessResult {
                    code: Some(255),
                    timed_out: false,
                    cancelled: false,
                    spawn_error: None,
                    truncated: false,
                    stderr: "Connection timed out".to_string(),
                };
            }
            for line in self.body.lines() {
                let _ = lines
                    .send(JobEvent::Line {
                        host: host.clone(),
                        stderr: false,
                        text: line.to_string(),
                    })
                    .await;
            }
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

    fn settings() -> HealthSettings {
        HealthSettings {
            refresh: Duration::from_secs(30),
            history_samples: 8,
            parallel: 2,
            warn: Thresholds {
                disk: 80.0,
                mem: 85.0,
                load_per_cpu: 1.0,
            },
            crit: Thresholds {
                disk: 90.0,
                mem: 95.0,
                load_per_cpu: 2.0,
            },
            program: PathBuf::from("ssh"),
        }
    }

    #[tokio::test]
    async fn poll_once_parses_script_output_and_marks_failures() {
        let body = "\
hostname=web-1
cpus=2
load1=0.20
mem_total_kb=1000
mem_available_kb=500
disk_mount=/
disk_total_kb=1000
disk_used_kb=400
disk_avail_kb=600
disk_used_pct=40
cpu_user=10
cpu_nice=0
cpu_system=0
cpu_idle=90
cpu_iowait=0
cpu_irq=0
cpu_softirq=0
cpu_steal=0
failed_units=0
reboot_required=0
";
        let runner = Arc::new(Scripted {
            body: body.to_string(),
            fail: vec!["db-1".to_string()],
            calls: AtomicUsize::new(0),
        });
        let targets = vec![
            HostTarget {
                name: "web-1".to_string(),
                args: vec!["ssh".to_string()],
            },
            HostTarget {
                name: "db-1".to_string(),
                args: vec!["ssh".to_string()],
            },
        ];
        let reports = poll_once(
            runner,
            targets,
            &settings(),
            false,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(reports["web-1"].status, HealthStatus::Ok);
        assert!((reports["web-1"].mem_pct.unwrap() - 50.0).abs() < 0.1);
        assert_eq!(reports["db-1"].status, HealthStatus::Unreachable);
        assert!(
            reports["db-1"]
                .error
                .as_deref()
                .unwrap()
                .contains("timed out")
        );
    }
}
