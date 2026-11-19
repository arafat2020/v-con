//! Browse directories and queue file transfers.

use crate::error::Error;
use crate::infra::remote_fs::{
    DirEntry, EntryKind, RemoteFs, file_name, join, normalize, parent_of,
};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Weak};
use std::time::{Instant, SystemTime};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

/// Bytes read/written per transfer chunk.
///
/// Large enough for one SFTP write on a typical OpenSSH server, so a VPS
/// upload is not capped at one tiny packet per round trip.
pub const CHUNK: usize = 256 * 1024;

/// Max concurrent transfers.
pub const PARALLEL: usize = 2;

/// How directory listings are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    /// Directories first, then name.
    Name,
    /// Directories first, then size.
    Size,
    /// Directories first, then mtime.
    Mtime,
}

impl SortKey {
    /// Cycle to the next key.
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::Size,
            Self::Size => Self::Mtime,
            Self::Mtime => Self::Name,
        }
    }

    /// Short label for the status line.
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Size => "size",
            Self::Mtime => "mtime",
        }
    }
}

/// Sort and optionally hide dotfiles.
pub fn prepare_listing(
    mut entries: Vec<DirEntry>,
    sort: SortKey,
    show_hidden: bool,
) -> Vec<DirEntry> {
    if !show_hidden {
        entries.retain(|entry| !entry.name.starts_with('.'));
    }
    entries.sort_by(|left, right| {
        let dir_order = right.is_dir().cmp(&left.is_dir());
        if dir_order != std::cmp::Ordering::Equal {
            return dir_order;
        }
        match sort {
            SortKey::Name => left.name.cmp(&right.name),
            SortKey::Size => left
                .size
                .cmp(&right.size)
                .then_with(|| left.name.cmp(&right.name)),
            SortKey::Mtime => left
                .mtime
                .cmp(&right.mtime)
                .then_with(|| left.name.cmp(&right.name)),
        }
    });
    entries
}

/// Direction of a transfer relative to the local pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// Local → remote.
    Upload,
    /// Remote → local.
    Download,
}

/// What to do when the destination already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictAction {
    /// Replace the destination.
    Overwrite,
    /// Leave the destination alone.
    Skip,
    /// Overwrite this and later conflicts.
    OverwriteAll,
    /// Skip this and later conflicts.
    SkipAll,
    /// Append `.N` until free.
    Rename,
}

/// One queued or running transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferJob {
    /// Stable id for UI updates.
    pub id: u64,
    /// Upload or download.
    pub direction: TransferDirection,
    /// Source absolute path.
    pub source: String,
    /// Destination absolute path.
    pub destination: String,
    /// Host name for remote side labels.
    pub host: String,
    /// Total bytes when known.
    pub total: u64,
    /// Bytes completed.
    pub done: u64,
    /// Current state.
    pub state: TransferState,
    /// Human error text when failed.
    pub error: Option<String>,
    /// When the job started running.
    pub started: Option<Instant>,
}

/// Lifecycle of a [`TransferJob`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferState {
    /// Waiting for a slot.
    Queued,
    /// Bytes are moving.
    Running,
    /// Finished successfully.
    Done,
    /// Failed; see `error`.
    Failed,
    /// Cancelled by the user.
    Cancelled,
}

/// Events the TUI folds into the transfer panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferEvent {
    /// A job was added.
    Queued(TransferJob),
    /// Progress update.
    Progress {
        /// Job id.
        id: u64,
        /// Bytes done.
        done: u64,
        /// Total bytes.
        total: u64,
    },
    /// Terminal state change.
    Finished {
        /// Job id.
        id: u64,
        /// Final state.
        state: TransferState,
        /// Error text when failed.
        error: Option<String>,
    },
}

/// Ask the UI how to resolve a conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct ConflictPrompt {
    /// Job that hit the conflict.
    pub id: u64,
    /// Destination path that exists.
    pub path: String,
}

/// Format bytes as `1.2K`, `3.4M`, …
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}{}", UNITS[unit])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

/// Compact age for listings: `2m`, `3h`, `4d`.
pub fn format_age(mtime: Option<SystemTime>, now: SystemTime) -> String {
    let Some(mtime) = mtime else {
        return "—".to_string();
    };
    let Ok(delta) = now.duration_since(mtime) else {
        return "—".to_string();
    };
    let secs = delta.as_secs();
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        return format!("{}m", secs / 60);
    }
    if secs < 86_400 {
        return format!("{}h", secs / 3600);
    }
    format!("{}d", secs / 86_400)
}

/// Unix mode as `drwxr-xr-x` / `-rw-r--r--`.
pub fn format_mode(kind: EntryKind, mode: Option<u32>) -> String {
    let Some(mode) = mode else {
        return "----------".to_string();
    };
    let mut out = String::with_capacity(10);
    out.push(match kind {
        EntryKind::Dir => 'd',
        EntryKind::Symlink => 'l',
        EntryKind::File => '-',
        EntryKind::Other => '?',
    });
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        out.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        out.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        out.push(if bits & 0o1 != 0 { 'x' } else { '-' });
    }
    out
}

/// Recursively collect file paths under `root` (files only for copy plan).
pub async fn collect_tree<F: RemoteFs>(
    fs: &F,
    root: &str,
) -> Result<Vec<(String, DirEntry)>, Error> {
    let root = normalize(root);
    let meta = fs.stat(&root).await?;
    if !meta.is_dir() {
        return Ok(vec![(root, meta)]);
    }
    let mut out = Vec::new();
    let mut stack = vec![root];
    let mut seen = BTreeSet::new();
    while let Some(dir) = stack.pop() {
        if !seen.insert(dir.clone()) {
            continue;
        }
        for entry in fs.list(&dir).await? {
            let path = join(&dir, &entry.name);
            if entry.is_dir() {
                stack.push(path);
            } else {
                out.push((path, entry));
            }
        }
    }
    Ok(out)
}

/// Files that [`plan_copies`] will transfer, plus whether an empty directory was created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCopy {
    /// `(source, destination, size)` for each file.
    pub files: Vec<(String, String, u64)>,
    /// True when `source` was an empty directory and that directory was created at the destination.
    pub made_directory: bool,
}

/// Where a CLI upload should write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteTarget {
    /// Copy into this directory (it exists, or `remote` ends with `/`).
    IntoDir(String),
    /// Write this exact file path, replacing a file that is already there.
    File(String),
}

/// Directory when `remote` exists as a directory or ends with `/`.
/// Otherwise the remote path is the file to create or overwrite.
pub fn classify_remote_target(remote: &str, existing_dir: bool) -> RemoteTarget {
    if remote.ends_with('/') || existing_dir {
        RemoteTarget::IntoDir(normalize(remote))
    } else {
        RemoteTarget::File(normalize(remote))
    }
}

/// Plan a `vcon push`. A remote path that is not a directory is the file name.
pub async fn plan_push<S: RemoteFs, D: RemoteFs>(
    src: &S,
    dst: &D,
    source: &str,
    remote: &str,
) -> Result<PlannedCopy, Error> {
    let source = normalize(source);
    let existing_dir = match dst.stat(remote).await {
        Ok(entry) => entry.is_dir(),
        Err(_) => false,
    };
    match classify_remote_target(remote, existing_dir) {
        RemoteTarget::IntoDir(dir) => plan_copies(src, dst, &source, &dir).await,
        RemoteTarget::File(dest) => {
            let meta = src.stat(&source).await?;
            if meta.is_dir() {
                return Err(Error::Message(format!(
                    "{remote} is a file path but {source} is a directory; add a trailing / to copy into a directory"
                )));
            }
            if let Some(parent) = parent_of(&dest) {
                ensure_dir(dst, &parent).await?;
            }
            Ok(PlannedCopy {
                files: vec![(source, dest, meta.size)],
                made_directory: false,
            })
        }
    }
}

/// Expand `source` into destination paths under `dest_dir`.
///
/// A directory keeps its own name (`/proj/a.txt` copied into `/home` becomes
/// `/home/proj/a.txt`). A file lands directly in `dest_dir`. An empty directory
/// is created and produces no file jobs.
pub async fn plan_copies<S: RemoteFs, D: RemoteFs>(
    src: &S,
    dst: &D,
    source: &str,
    dest_dir: &str,
) -> Result<PlannedCopy, Error> {
    let source = normalize(source);
    let dest_dir = normalize(dest_dir);
    let meta = src.stat(&source).await?;
    let entries = collect_tree(src, &source).await?;
    if entries.is_empty() && meta.is_dir() {
        ensure_dir(dst, &join(&dest_dir, file_name(&source))).await?;
        return Ok(PlannedCopy {
            files: Vec::new(),
            made_directory: true,
        });
    }
    let root = parent_of(&source).unwrap_or_else(|| "/".to_string());
    Ok(PlannedCopy {
        files: map_destinations(&entries, &root, &dest_dir),
        made_directory: false,
    })
}

/// One lock per normalized destination so `/out/./same.bin` and
/// `/out/same.bin` cannot interleave.
fn destination_lock(destination: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: LazyLock<std::sync::Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>> =
        LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));
    let mut locks = LOCKS.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(lock) = locks.get(destination).and_then(Weak::upgrade) {
        return lock;
    }
    locks.retain(|_, weak| weak.strong_count() > 0);
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(destination.to_string(), Arc::downgrade(&lock));
    lock
}

/// `..` in a destination is how a hostile listing writes outside the directory.
fn destination_escapes(path: &str) -> bool {
    path.split('/').any(|part| part == "..")
}

/// Refuse a symlink component whose target leaves its parent directory.
async fn refuse_symlink_destination<F: RemoteFs>(fs: &F, destination: &str) -> Result<(), Error> {
    let absolute = destination.starts_with('/');
    let mut acc = String::new();
    for part in destination
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
    {
        if acc.is_empty() {
            acc = if absolute {
                format!("/{part}")
            } else {
                part.to_string()
            };
        } else if acc == "/" {
            acc = format!("/{part}");
        } else {
            acc = format!("{acc}/{part}");
        }
        match fs.stat(&acc).await {
            Ok(entry) if entry.kind == EntryKind::Symlink => {
                if symlink_leaves_parent(&acc, entry.target.as_deref()) {
                    return Err(Error::Message(format!("refusing to follow symlink {acc}")));
                }
            }
            Err(_) => break,
            Ok(_) => {}
        }
    }
    Ok(())
}

/// True when `link`'s target is outside the directory that contains the link.
///
/// A root symlink such as macOS `/var` → `/private/var` stays inside `/`.
fn symlink_leaves_parent(link: &str, target: Option<&str>) -> bool {
    let Some(parent) = parent_of(link) else {
        return false;
    };
    if parent == "/" {
        return false;
    }
    let Some(target) = target.filter(|target| !target.is_empty()) else {
        return true;
    };
    let resolved = normalize(&if target.starts_with('/') {
        target.to_string()
    } else {
        join(&parent, target)
    });
    let parent = normalize(&parent);
    resolved != parent && !resolved.starts_with(&format!("{parent}/"))
}

/// Copy one file from `src` to `dst`, writing to a `.part` then renaming.
pub async fn copy_file<S: RemoteFs, D: RemoteFs>(
    src: &S,
    dst: &D,
    source: &str,
    destination: &str,
    cancel: &CancellationToken,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<(), Error> {
    // Reject `..` before normalize collapses an absolute path out of the tree.
    if destination_escapes(destination) {
        return Err(Error::Message(format!(
            "refusing destination that escapes its directory: {destination}"
        )));
    }
    let destination = normalize(destination);
    let lock = destination_lock(&destination);
    let _guard = lock.lock().await;
    let part = format!("{destination}.part");
    let result = copy_file_body(
        src,
        dst,
        source,
        &destination,
        &part,
        cancel,
        &mut on_progress,
    )
    .await;
    if result.is_err() {
        let _ = src.release(source).await;
        let _ = dst.release(&part).await;
        // The destination backend removes its own partial file. Do not unlink
        // that path on the local disk: a remote name can collide with a local
        // file that this transfer did not create.
        if let Err(cleanup) = dst.remove_file(&part).await {
            tracing::warn!("failed to remove partial {part}: {cleanup}");
        }
    }
    result
}

async fn copy_file_body<S: RemoteFs, D: RemoteFs>(
    src: &S,
    dst: &D,
    source: &str,
    destination: &str,
    part: &str,
    cancel: &CancellationToken,
    on_progress: &mut impl FnMut(u64, u64),
) -> Result<(), Error> {
    // Show the job as running before the first network write.
    on_progress(0, 0);
    refuse_symlink_destination(dst, destination).await?;
    let meta = src.stat(source).await?;
    if meta.kind == EntryKind::Symlink {
        return Err(Error::Message(format!("refusing to copy symlink {source}")));
    }
    if !meta.is_file() {
        return Err(Error::Message(format!(
            "refusing to copy non-regular file {source}"
        )));
    }
    let total = meta.size;
    on_progress(0, total);
    if let Some(parent) = parent_of(destination) {
        ensure_dir(dst, &parent).await?;
    }
    // A leftover handle would keep writing the pre-truncate inode.
    // `create` truncates, so a stale `.part` from a dropped session is replaced.
    let _ = dst.release(part).await;
    dst.create(part).await?;
    let mut offset = 0_u64;
    if total == 0 {
        // A real empty file reads EOF immediately. `/dev/zero` and a peer
        // that advertises size 0 keep returning bytes; stop on the first one.
        let chunk = src.read_at(source, 0, CHUNK).await?;
        if !chunk.is_empty() {
            return Err(Error::Message(format!(
                "refusing unbounded copy of {source}: size is 0 but the reader returned {} bytes",
                chunk.len()
            )));
        }
    } else {
        while offset < total {
            if cancel.is_cancelled() {
                return Err(Error::Message("cancelled".to_string()));
            }
            let remaining = CHUNK.min((total - offset) as usize);
            let chunk = src.read_at(source, offset, remaining).await?;
            if chunk.is_empty() || chunk.len() > remaining {
                return Err(Error::Message(format!(
                    "short read of {source}: expected {total} bytes, got {offset}"
                )));
            }
            dst.write_at(part, offset, &chunk).await?;
            offset += chunk.len() as u64;
            on_progress(offset, total);
        }
    }
    // Close before rename. Some SFTP servers refuse to rename an open file,
    // and the bytes are only durable once the handle is closed.
    src.release(source).await?;
    dst.release(part).await?;
    // Apply metadata before publishing. A failed chmod must not leave the
    // previous file deleted or a world-readable replacement in its place.
    dst.set_meta(part, meta.mode, meta.mtime).await?;
    // `rename` replaces the destination. Deleting it first drops the old
    // bytes when the rename itself fails.
    dst.rename(part, destination).await?;
    on_progress(total, total);
    Ok(())
}

/// Create `path` and any missing parents (best-effort if a race creates them).
async fn ensure_dir<F: RemoteFs>(fs: &F, path: &str) -> Result<(), Error> {
    let path = normalize(path);
    if path.is_empty() || path == "/" || path == "." {
        return Ok(());
    }
    if let Ok(entry) = fs.stat(&path).await {
        if entry.kind == EntryKind::Symlink {
            return Err(Error::Message(format!(
                "refusing to use symlink as a directory: {path}"
            )));
        }
        if entry.is_dir() {
            return Ok(());
        }
        return Err(Error::Message(format!("{path} is not a directory")));
    }
    if let Some(parent) = parent_of(&path) {
        Box::pin(ensure_dir(fs, &parent)).await?;
    }
    match fs.mkdir(&path).await {
        Ok(()) => Ok(()),
        Err(_error) if fs.stat(&path).await.is_ok() => Ok(()),
        Err(error) => Err(error),
    }
}

/// Destination path that does not yet exist, using `.1`, `.2`, …
pub async fn unique_name<F: RemoteFs>(fs: &F, path: &str) -> Result<String, Error> {
    if fs.stat(path).await.is_err() {
        return Ok(path.to_string());
    }
    let parent = parent_of(path).unwrap_or_else(|| "/".to_string());
    let base = file_name(path);
    for index in 1..10_000 {
        let candidate = join(&parent, &format!("{base}.{index}"));
        if fs.stat(&candidate).await.is_err() {
            return Ok(candidate);
        }
    }
    Err(Error::Message(format!("could not rename around {path}")))
}

/// Run queued jobs with limited parallelism. `jobs` are drained in order.
pub async fn run_queue<S, D>(
    src: Arc<S>,
    dst: Arc<D>,
    jobs: Vec<TransferJob>,
    cancel: CancellationToken,
    events: mpsc::Sender<TransferEvent>,
) where
    S: RemoteFs + 'static,
    D: RemoteFs + 'static,
{
    let slots = Arc::new(Semaphore::new(PARALLEL));
    let mut handles = Vec::new();
    for job in jobs {
        let slots = Arc::clone(&slots);
        let src = Arc::clone(&src);
        let dst = Arc::clone(&dst);
        let cancel = cancel.clone();
        let events = events.clone();
        handles.push(tokio::spawn(async move {
            let Ok(_permit) = slots.acquire_owned().await else {
                return;
            };
            if cancel.is_cancelled() {
                let _ = events
                    .send(TransferEvent::Finished {
                        id: job.id,
                        state: TransferState::Cancelled,
                        error: None,
                    })
                    .await;
                return;
            }
            let id = job.id;
            let source = job.source.clone();
            let destination = job.destination.clone();
            let events_progress = events.clone();
            let result = copy_file(
                src.as_ref(),
                dst.as_ref(),
                &source,
                &destination,
                &cancel,
                |done, total| {
                    let _ = events_progress.try_send(TransferEvent::Progress { id, done, total });
                },
            )
            .await;
            let (state, error) = match result {
                Ok(()) => (TransferState::Done, None),
                Err(error) if error.to_string().contains("cancelled") => {
                    (TransferState::Cancelled, None)
                }
                Err(error) => (TransferState::Failed, Some(error.to_string())),
            };
            let _ = events
                .send(TransferEvent::Finished { id, state, error })
                .await;
        }));
    }
    for handle in handles {
        let _ = handle.await;
    }
}

/// Build destination paths for a set of source paths into `dest_dir`.
pub fn map_destinations(
    sources: &[(String, DirEntry)],
    source_root: &str,
    dest_dir: &str,
) -> Vec<(String, String, u64)> {
    let root = normalize(source_root);
    sources
        .iter()
        .map(|(source, entry)| {
            let relative = source
                .strip_prefix(&root)
                .map(|rest| rest.trim_start_matches('/'))
                .filter(|rest| !rest.is_empty())
                .unwrap_or_else(|| file_name(source));
            let destination = join(dest_dir, relative);
            (source.clone(), destination, entry.size)
        })
        .collect()
}

/// Next job id helper.
#[derive(Debug, Default)]
pub struct JobIds {
    next: u64,
}

impl JobIds {
    /// Allocate a new id.
    pub fn next(&mut self) -> u64 {
        self.next = self.next.saturating_add(1);
        self.next
    }
}

/// In-memory queue the TUI mirrors.
#[derive(Debug, Default)]
pub struct TransferQueue {
    /// Jobs newest last.
    pub jobs: VecDeque<TransferJob>,
    /// Ids.
    pub ids: JobIds,
}

impl TransferQueue {
    /// True when any job is queued or running.
    pub fn active(&self) -> bool {
        self.jobs
            .iter()
            .any(|job| matches!(job.state, TransferState::Queued | TransferState::Running))
    }

    /// Apply a transfer event.
    pub fn apply(&mut self, event: TransferEvent) {
        match event {
            TransferEvent::Queued(job) => {
                if self.jobs.iter().any(|existing| existing.id == job.id) {
                    return;
                }
                self.jobs.push_back(job);
            }
            TransferEvent::Progress { id, done, total } => {
                if let Some(job) = self.jobs.iter_mut().find(|job| job.id == id) {
                    job.done = done;
                    job.total = total;
                    job.state = TransferState::Running;
                    if job.started.is_none() {
                        job.started = Some(Instant::now());
                    }
                }
            }
            TransferEvent::Finished { id, state, error } => {
                if let Some(job) = self.jobs.iter_mut().find(|job| job.id == id) {
                    job.state = state;
                    job.error = error;
                    if state == TransferState::Done {
                        job.done = job.total;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::remote_fs::FakeFs;

    #[test]
    fn prepare_listing_sorts_dirs_first() {
        let entries = vec![
            DirEntry {
                name: "b.txt".into(),
                kind: EntryKind::File,
                size: 2,
                mtime: None,
                mode: None,
                target: None,
            },
            DirEntry {
                name: "a".into(),
                kind: EntryKind::Dir,
                size: 0,
                mtime: None,
                mode: None,
                target: None,
            },
            DirEntry {
                name: ".hidden".into(),
                kind: EntryKind::File,
                size: 1,
                mtime: None,
                mode: None,
                target: None,
            },
        ];
        let visible = prepare_listing(entries, SortKey::Name, false);
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].name, "a");
        assert_eq!(visible[1].name, "b.txt");
    }

    #[tokio::test]
    async fn copy_file_uses_part_then_rename() {
        let src = FakeFs::new();
        let dst = FakeFs::new();
        src.insert_file("/a.txt", b"hello");
        dst.insert_dir("/dest");
        copy_file(
            &src,
            &dst,
            "/a.txt",
            "/dest/a.txt",
            &CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(dst.read_at("/dest/a.txt", 0, 16).await.unwrap(), b"hello");
        assert!(dst.stat("/dest/a.txt.part").await.is_err());
    }

    #[tokio::test]
    async fn cancel_removes_partial() {
        let src = FakeFs::new();
        let dst = FakeFs::new();
        src.insert_file("/big.bin", vec![1_u8; CHUNK * 4]);
        dst.insert_dir("/dest");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = copy_file(&src, &dst, "/big.bin", "/dest/big.bin", &cancel, |_, _| {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cancelled"));
        assert!(dst.stat("/dest/big.bin").await.is_err());
        assert!(dst.stat("/dest/big.bin.part").await.is_err());
    }

    #[tokio::test]
    async fn copy_file_creates_missing_parent_dirs() {
        let src = FakeFs::new();
        let dst = FakeFs::new();
        src.insert_file("/a.txt", b"nested");
        dst.insert_dir("/dest");
        copy_file(
            &src,
            &dst,
            "/a.txt",
            "/dest/deep/down/a.txt",
            &CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(
            dst.read_at("/dest/deep/down/a.txt", 0, 16).await.unwrap(),
            b"nested"
        );
    }

    #[test]
    fn remote_file_path_is_not_treated_as_a_directory() {
        assert_eq!(
            classify_remote_target("/tmp/app.conf", false),
            RemoteTarget::File("/tmp/app.conf".into())
        );
        assert_eq!(
            classify_remote_target("/tmp/app.conf/", false),
            RemoteTarget::IntoDir("/tmp/app.conf".into())
        );
        assert_eq!(
            classify_remote_target("/tmp/app.conf", true),
            RemoteTarget::IntoDir("/tmp/app.conf".into())
        );
    }

    #[tokio::test]
    async fn plan_push_overwrites_a_remote_file() {
        let src = FakeFs::new();
        let dst = FakeFs::new();
        src.insert_file("/local.conf", b"new");
        dst.insert_file("/tmp/app.conf", b"old");
        let planned = plan_push(&src, &dst, "/local.conf", "/tmp/app.conf")
            .await
            .unwrap();
        assert_eq!(planned.files.len(), 1);
        assert_eq!(planned.files[0].1, "/tmp/app.conf");
    }

    #[tokio::test]
    async fn plan_push_uses_the_path_as_the_file_when_it_is_missing() {
        let src = FakeFs::new();
        let dst = FakeFs::new();
        src.insert_file("/big.bin", b"data");
        let planned = plan_push(&src, &dst, "/big.bin", "/tmp/big.bin")
            .await
            .unwrap();
        assert_eq!(planned.files[0].1, "/tmp/big.bin");
        assert!(dst.stat("/tmp").await.unwrap().is_dir());
        assert!(dst.stat("/tmp/big.bin").await.is_err());
    }

    #[tokio::test]
    async fn plan_copies_keeps_the_directory_name() {
        let src = FakeFs::new();
        let dst = FakeFs::new();
        src.insert_file("/proj/a.txt", b"a");
        src.insert_file("/proj/sub/b.txt", b"b");
        dst.insert_dir("/home");
        let planned = plan_copies(&src, &dst, "/proj", "/home").await.unwrap();
        let dests: Vec<&str> = planned
            .files
            .iter()
            .map(|(_, dest, _)| dest.as_str())
            .collect();
        assert!(dests.contains(&"/home/proj/a.txt"));
        assert!(dests.contains(&"/home/proj/sub/b.txt"));
        assert!(!planned.made_directory);
    }

    #[tokio::test]
    async fn plan_copies_creates_an_empty_directory() {
        let src = FakeFs::new();
        let dst = FakeFs::new();
        src.insert_dir("/empty");
        dst.insert_dir("/home");
        let planned = plan_copies(&src, &dst, "/empty", "/home").await.unwrap();
        assert!(planned.files.is_empty());
        assert!(planned.made_directory);
        assert!(dst.stat("/home/empty").await.unwrap().is_dir());
    }

    #[test]
    fn queued_event_does_not_duplicate_the_same_id() {
        let mut queue = TransferQueue::default();
        let job = TransferJob {
            id: 1,
            direction: TransferDirection::Upload,
            source: "/a".into(),
            destination: "/b".into(),
            host: "web-1".into(),
            total: 1,
            done: 0,
            state: TransferState::Queued,
            error: None,
            started: None,
        };
        queue.apply(TransferEvent::Queued(job.clone()));
        queue.apply(TransferEvent::Queued(job));
        assert_eq!(queue.jobs.len(), 1);
    }
}
