//! Local filesystem via `tokio::fs`.

use super::{DirEntry, EntryKind, RemoteFs, file_name};
use crate::error::Error;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::Mutex;

/// Write handles opened for a copy. Later chunks use the same fd so a path
/// swapped for a symlink after `create` is not followed.
fn write_handles() -> &'static Mutex<HashMap<String, File>> {
    static HANDLES: OnceLock<Mutex<HashMap<String, File>>> = OnceLock::new();
    HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Pane backed by the local disk.
#[derive(Debug, Default)]
pub struct LocalFs;

impl LocalFs {
    /// Expand `~` with [`shellexpand`] and normalize.
    pub fn expand(path: &str) -> PathBuf {
        PathBuf::from(shellexpand::tilde(path).as_ref())
    }
}

impl RemoteFs for LocalFs {
    async fn list(&self, dir: &str) -> Result<Vec<DirEntry>, Error> {
        let path = Self::expand(dir);
        let mut entries = Vec::new();
        let mut read = fs::read_dir(&path).await.map_err(|source| Error::Read {
            path: path.clone(),
            source,
        })?;
        while let Some(entry) = read.next_entry().await.map_err(|source| Error::Read {
            path: path.clone(),
            source,
        })? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "." || name == ".." {
                continue;
            }
            // Do not follow symlinks. `metadata` would turn a link to a
            // directory into a directory and a link to a file into its target.
            let meta = fs::symlink_metadata(entry.path())
                .await
                .map_err(|source| Error::Read {
                    path: entry.path(),
                    source,
                })?;
            entries.push(from_meta(name, &meta, None));
        }
        Ok(entries)
    }

    async fn stat(&self, path: &str) -> Result<DirEntry, Error> {
        let path = Self::expand(path);
        let meta = fs::symlink_metadata(&path)
            .await
            .map_err(|source| Error::Read {
                path: path.clone(),
                source,
            })?;
        let target = if meta.file_type().is_symlink() {
            fs::read_link(&path)
                .await
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        } else {
            None
        };
        Ok(from_meta(
            file_name(&path.to_string_lossy()).to_string(),
            &meta,
            target,
        ))
    }

    async fn mkdir(&self, path: &str) -> Result<(), Error> {
        let path = Self::expand(path);
        fs::create_dir(&path)
            .await
            .map_err(|source| Error::Write { path, source })
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let from = Self::expand(from);
        let to = Self::expand(to);
        fs::rename(&from, &to)
            .await
            .map_err(|source| Error::Write { path: to, source })
    }

    async fn remove_file(&self, path: &str) -> Result<(), Error> {
        let path = Self::expand(path);
        fs::remove_file(&path)
            .await
            .map_err(|source| Error::Write { path, source })
    }

    async fn remove_dir(&self, path: &str) -> Result<(), Error> {
        let path = Self::expand(path);
        fs::remove_dir(&path)
            .await
            .map_err(|source| Error::Write { path, source })
    }

    async fn realpath(&self, path: &str) -> Result<String, Error> {
        let expanded = Self::expand(path);
        match fs::canonicalize(&expanded).await {
            Ok(canonical) => Ok(canonical.to_string_lossy().into_owned()),
            Err(_) => {
                if let Some(parent) = expanded.parent() {
                    let parent = fs::canonicalize(parent)
                        .await
                        .map_err(|source| Error::Read {
                            path: parent.to_path_buf(),
                            source,
                        })?;
                    Ok(parent
                        .join(expanded.file_name().unwrap_or_default())
                        .to_string_lossy()
                        .into_owned())
                } else {
                    Err(Error::Message(format!(
                        "cannot resolve {}",
                        expanded.display()
                    )))
                }
            }
        }
    }

    async fn read_at(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        let path = Self::expand(path);
        let meta = fs::symlink_metadata(&path)
            .await
            .map_err(|source| Error::Read {
                path: path.clone(),
                source,
            })?;
        if !meta.file_type().is_file() {
            return Err(Error::Message(format!(
                "not a regular file: {}",
                path.display()
            )));
        }
        let mut file = File::open(&path).await.map_err(|source| Error::Read {
            path: path.clone(),
            source,
        })?;
        file.seek(SeekFrom::Start(offset))
            .await
            .map_err(|source| Error::Read {
                path: path.clone(),
                source,
            })?;
        let mut buf = vec![0_u8; len];
        let read = file.read(&mut buf).await.map_err(|source| Error::Read {
            path: path.clone(),
            source,
        })?;
        buf.truncate(read);
        Ok(buf)
    }

    async fn create(&self, path: &str) -> Result<(), Error> {
        let path = Self::expand(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|source| Error::Write {
                    path: parent.to_path_buf(),
                    source,
                })?;
        }
        let file = open_nofollow(&path, true).map_err(|source| Error::Write {
            path: path.clone(),
            source,
        })?;
        let key = path.to_string_lossy().into_owned();
        write_handles().lock().await.insert(key, file);
        Ok(())
    }

    async fn write_at(&self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        let path = Self::expand(path);
        let key = path.to_string_lossy().into_owned();
        let mut handles = write_handles().lock().await;
        if let Some(file) = handles.get_mut(&key) {
            file.seek(SeekFrom::Start(offset))
                .await
                .map_err(|source| Error::Write {
                    path: path.clone(),
                    source,
                })?;
            file.write_all(data).await.map_err(|source| Error::Write {
                path: path.clone(),
                source,
            })?;
            file.flush().await.map_err(|source| Error::Write {
                path: path.clone(),
                source,
            })?;
            return Ok(());
        }
        drop(handles);
        let mut file = open_nofollow(&path, false).map_err(|source| Error::Write {
            path: path.clone(),
            source,
        })?;
        file.seek(SeekFrom::Start(offset))
            .await
            .map_err(|source| Error::Write {
                path: path.clone(),
                source,
            })?;
        file.write_all(data).await.map_err(|source| Error::Write {
            path: path.clone(),
            source,
        })?;
        file.flush()
            .await
            .map_err(|source| Error::Write { path, source })?;
        Ok(())
    }

    async fn release(&self, path: &str) -> Result<(), Error> {
        let key = Self::expand(path).to_string_lossy().into_owned();
        write_handles().lock().await.remove(&key);
        Ok(())
    }

    async fn set_meta(
        &self,
        path: &str,
        mode: Option<u32>,
        mtime: Option<SystemTime>,
    ) -> Result<(), Error> {
        if mode.is_none() && mtime.is_none() {
            return Ok(());
        }
        let path = Self::expand(path);
        let key = path.to_string_lossy().into_owned();
        let mut handles = write_handles().lock().await;
        if let Some(file) = handles.get_mut(&key) {
            return apply_meta(file, &path, mode, mtime).await;
        }
        drop(handles);
        let mut file = open_nofollow(&path, false).map_err(|source| Error::Write {
            path: path.clone(),
            source,
        })?;
        apply_meta(&mut file, &path, mode, mtime).await
    }
}

async fn apply_meta(
    file: &mut File,
    path: &Path,
    mode: Option<u32>,
    mtime: Option<SystemTime>,
) -> Result<(), Error> {
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .await
            .map_err(|source| Error::Write {
                path: path.to_path_buf(),
                source,
            })?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    if let Some(mtime) = mtime {
        file.set_modified(mtime)
            .await
            .map_err(|source| Error::Write {
                path: path.to_path_buf(),
                source,
            })?;
    }
    Ok(())
}

/// Open the last component with `O_NOFOLLOW` from a directory fd.
///
/// The parent directory is allowed to be a symlink the user chose (for
/// example `/tmp`). A symlink swapped in as the file itself is not followed.
fn open_nofollow(path: &Path, create: bool) -> std::io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let Some(name) = path.file_name() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no file name",
        ));
    };
    let dir = match parent {
        Some(parent) => std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(parent)?,
        None => std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(".")?,
    };
    let mut c_name = name.as_bytes().to_vec();
    c_name.push(0);
    let mut flags = libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    if create {
        flags |= libc::O_CREAT | libc::O_TRUNC;
    }
    let fd = unsafe { libc::openat(dir.as_raw_fd(), c_name.as_ptr().cast(), flags, 0o644) };
    drop(dir);
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let std_file = unsafe { std::fs::File::from_raw_fd(fd) };
    Ok(File::from_std(std_file))
}

fn from_meta(name: String, meta: &std::fs::Metadata, target: Option<String>) -> DirEntry {
    let file_type = meta.file_type();
    let kind = if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode())
    };
    #[cfg(not(unix))]
    let mode = None;
    DirEntry {
        name,
        kind,
        size: meta.len(),
        mtime: meta.modified().ok(),
        mode,
        target,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lists_and_round_trips_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hello world").unwrap();
        let fs = LocalFs;
        let entries = fs.list(&dir.path().to_string_lossy()).await.unwrap();
        assert!(entries.iter().any(|entry| entry.name == "hello.txt"));
        let path = file.to_string_lossy().into_owned();
        let chunk = fs.read_at(&path, 0, 5).await.unwrap();
        assert_eq!(chunk, b"hello");
        let dest = dir.path().join("out.txt").to_string_lossy().into_owned();
        fs.create(&dest).await.unwrap();
        fs.write_at(&dest, 0, b"abc").await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"abc");
        fs.release(&dest).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_at_does_not_follow_a_symlink_swapped_in_after_create() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"SECRET").unwrap();
        let dest = dir.path().join("out.bin");
        let fs = LocalFs;
        let dest_str = dest.to_string_lossy().into_owned();
        fs.create(&dest_str).await.unwrap();
        std::fs::remove_file(&dest).unwrap();
        std::os::unix::fs::symlink(&outside, &dest).unwrap();
        let wrote = fs.write_at(&dest_str, 0, b"PWNED").await;
        assert_ne!(
            std::fs::read(&outside).unwrap(),
            b"PWNED",
            "write followed a symlink swapped in after create ({wrote:?})"
        );
        fs.release(&dest_str).await.unwrap();
    }
}
