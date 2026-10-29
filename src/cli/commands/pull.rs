//! `vcon pull`: download remote paths from one host over SFTP.

use crate::config::Config;
use crate::error::Error;
use crate::infra::remote_fs::{LocalFs, RemoteFs, SftpFs, normalize, parent_of, sftp_arguments};
use crate::paths::Paths;
use crate::services::files::{collect_tree, copy_file, map_destinations};
use crate::services::inventory::InventoryBundle;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Exit when any file fails.
pub const EXIT_SOME_FAILED: u8 = 3;

const SFTP_CONNECT_TIMEOUT: u64 = 30;

/// One `vcon pull` invocation.
pub struct PullRequest<'a> {
    /// Host name.
    pub host: &'a str,
    /// Remote file or directory.
    pub remote: &'a str,
    /// Local destination directory.
    pub local: &'a Path,
}

/// Download `remote` from `host` into local directory `local`.
pub fn run(
    bundle: &InventoryBundle,
    config: &Config,
    paths: &Paths,
    request: &PullRequest<'_>,
) -> Result<ExitCode, Error> {
    let Some(host) = bundle.hosts.iter().find(|host| host.name == request.host) else {
        return Err(Error::Message(format!("unknown host `{}`", request.host)));
    };
    let program = crate::infra::ssh::system::find_ssh()?;
    let local_dir = request
        .local
        .to_str()
        .ok_or_else(|| Error::Message("local path is not valid UTF-8".to_string()))?;
    let remote_source = normalize(request.remote);
    let local_dest = normalize(local_dir);
    let control = config
        .multiplexing
        .then(|| paths.control_dir().display().to_string());
    let args = sftp_arguments(
        host,
        &bundle.hosts,
        Some(SFTP_CONNECT_TIMEOUT),
        config.multiplexing,
        control.as_deref(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Message(error.to_string()))?;
    let failed = runtime.block_on(async {
        let remote = Arc::new(SftpFs::connect(program, args).await?);
        let meta = remote.stat(&remote_source).await?;
        let root = if meta.is_dir() {
            remote_source.clone()
        } else {
            parent_of(&remote_source).unwrap_or_else(|| "/".to_string())
        };
        let entries = collect_tree(remote.as_ref(), &remote_source).await?;
        let mapped = map_destinations(&entries, &root, &local_dest);
        let cancel = CancellationToken::new();
        let mut any_failed = false;
        for (src_path, destination, _size) in mapped {
            if copy_file(
                remote.as_ref(),
                &LocalFs,
                &src_path,
                &destination,
                &cancel,
                |_, _| {},
            )
            .await
            .is_err()
            {
                any_failed = true;
                eprintln!("failed: {src_path} → {destination}");
            }
        }
        if let Ok(sftp) = Arc::try_unwrap(remote) {
            let _ = sftp.close().await;
        }
        Ok::<bool, Error>(any_failed)
    })?;
    if failed {
        Ok(ExitCode::from(EXIT_SOME_FAILED))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}
