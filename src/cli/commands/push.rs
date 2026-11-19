//! `vcon push`: upload local paths to many hosts over SFTP.

use crate::config::Config;
use crate::domain::selector::{evaluate_selector, parse_selector};
use crate::error::Error;
use crate::infra::remote_fs::{
    LocalFs, SFTP_CONNECT_TIMEOUT_SECS, SftpFs, normalize, sftp_arguments_auth,
};
use crate::infra::secrets::{self, AskpassMode};
use crate::paths::Paths;
use crate::services::exec::{ConfirmGate, ExecSettings, confirmation_gate, needs_confirmation};
use crate::services::files::{copy_file, plan_push};
use crate::services::inventory::InventoryBundle;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Exit when any host or file fails.
pub const EXIT_SOME_FAILED: u8 = 3;

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
    let store = secrets::system_store();
    let interactive = io::stdin().is_terminal();
    for host in &hosts {
        crate::services::login::prepare_host_auth(
            &crate::services::login::Prepare {
                store: store.as_ref(),
                cache_dir: &paths.cache_dir,
                program: &program,
                host,
                hosts: &bundle.hosts,
                interactive,
                multiplexing: config.multiplexing,
                control_dir: &paths.control_dir(),
            },
            &crate::services::login::TtyPrompt,
        )?;
    }
    let local_path = request
        .local
        .to_str()
        .ok_or_else(|| Error::Message("local path is not valid UTF-8".to_string()))?;
    let control = config
        .multiplexing
        .then(|| paths.control_dir().display().to_string());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Message(error.to_string()))?;
    let mut failed = false;
    for host in &hosts {
        let started = std::time::Instant::now();
        let auth = secrets::saved_auth(store.as_ref(), &paths.cache_dir, host, AskpassMode::Force);
        let args = sftp_arguments_auth(
            host,
            &bundle.hosts,
            Some(SFTP_CONNECT_TIMEOUT_SECS),
            config.multiplexing,
            control.as_deref(),
            auth.password_auth,
        );
        let result = runtime.block_on(async {
            let remote =
                Arc::new(SftpFs::connect_with_env(program.clone(), args, &auth.env).await?);
            let source = normalize(local_path);
            let planned = plan_push(&LocalFs, remote.as_ref(), &source, request.remote).await?;
            let cancel = CancellationToken::new();
            for (src_path, destination, _size) in planned.files {
                if let Err(error) = copy_file(
                    &LocalFs,
                    remote.as_ref(),
                    &src_path,
                    &destination,
                    &cancel,
                    |_, _| {},
                )
                .await
                {
                    return Err(Error::Message(format!(
                        "copy failed for {destination}: {error}"
                    )));
                }
            }
            if let Ok(sftp) = Arc::try_unwrap(remote) {
                let _ = sftp.close().await;
            }
            Ok::<(), Error>(())
        });
        let exit_code = match &result {
            Ok(()) => 0,
            Err(error) => error.remote_code().unwrap_or(1),
        };
        crate::services::audit::record_transfer(
            &paths.audit_file(),
            &config.audit,
            crate::services::audit::TransferAudit {
                direction: "upload".into(),
                selector: Some(request.selector.to_string()),
                host: host.name.clone(),
                src: local_path.to_string(),
                dst: request.remote.to_string(),
                exit_code,
                duration_ms: started.elapsed().as_millis() as u64,
            },
        );
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
