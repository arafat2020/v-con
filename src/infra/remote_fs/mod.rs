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
    /// True for directories (symlink-to-dir still reports as symlink).
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }

    /// True for regular files.
    pub fn is_file(&self) -> bool {
        self.kind == EntryKind::File
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
}
