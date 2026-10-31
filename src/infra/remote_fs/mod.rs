//! [`RemoteFs`] trait. v1 speaks SFTP over system `ssh`.

#[cfg(test)]
mod fake;
mod local;
mod path;
mod sftp;

#[cfg(test)]
pub use fake::FakeFs;
pub use local::LocalFs;
pub use path::{file_name, join, normalize, parent_of};
pub use sftp::{SftpFs, sftp_arguments};

use crate::error::Error;
use std::future::Future;
use std::time::SystemTime;

/// Kind of a directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link.
    Symlink,
    /// Socket, device, or other.
    Other,
}

/// One name in a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Basename only.
    pub name: String,
    /// File / dir / symlink / other.
    pub kind: EntryKind,
    /// Size in bytes. Directories may report 0.
    pub size: u64,
    /// Modification time when known.
    pub mtime: Option<SystemTime>,
    /// Unix mode bits when known.
    pub mode: Option<u32>,
    /// Symlink target when `kind` is [`EntryKind::Symlink`].
    pub target: Option<String>,
}

impl DirEntry {
    /// True for directories.
    ///
    /// Also treats Unix mode `S_IFDIR` as a directory when the SFTP server
    /// omits an explicit file type (otherwise Enter tries to "view" folders).
    pub fn is_dir(&self) -> bool {
        if self.kind == EntryKind::Dir {
            return true;
        }
        self.mode.is_some_and(|mode| mode & 0o170_000 == 0o040_000)
    }

    /// True for regular files (not dirs/symlinks).
    pub fn is_file(&self) -> bool {
        self.kind == EntryKind::File && !self.is_dir()
    }

    /// Whether Enter should navigate into this entry.
    pub fn is_enterable(&self) -> bool {
        self.is_dir()
            || self.kind == EntryKind::Symlink
            || self.mode.is_some_and(|mode| mode & 0o170_000 == 0o120_000)
    }
}

/// Browse and mutate a file tree. Paths are `/`-separated strings.
pub trait RemoteFs: Send + Sync {
    /// List one directory. `.` and `..` are omitted.
    fn list(&self, dir: &str) -> impl Future<Output = Result<Vec<DirEntry>, Error>> + Send;

    /// Metadata for one path.
    fn stat(&self, path: &str) -> impl Future<Output = Result<DirEntry, Error>> + Send;

    /// Create a directory.
    fn mkdir(&self, path: &str) -> impl Future<Output = Result<(), Error>> + Send;

    /// Rename or move within the same filesystem.
    fn rename(&self, from: &str, to: &str) -> impl Future<Output = Result<(), Error>> + Send;

    /// Remove a file or symlink.
    fn remove_file(&self, path: &str) -> impl Future<Output = Result<(), Error>> + Send;

    /// Remove an empty directory.
    fn remove_dir(&self, path: &str) -> impl Future<Output = Result<(), Error>> + Send;

    /// Resolve `~` / relative paths to an absolute remote path.
    fn realpath(&self, path: &str) -> impl Future<Output = Result<String, Error>> + Send;

    /// Read up to `len` bytes starting at `offset`.
    fn read_at(
        &self,
        path: &str,
        offset: u64,
        len: usize,
    ) -> impl Future<Output = Result<Vec<u8>, Error>> + Send;

    /// Create or truncate `path`, then write `data` at offset 0. Subsequent
    /// chunks use [`RemoteFs::write_at`].
    fn create(&self, path: &str) -> impl Future<Output = Result<(), Error>> + Send;

    /// Write `data` at `offset` (file must already exist).
    fn write_at(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
    ) -> impl Future<Output = Result<(), Error>> + Send;

    /// Apply mode and/or mtime when the backend supports it.
    fn set_meta(
        &self,
        path: &str,
        mode: Option<u32>,
        mtime: Option<SystemTime>,
    ) -> impl Future<Output = Result<(), Error>> + Send;

    /// Close a handle left open by repeated [`RemoteFs::read_at`] / [`RemoteFs::write_at`] calls.
    ///
    /// SFTP keeps one handle per path so a multi-megabyte upload is not an
    /// open/close round trip per chunk. Other backends do nothing.
    fn release(&self, path: &str) -> impl Future<Output = Result<(), Error>> + Send {
        let _ = path;
        async { Ok(()) }
    }
}

#[cfg(test)]
mod entry_tests {
    use super::*;

    #[test]
    fn mode_bits_mark_directories_enterable() {
        let dir = DirEntry {
            name: "etc".into(),
            kind: EntryKind::Other,
            size: 0,
            mtime: None,
            mode: Some(0o040_755),
            target: None,
        };
        assert!(dir.is_dir());
        assert!(dir.is_enterable());
        let file = DirEntry {
            name: "a".into(),
            kind: EntryKind::File,
            size: 1,
            mtime: None,
            mode: Some(0o100_644),
            target: None,
        };
        assert!(!file.is_dir());
        assert!(!file.is_enterable());
    }
}
