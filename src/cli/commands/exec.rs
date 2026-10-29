//! `vcon exec`: run one command on every host a selector matches.
//!
//! Grouped output is the default: each host's text is printed when that host
//! finishes, and a progress count is written to stderr while the job runs.
//! `--stream` prints lines as they arrive. The process exits 3 when any host
//! fails.

use crate::config::Config;
use crate::domain::host::EffectiveHost;
use crate::domain::selector::{evaluate_selector, parse_selector};
use crate::error::Error;
use crate::infra::ssh::command::{command_line, exec_arguments};
use crate::infra::ssh::system::SystemSsh;
use crate::paths::Paths;
use crate::services::exec::{self, ConfirmGate, DEFAULT_TIMEOUT, EndStatus, HostTarget, JobEvent};
use crate::services::inventory::InventoryBundle;
use serde::Serialize;
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Exit status when at least one host failed, timed out, or was cancelled.
pub const EXIT_SOME_FAILED: u8 = 3;

/// Options for one `vcon exec` invocation.
pub struct ExecRequest<'a> {
    /// Selector text, for example `@web`.
    pub selector: &'a str,
    /// Remote command pieces. They are joined with spaces.
    pub command: &'a [String],
    /// Overrides `config.parallel` when set.
    pub parallel: Option<usize>,
    /// Per-host timeout in seconds. `Some(0)` waits without a limit.
    pub timeout: Option<u64>,
    /// Print lines as they arrive instead of grouping them.
    pub stream: bool,
    /// Rejected when combined with `stream`. Grouped is already the default.
    pub grouped: bool,
    /// Print only hosts that did not succeed.
    pub only_failed: bool,
    /// Print the ssh command and do not run it.
    pub dry_run: bool,
    /// Prefix the command with `sudo -n`.
    pub sudo: bool,
    /// Emit one JSON object instead of text.
    pub json: bool,
    /// Skip the confirmation prompt.
    pub yes: bool,
}

/// Run the command described by `request`.
pub fn run(
    bundle: &InventoryBundle,
    config: &Config,
    paths: &Paths,
    request: &ExecRequest<'_>,
) -> Result<ExitCode, Error> {
    if request.stream && request.grouped {
        return Err(Error::Message(
            "--stream and --grouped cannot both be set".to_string(),
        ));
    }
    let command = request.command.join(" ");
    let command = command.trim();
    if command.is_empty() {
        return Err(Error::Message(
            "missing command; pass it after --".to_string(),
        ));
    }
    let terms = parse_selector(request.selector).map_err(Error::Message)?;
    let selection = evaluate_selector(&terms, &bundle.hosts);
    if let Some(error) = &selection.error {
        return Err(Error::Message(error.clone()));
    }
    for warning in &selection.warnings {
        eprintln!("{warning}");
    }
    let hosts: Vec<EffectiveHost> = selection.hosts.into_iter().cloned().collect();
    let remote = exec::remote_command(command, request.sudo);
    let timeout = request
        .timeout
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TIMEOUT);
    let connect_timeout = (timeout.as_secs() > 0).then_some(timeout.as_secs());
    let control = paths.control_dir();
    let targets: Vec<HostTarget> = hosts
        .iter()
        .map(|host| HostTarget {
            name: host.name.clone(),
            args: exec_arguments(
                host,
                &bundle.hosts,
                connect_timeout,
                config.multiplexing,
                Some(&control.display().to_string()),
                &remote,
            ),
        })
        .collect();
    if request.dry_run {
        for target in &targets {
            println!(
                "{}  {}",
                target.name,
                command_line(&argv_with_program("ssh", &target.args))
            );
        }
        println!("{} hosts, command not run", targets.len());
        return Ok(ExitCode::SUCCESS);
    }
    let needed = exec::needs_confirmation(
        &hosts,
        &exec::ExecSettings {
            confirm_threshold: config.confirm_threshold,
            protected_tags: config.protected_tags.clone(),
        },
    );
    match exec::confirmation_gate(needed.is_some(), request.yes, io::stdin().is_terminal()) {
        ConfirmGate::Refuse => {
            return Err(Error::Message(format!(
                "confirmation required ({}); pass --yes",
                needed.unwrap().message()
            )));
        }
        ConfirmGate::Ask => {
            let reason = needed.unwrap().message();
            if !prompt(&remote, &reason)? {
                return Err(Error::Message("exec cancelled".to_string()));
            }
        }
        ConfirmGate::Run => {}
    }
    let parallel = request.parallel.unwrap_or(config.parallel);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Message(error.to_string()))?;
    let summary = runtime.block_on(drive(
        &targets,
        timeout,
        parallel,
        request.stream,
        request.only_failed,
        request.json,
        &remote,
    ))?;
    if summary.failed > 0 {
        Ok(ExitCode::from(EXIT_SOME_FAILED))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn argv_with_program(program: &str, args: &[String]) -> Vec<String> {
    let mut argv = vec![program.to_string()];
    argv.extend(args.iter().cloned());
    argv
}

fn prompt(command: &str, reason: &str) -> Result<bool, Error> {
    eprint!("Run `{command}` on {reason}? [y/N] ");
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|error| Error::Message(error.to_string()))?;
    let answer = line.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

struct HostCapture {
    name: String,
    lines: Vec<(bool, String)>,
    outcome: Option<exec::HostOutcome>,
}

async fn drive(
    targets: &[HostTarget],
    timeout: Duration,
    parallel: usize,
    stream: bool,
    only_failed: bool,
    json: bool,
    command: &str,
) -> Result<exec::Summary, Error> {
    let program = crate::infra::ssh::system::find_ssh()?;
    let (tx, mut rx) = mpsc::channel(128);
    let runner = Arc::new(SystemSsh::new(program));
    let targets = targets.to_vec();
    let total = targets.len();
    let job = tokio::spawn(async move {
        exec::run_job(
            runner,
            targets,
            timeout,
            parallel,
            CancellationToken::new(),
            tx,
        )
        .await
    });
    let mut hosts: Vec<HostCapture> = Vec::new();
    let mut summary = exec::Summary {
        ok: 0,
        failed: 0,
        skipped: 0,
        duration: Duration::ZERO,
    };
    let mut finished = 0usize;
    while let Some(event) = rx.recv().await {
        match event {
            JobEvent::HostStarted { host } => {
                if !json && !stream {
                    eprint!("\r{finished}/{total} {host}   ");
                    let _ = io::stderr().flush();
                }
            }
            JobEvent::Line { host, stderr, text } => {
                if stream && !json {
                    if stderr {
                        println!("[{host}] ! {text}");
                    } else {
                        println!("[{host}] {text}");
                    }
                }
                capture(&mut hosts, &host).lines.push((stderr, text));
            }
            JobEvent::HostFinished { host, outcome } => {
                finished += 1;
                if !json && !stream {
                    eprint!("\r{finished}/{total} {host}   ");
                    let _ = io::stderr().flush();
                }
                let show = !only_failed || outcome.status != EndStatus::Ok;
                if !json && !stream && show {
                    eprintln!();
                    print!(
                        "{}",
                        format_host(&host, &outcome, &capture(&mut hosts, &host).lines)
                    );
                }
                capture(&mut hosts, &host).outcome = Some(outcome);
            }
            JobEvent::JobFinished { summary: done } => summary = done,
        }
    }
    job.await
        .map_err(|error| Error::Message(error.to_string()))?;
    if !json && !stream {
        eprintln!();
    }
    if json {
        println!(
            "{}",
            serde_json::to_string(&json_report(command, &summary, &hosts))
                .map_err(|error| Error::Message(error.to_string()))?
        );
    } else {
        println!("{}", format_summary(&summary, &hosts));
    }
    Ok(summary)
}

fn capture<'a>(hosts: &'a mut Vec<HostCapture>, name: &str) -> &'a mut HostCapture {
    if let Some(index) = hosts.iter().position(|host| host.name == name) {
        return &mut hosts[index];
    }
    hosts.push(HostCapture {
        name: name.to_string(),
        lines: Vec::new(),
        outcome: None,
    });
    hosts.last_mut().unwrap()
}

fn format_host(name: &str, outcome: &exec::HostOutcome, lines: &[(bool, String)]) -> String {
    let mut text = format!(
        "── {name} ─ {} ─ {}\n",
        outcome.reason,
        format_duration(outcome.duration)
    );
    for (stderr, line) in lines {
        if *stderr {
            text.push_str("! ");
        }
        text.push_str(line);
        text.push('\n');
    }
    text.push('\n');
    text
}

fn format_summary(summary: &exec::Summary, hosts: &[HostCapture]) -> String {
    let failed: Vec<&str> = hosts
        .iter()
        .filter(|host| {
            host.outcome
                .as_ref()
                .is_some_and(|outcome| outcome.status != EndStatus::Ok)
        })
        .map(|host| host.name.as_str())
        .collect();
    if failed.is_empty() {
        format!(
            "{} hosts: {} ok",
            summary.ok + summary.failed + summary.skipped,
            summary.ok
        )
    } else {
        format!(
            "{} hosts: {} ok, {} failed ({})",
            summary.ok + summary.failed + summary.skipped,
            summary.ok,
            summary.failed + summary.skipped,
            failed.join(", ")
        )
    }
}

fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 10_000 {
        format!("{:.1}s", millis as f64 / 1000.0)
    } else {
        format!("{}s", duration.as_secs())
    }
}

#[derive(Serialize)]
struct JsonReport<'a> {
    command: &'a str,
    ok: usize,
    failed: usize,
    skipped: usize,
    hosts: Vec<JsonHost>,
}

#[derive(Serialize)]
struct JsonHost {
    name: String,
    status: String,
    code: Option<i32>,
    reason: String,
    duration_ms: u128,
    stdout: String,
    stderr: String,
}

fn json_report<'a>(
    command: &'a str,
    summary: &exec::Summary,
    hosts: &[HostCapture],
) -> JsonReport<'a> {
    JsonReport {
        command,
        ok: summary.ok,
        failed: summary.failed,
        skipped: summary.skipped,
        hosts: hosts
            .iter()
            .map(|host| {
                let outcome = host.outcome.clone().unwrap_or(exec::HostOutcome {
                    status: EndStatus::Skipped,
                    code: None,
                    reason: "skipped".to_string(),
                    duration: Duration::ZERO,
                    truncated: false,
                });
                let mut stdout = String::new();
                let mut stderr = String::new();
                for (is_err, line) in &host.lines {
                    let bucket = if *is_err { &mut stderr } else { &mut stdout };
                    bucket.push_str(line);
                    bucket.push('\n');
                }
                JsonHost {
                    name: host.name.clone(),
                    status: status_name(outcome.status).to_string(),
                    code: outcome.code,
                    reason: outcome.reason,
                    duration_ms: outcome.duration.as_millis(),
                    stdout,
                    stderr,
                }
            })
            .collect(),
    }
}

fn status_name(status: EndStatus) -> &'static str {
    match status {
        EndStatus::Ok => "ok",
        EndStatus::Failed => "failed",
        EndStatus::Timeout => "timeout",
        EndStatus::Cancelled => "cancelled",
        EndStatus::Skipped => "skipped",
        EndStatus::SpawnFailed => "spawn_failed",
    }
}

/// Dry-run text for tests. Does not spawn ssh.
#[cfg(test)]
fn dry_run_lines(hosts: &[EffectiveHost], remote: &str) -> Vec<String> {
    hosts
        .iter()
        .map(|host| {
            let args = exec_arguments(host, hosts, Some(30), false, None, remote);
            format!(
                "{}  {}",
                host.name,
                command_line(&argv_with_program("ssh", &args))
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str) -> EffectiveHost {
        EffectiveHost {
            name: name.to_string(),
            address: "203.0.113.10".to_string(),
            user: "deploy".to_string(),
            port: 22,
            identity_file: None,
            jump: None,
            tags: vec!["web".to_string()],
            notes: None,
            ssh_options: std::collections::BTreeMap::new(),
            disabled: false,
            connect_timeout: None,
            tunnels: Vec::new(),
        }
    }

    #[test]
    fn dry_run_shows_the_command_as_one_argument() {
        let hosts = vec![host("web-1"), host("web-2")];
        let lines = dry_run_lines(&hosts, "echo '$HOME' && true");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("web-1"));
        assert!(lines[0].contains("&& true"), "{}", lines[0]);
        assert!(lines[0].contains("BatchMode=yes"), "{}", lines[0]);
    }

    #[test]
    fn grouped_summary_names_the_failed_host() {
        let hosts = vec![HostCapture {
            name: "staging-1".to_string(),
            lines: vec![(false, "inactive".to_string())],
            outcome: Some(exec::HostOutcome {
                status: EndStatus::Failed,
                code: Some(3),
                reason: "exit 3".to_string(),
                duration: Duration::from_millis(300),
                truncated: false,
            }),
        }];
        let summary = exec::Summary {
            ok: 3,
            failed: 1,
            skipped: 0,
            duration: Duration::from_secs(1),
        };
        let text = format_host(
            "staging-1",
            hosts[0].outcome.as_ref().unwrap(),
            &hosts[0].lines,
        );
        assert!(text.contains("staging-1"), "{text}");
        assert!(text.contains("exit 3"), "{text}");
        assert!(text.contains("inactive"), "{text}");
        let summary = format_summary(&summary, &hosts);
        assert!(summary.contains("3 ok, 1 failed (staging-1)"), "{summary}");
    }
}
