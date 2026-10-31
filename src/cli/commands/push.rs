//! `vcon push`: upload local paths to many hosts over SFTP.

use crate::config::Config;
use crate::domain::selector::{evaluate_selector, parse_selector};
use crate::error::Error;
use crate::infra::remote_fs::{LocalFs, SftpFs, normalize, sftp_arguments};
use crate::paths::Paths;
use crate::services::exec::{ConfirmGate, ExecSettings, confirmation_gate, needs_confirmation};
use crate::services::files::{copy_file, plan_copies};
use crate::services::inventory::InventoryBundle;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Exit when any host or file fails.
pub const EXIT_SOME_FAILED: u8 = 3;

const SFTP_CONNECT_TIMEOUT: u64 = 30;

/// One `vcon push` invocation.
pub struct PushRequest<'a> {
    /// Host selector.
    pub selector: &'a str,
    /// Local file or directory.
    pub local: &'a Path,
    /// Remote destination directory.
    pub remote: &'a str,
    /// Skip confirmation prompts.
    pub yes: bool,
}

/// Upload `local` to `remote` on every host matched by `selector`.
pub fn run(
    bundle: &InventoryBundle,
    config: &Config,
    paths: &Paths,
    request: &PushRequest<'_>,
) -> Result<ExitCode, Error> {
    let terms = parse_selector(request.selector).map_err(Error::Message)?;
    let selection = evaluate_selector(&terms, &bundle.hosts);
    if let Some(error) = &selection.error {
        return Err(Error::Message(error.clone()));
    }
    for warning in &selection.warnings {
        eprintln!("{warning}");
    }
    let hosts: Vec<_> = selection.hosts.into_iter().cloned().collect();
    if hosts.is_empty() {
        return Err(Error::Message("selector matched no hosts".to_string()));
    }
    let needed = needs_confirmation(
        &hosts,
        &ExecSettings {
            confirm_threshold: config.confirm_threshold,
            protected_tags: config.protected_tags.clone(),
        },
    );
    match confirmation_gate(needed.is_some(), request.yes, io::stdin().is_terminal()) {
        ConfirmGate::Refuse => {
            return Err(Error::Message(format!(
                "confirmation required ({}); pass --yes",
                needed.unwrap().message()
            )));
        }
        ConfirmGate::Ask => {
            if !confirm_prompt(&format!(
                "push {} to {} on {}",
                request.local.display(),
                request.remote,
                needed.unwrap().message()
            ))? {
                return Err(Error::Message("push cancelled".to_string()));
            }
        }
        ConfirmGate::Run => {}
    }
    let program = crate::infra::ssh::system::find_ssh()?;
    let local_path = request
        .local
        .to_str()
        .ok_or_else(|| Error::Message("local path is not valid UTF-8".to_string()))?;
    let remote_dir = normalize(request.remote);
    let control = config
        .multiplexing
        .then(|| paths.control_dir().display().to_string());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Message(error.to_string()))?;
    let mut failed = false;
    for host in &hosts {
        let args = sftp_arguments(
            host,
            &bundle.hosts,
            Some(SFTP_CONNECT_TIMEOUT),
            config.multiplexing,
            control.as_deref(),
        );
        let result = runtime.block_on(async {
            let remote = Arc::new(SftpFs::connect(program.clone(), args).await?);
            let source = normalize(local_path);
            let planned = plan_copies(&LocalFs, remote.as_ref(), &source, &remote_dir).await?;
            let cancel = CancellationToken::new();
            for (src_path, destination, _size) in planned.files {
                if copy_file(
                    &LocalFs,
                    remote.as_ref(),
                    &src_path,
                    &destination,
                    &cancel,
                    |_, _| {},
                )
                .await
                .is_err()
                {
                    return Err(Error::Message(format!(
                        "{}: copy failed for {src_path}",
                        host.name
                    )));
                }
            }
            if let Ok(sftp) = Arc::try_unwrap(remote) {
                let _ = sftp.close().await;
            }
            Ok::<(), Error>(())
        });
        match result {
            Ok(()) => println!("{} ok", host.name),
            Err(error) => {
                failed = true;
                eprintln!("{}: {error}", host.name);
            }
        }
    }
    if failed {
        Ok(ExitCode::from(EXIT_SOME_FAILED))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn confirm_prompt(reason: &str) -> Result<bool, Error> {
    eprint!("{reason}? [y/N] ");
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|error| Error::Message(error.to_string()))?;
    Ok(line.trim().eq_ignore_ascii_case("y") || line.trim().eq_ignore_ascii_case("yes"))
}
