//! Browse directories and queue file transfers.

use crate::error::Error;
use crate::infra::remote_fs::{
    DirEntry, EntryKind, RemoteFs, file_name, join, normalize, parent_of,
};
use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

/// Bytes read/written per transfer chunk.
pub const CHUNK: usize = 64 * 1024;

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
    if meta.is_file() || meta.kind == EntryKind::Symlink {
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
            match entry.kind {
                EntryKind::Dir => stack.push(path),
                EntryKind::File | EntryKind::Symlink | EntryKind::Other => {
                    out.push((path, entry));
                }
            }
        }
    }
    Ok(out)
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
    let meta = src.stat(source).await?;
    let total = meta.size;
    let part = format!("{destination}.part");
    if let Some(parent) = parent_of(destination) {
        let _ = dst.mkdir(&parent).await;
    }
    dst.create(&part).await?;
    let mut offset = 0_u64;
    while offset < total {
        if cancel.is_cancelled() {
            let _ = dst.remove_file(&part).await;
            return Err(Error::Message("cancelled".to_string()));
        }
        let want = CHUNK.min((total - offset) as usize);
        let chunk = src.read_at(source, offset, want).await?;
        if chunk.is_empty() {
            break;
        }
        dst.write_at(&part, offset, &chunk).await?;
        offset += chunk.len() as u64;
        on_progress(offset, total);
    }
    // Replace destination.
    let _ = dst.remove_file(destination).await;
    dst.rename(&part, destination).await?;
    let _ = dst.set_meta(destination, meta.mode, meta.mtime).await;
    on_progress(total, total);
    Ok(())
}

/// Destination path that does not yet exist, using `.1`, `.2`, …
#[allow(dead_code)]
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
            TransferEvent::Queued(job) => self.jobs.push_back(job),
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
}
