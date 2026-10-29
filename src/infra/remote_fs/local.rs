//! Local filesystem via `tokio::fs`.

use super::{DirEntry, EntryKind, RemoteFs, file_name};
use crate::error::Error;
use std::path::PathBuf;
use std::time::SystemTime;
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

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
            let meta = entry.metadata().await.map_err(|source| Error::Read {
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
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .await
            .map_err(|source| Error::Write {
                path: path.clone(),
                source,
            })?;
        Ok(())
    }

    async fn write_at(&self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        let path = Self::expand(path);
        let mut file = OpenOptions::new()
            .write(true)
            .open(&path)
            .await
            .map_err(|source| Error::Write {
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
        file.flush().await.map_err(|source| Error::Write {
            path: path.clone(),
            source,
        })?;
        file.sync_all().await.map_err(|source| Error::Write {
            path: path.clone(),
            source,
        })?;
        Ok(())
    }

    async fn set_meta(
        &self,
        path: &str,
        mode: Option<u32>,
        mtime: Option<SystemTime>,
    ) -> Result<(), Error> {
        let path = Self::expand(path);
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(mode);
            fs::set_permissions(&path, perms)
                .await
                .map_err(|source| Error::Write {
                    path: path.clone(),
                    source,
                })?;
        }
        if let Some(mtime) = mtime {
            let file = File::open(&path).await.map_err(|source| Error::Write {
                path: path.clone(),
                source,
            })?;
            file.set_modified(mtime)
                .await
                .map_err(|source| Error::Write {
                    path: path.clone(),
                    source,
                })?;
        }
        Ok(())
    }
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
    }
}
