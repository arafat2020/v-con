//! SFTP client driven by a system `ssh -s sftp` process.

use super::{DirEntry, EntryKind, RemoteFs, file_name};
use crate::domain::host::EffectiveHost;
use crate::error::Error;
use crate::infra::ssh::command::{SshCommandOptions, build_ssh_command};
use bytes::BytesMut;
use futures::StreamExt;
use openssh_sftp_client::metadata::{MetaData, MetaDataBuilder, Permissions};
use openssh_sftp_client::{Sftp, SftpOptions, UnixTimeStamp};
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::time::SystemTime;
use tokio::io::AsyncSeekExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// Build ssh argv (without the binary) that starts the SFTP subsystem.
pub fn sftp_arguments(
    host: &EffectiveHost,
    hosts: &[EffectiveHost],
    connect_timeout: Option<u64>,
    multiplexing: bool,
    control_dir: Option<&str>,
) -> Vec<String> {
    let options = SshCommandOptions {
        batch_mode: true,
        connect_timeout,
        multiplexing,
        control_path_dir: control_dir.map(str::to_string),
    };
    let mut args = build_ssh_command(host, hosts, &options);
    args.remove(0);
    args.push("-s".to_string());
    args.push("sftp".to_string());
    args
}

/// One SFTP session over a child `ssh`.
pub struct SftpFs {
    sftp: Mutex<Sftp>,
    child: Mutex<Child>,
}

impl SftpFs {
    /// Spawn `program` with `args` (already including `-s sftp`) and handshake.
    pub async fn connect(program: PathBuf, args: Vec<String>) -> Result<Self, Error> {
        let mut child = Command::new(&program)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| {
                Error::Message(format!(
                    "could not start ssh sftp ({}): {source}",
                    program.display()
                ))
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Message("ssh sftp stdin missing".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Message("ssh sftp stdout missing".to_string()))?;
        let sftp = Sftp::new(stdin, stdout, SftpOptions::new())
            .await
            .map_err(|error| Error::Message(format!("sftp handshake failed: {error}")))?;
        Ok(Self {
            sftp: Mutex::new(sftp),
            child: Mutex::new(child),
        })
    }

    /// Close the SFTP channel and wait for ssh to exit.
    pub async fn close(self) -> Result<(), Error> {
        let Self { sftp, child } = self;
        let sftp = sftp.into_inner();
        sftp.close()
            .await
            .map_err(|error| Error::Message(format!("sftp close failed: {error}")))?;
        let mut child = child.into_inner();
        let _ = child.kill().await;
        Ok(())
    }
}

impl RemoteFs for SftpFs {
    async fn list(&self, dir: &str) -> Result<Vec<DirEntry>, Error> {
        let sftp = self.sftp.lock().await;
        let mut fs = sftp.fs();
        let dir_handle = fs
            .open_dir(Path::new(dir))
            .await
            .map_err(|error| sftp_err("list", dir, error))?;
        let mut read_dir = pin!(dir_handle.read_dir());
        let mut entries = Vec::new();
        while let Some(entry) = read_dir.next().await {
            let entry = entry.map_err(|error| sftp_err("list", dir, error))?;
            let name = entry.filename().to_string_lossy().into_owned();
            if name == "." || name == ".." {
                continue;
            }
            entries.push(from_meta(name, entry.metadata(), None));
        }
        Ok(entries)
    }

    async fn stat(&self, path: &str) -> Result<DirEntry, Error> {
        let sftp = self.sftp.lock().await;
        let mut fs = sftp.fs();
        let meta = fs
            .symlink_metadata(Path::new(path))
            .await
            .map_err(|error| sftp_err("stat", path, error))?;
        let target = if meta.file_type().is_some_and(|ft| ft.is_symlink()) {
            fs.read_link(Path::new(path))
                .await
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        } else {
            None
        };
        Ok(from_meta(file_name(path).to_string(), meta, target))
    }

    async fn mkdir(&self, path: &str) -> Result<(), Error> {
        let sftp = self.sftp.lock().await;
        sftp.fs()
            .create_dir(Path::new(path))
            .await
            .map_err(|error| sftp_err("mkdir", path, error))
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let sftp = self.sftp.lock().await;
        sftp.fs()
            .rename(Path::new(from), Path::new(to))
            .await
            .map_err(|error| sftp_err("rename", from, error))
    }

    async fn remove_file(&self, path: &str) -> Result<(), Error> {
        let sftp = self.sftp.lock().await;
        sftp.fs()
            .remove_file(Path::new(path))
            .await
            .map_err(|error| sftp_err("remove", path, error))
    }

    async fn remove_dir(&self, path: &str) -> Result<(), Error> {
        let sftp = self.sftp.lock().await;
        sftp.fs()
            .remove_dir(Path::new(path))
            .await
            .map_err(|error| sftp_err("rmdir", path, error))
    }

    async fn realpath(&self, path: &str) -> Result<String, Error> {
        let sftp = self.sftp.lock().await;
        let resolved = sftp
            .fs()
            .canonicalize(Path::new(path))
            .await
            .map_err(|error| sftp_err("realpath", path, error))?;
        Ok(resolved.to_string_lossy().into_owned())
    }

    async fn read_at(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        let sftp = self.sftp.lock().await;
        let mut file = sftp
            .open(Path::new(path))
            .await
            .map_err(|error| sftp_err("open", path, error))?;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|source| Error::Message(format!("sftp seek {path}: {source}")))?;
        let n = u32::try_from(len).unwrap_or(u32::MAX);
        let buf = BytesMut::with_capacity(len.min(n as usize));
        match file
            .read(n, buf)
            .await
            .map_err(|error| sftp_err("read", path, error))?
        {
            Some(bytes) => Ok(bytes.to_vec()),
            None => Ok(Vec::new()),
        }
    }

    async fn create(&self, path: &str) -> Result<(), Error> {
        let sftp = self.sftp.lock().await;
        let file = sftp
            .create(Path::new(path))
            .await
            .map_err(|error| sftp_err("create", path, error))?;
        drop(file);
        Ok(())
    }

    async fn write_at(&self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        let sftp = self.sftp.lock().await;
        let mut file = sftp
            .options()
            .write(true)
            .open(Path::new(path))
            .await
            .map_err(|error| sftp_err("open", path, error))?;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|source| Error::Message(format!("sftp seek {path}: {source}")))?;
        file.write_all(data)
            .await
            .map_err(|error| sftp_err("write", path, error))?;
        Ok(())
    }

    async fn set_meta(
        &self,
        path: &str,
        mode: Option<u32>,
        mtime: Option<SystemTime>,
    ) -> Result<(), Error> {
        let sftp = self.sftp.lock().await;
        let mut fs = sftp.fs();
        if let Some(mode) = mode {
            let perms = Permissions::from(mode as u16);
            fs.set_permissions(Path::new(path), perms)
                .await
                .map_err(|error| sftp_err("chmod", path, error))?;
        }
        if let Some(mtime) = mtime {
            let stamp = UnixTimeStamp::new(mtime).unwrap_or_else(|_| UnixTimeStamp::unix_epoch());
            let mut builder = MetaDataBuilder::new();
            builder.time(stamp, stamp);
            fs.set_metadata(Path::new(path), builder.create())
                .await
                .map_err(|error| sftp_err("utime", path, error))?;
        }
        Ok(())
    }
}

fn sftp_err(op: &str, path: &str, error: openssh_sftp_client::Error) -> Error {
    Error::Message(format!("sftp {op} {path}: {error}"))
}

fn from_meta(name: String, meta: MetaData, target: Option<String>) -> DirEntry {
    let mode = meta.permissions().map(|perms| perms.as_raw().bits());
    let kind = match meta.file_type() {
        Some(ft) if ft.is_dir() => EntryKind::Dir,
        Some(ft) if ft.is_symlink() => EntryKind::Symlink,
        Some(ft) if ft.is_file() => EntryKind::File,
        Some(_) => EntryKind::Other,
        None => match mode.map(|m| m & 0o170_000) {
            Some(0o040_000) => EntryKind::Dir,
            Some(0o120_000) => EntryKind::Symlink,
            Some(0o100_000) => EntryKind::File,
            _ => EntryKind::Other,
        },
    };
    let mtime = meta.modified().map(|stamp| stamp.as_system_time());
    DirEntry {
        name,
        kind,
        size: meta.len().unwrap_or(0),
        mtime,
        mode,
        target,
    }
}
