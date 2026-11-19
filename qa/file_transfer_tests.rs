//! QA coverage for file transfer. These tests intentionally assert the safe
//! behavior. Failures are findings, not something the suite papers over.

use crate::domain::host::EffectiveHost;
use crate::error::Error;
use crate::infra::inventory_store::write_atomic;
use crate::infra::remote_fs::{
    DirEntry, EntryKind, LocalFs, RemoteFs, SftpFs, file_name, normalize, sftp_arguments_auth,
};
use crate::infra::ssh::system::find_ssh;
use crate::services::files::{
    CHUNK, TransferDirection, TransferEvent, TransferJob, TransferState, copy_file, plan_copies,
    run_queue,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn file_entry(name: &str, size: u64) -> DirEntry {
    DirEntry {
        name: name.to_string(),
        kind: EntryKind::File,
        size,
        mtime: Some(SystemTime::UNIX_EPOCH),
        mode: Some(0o100644),
        target: None,
    }
}

fn dir_entry(name: &str) -> DirEntry {
    DirEntry {
        name: name.to_string(),
        kind: EntryKind::Dir,
        size: 0,
        mtime: Some(SystemTime::UNIX_EPOCH),
        mode: Some(0o040755),
        target: None,
    }
}

#[derive(Clone)]
enum ReadMode {
    Stored,
    /// First read returns `payload`, later reads return EOF.
    PrefixThenEof,
    /// Every read returns bytes. Used with a declared size of 0.
    Forever,
    /// First read returns `payload`, the next read is an error.
    FailOnSecond,
}

struct Node {
    dir: bool,
    data: Vec<u8>,
    mode: u32,
}

struct LabFs {
    nodes: Mutex<BTreeMap<String, Node>>,
    hostile: Mutex<Option<(String, Vec<DirEntry>)>>,
    payload: Mutex<Vec<u8>>,
    read_mode: Mutex<ReadMode>,
    force_size: Mutex<Option<u64>>,
    stat_missing_as_file: AtomicBool,
    fail_rename: AtomicBool,
    fail_meta: AtomicBool,
    fail_remove: AtomicBool,
    discard: AtomicBool,
    written: AtomicU64,
    reads: AtomicUsize,
    inflight: AtomicUsize,
    overlapped: AtomicBool,
    delay: Mutex<Duration>,
}

impl LabFs {
    fn new() -> Self {
        let fs = Self {
            nodes: Mutex::new(BTreeMap::new()),
            hostile: Mutex::new(None),
            payload: Mutex::new(Vec::new()),
            read_mode: Mutex::new(ReadMode::Stored),
            force_size: Mutex::new(None),
            stat_missing_as_file: AtomicBool::new(false),
            fail_rename: AtomicBool::new(false),
            fail_meta: AtomicBool::new(false),
            fail_remove: AtomicBool::new(false),
            discard: AtomicBool::new(false),
            written: AtomicU64::new(0),
            reads: AtomicUsize::new(0),
            inflight: AtomicUsize::new(0),
            overlapped: AtomicBool::new(false),
            delay: Mutex::new(Duration::ZERO),
        };
        fs.nodes.lock().expect("lab").insert(
            "/".to_string(),
            Node {
                dir: true,
                data: Vec::new(),
                mode: 0o755,
            },
        );
        fs
    }

    fn insert_dir(&self, path: &str) {
        self.nodes.lock().expect("lab").insert(
            normalize(path),
            Node {
                dir: true,
                data: Vec::new(),
                mode: 0o755,
            },
        );
    }

    fn insert_file(&self, path: &str, data: &[u8]) {
        self.nodes.lock().expect("lab").insert(
            normalize(path),
            Node {
                dir: false,
                data: data.to_vec(),
                mode: 0o644,
            },
        );
    }

    fn snapshot(&self, path: &str) -> Option<Vec<u8>> {
        self.nodes
            .lock()
            .expect("lab")
            .get(&normalize(path))
            .filter(|node| !node.dir)
            .map(|node| node.data.clone())
    }

    fn mode_of(&self, path: &str) -> Option<u32> {
        self.nodes
            .lock()
            .expect("lab")
            .get(&normalize(path))
            .map(|node| node.mode)
    }
}

impl RemoteFs for LabFs {
    async fn list(&self, dir: &str) -> Result<Vec<DirEntry>, Error> {
        let dir = normalize(dir);
        if let Some((root, entries)) = self.hostile.lock().expect("lab").clone()
            && dir == root
        {
            return Ok(entries);
        }
        let nodes = self.nodes.lock().expect("lab");
        if !matches!(nodes.get(&dir), Some(Node { dir: true, .. })) {
            return Err(Error::Message(format!("no such directory: {dir}")));
        }
        let prefix = if dir == "/" {
            "/".to_string()
        } else {
            format!("{dir}/")
        };
        let mut entries = Vec::new();
        for (path, node) in nodes.iter() {
            if !path.starts_with(&prefix) {
                continue;
            }
            let rest = &path[prefix.len()..];
            if rest.is_empty() || rest.contains('/') {
                continue;
            }
            if node.dir {
                entries.push(dir_entry(rest));
            } else {
                entries.push(file_entry(rest, node.data.len() as u64));
            }
        }
        Ok(entries)
    }

    async fn stat(&self, path: &str) -> Result<DirEntry, Error> {
        let path = normalize(path);
        let nodes = self.nodes.lock().expect("lab");
        if let Some(node) = nodes.get(&path) {
            if node.dir {
                return Ok(dir_entry(file_name(&path)));
            }
            let size = self
                .force_size
                .lock()
                .expect("lab")
                .unwrap_or(node.data.len() as u64);
            let mut entry = file_entry(file_name(&path), size);
            entry.mode = Some(node.mode);
            return Ok(entry);
        }
        drop(nodes);
        if self.stat_missing_as_file.load(Ordering::SeqCst)
            || self.force_size.lock().expect("lab").is_some()
        {
            let size = self
                .force_size
                .lock()
                .expect("lab")
                .unwrap_or_else(|| self.payload.lock().expect("lab").len() as u64);
            return Ok(file_entry(file_name(&path), size));
        }
        Err(Error::Message(format!("no such path: {path}")))
    }

    async fn mkdir(&self, path: &str) -> Result<(), Error> {
        let path = normalize(path);
        let mut nodes = self.nodes.lock().expect("lab");
        if nodes.contains_key(&path) {
            return Err(Error::Message(format!("already exists: {path}")));
        }
        nodes.insert(
            path,
            Node {
                dir: true,
                data: Vec::new(),
                mode: 0o755,
            },
        );
        Ok(())
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        if self.fail_rename.load(Ordering::SeqCst) {
            return Err(Error::Message(format!(
                "injected rename failure {from} -> {to}"
            )));
        }
        let from = normalize(from);
        let to = normalize(to);
        let mut nodes = self.nodes.lock().expect("lab");
        let node = nodes
            .remove(&from)
            .ok_or_else(|| Error::Message(format!("no such path: {from}")))?;
        nodes.insert(to, node);
        Ok(())
    }

    async fn remove_file(&self, path: &str) -> Result<(), Error> {
        if self.fail_remove.load(Ordering::SeqCst) {
            return Err(Error::Message(format!("injected remove failure {path}")));
        }
        let path = normalize(path);
        let mut nodes = self.nodes.lock().expect("lab");
        match nodes.get(&path) {
            Some(Node { dir: false, .. }) => {
                nodes.remove(&path);
                Ok(())
            }
            Some(_) => Err(Error::Message(format!("{path} is not a file"))),
            None => Err(Error::Message(format!("no such file: {path}"))),
        }
    }

    async fn remove_dir(&self, path: &str) -> Result<(), Error> {
        let path = normalize(path);
        let mut nodes = self.nodes.lock().expect("lab");
        if matches!(nodes.get(&path), Some(Node { dir: true, .. })) {
            nodes.remove(&path);
            return Ok(());
        }
        Err(Error::Message(format!("no such directory: {path}")))
    }

    async fn realpath(&self, path: &str) -> Result<String, Error> {
        Ok(normalize(path))
    }

    async fn read_at(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        let mode = self.read_mode.lock().expect("lab").clone();
        match mode {
            ReadMode::PrefixThenEof => {
                let n = self.reads.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Ok(self.payload.lock().expect("lab").clone())
                } else {
                    Ok(Vec::new())
                }
            }
            ReadMode::Forever => {
                // Yield so a caller timeout can observe that the copy never finishes.
                tokio::task::yield_now().await;
                Ok(vec![9_u8; len.max(1)])
            }
            ReadMode::FailOnSecond => {
                let n = self.reads.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Ok(self.payload.lock().expect("lab").clone())
                } else {
                    Err(Error::Message("injected read failure".to_string()))
                }
            }
            ReadMode::Stored => {
                let nodes = self.nodes.lock().expect("lab");
                if let Some(Node {
                    dir: false, data, ..
                }) = nodes.get(&normalize(path))
                {
                    let start = usize::try_from(offset).unwrap_or(usize::MAX);
                    if start >= data.len() {
                        return Ok(Vec::new());
                    }
                    let end = (start + len).min(data.len());
                    return Ok(data[start..end].to_vec());
                }
                drop(nodes);
                if self.stat_missing_as_file.load(Ordering::SeqCst) {
                    let data = self.payload.lock().expect("lab");
                    let start = usize::try_from(offset).unwrap_or(usize::MAX);
                    if start >= data.len() {
                        return Ok(Vec::new());
                    }
                    let end = (start + len).min(data.len());
                    return Ok(data[start..end].to_vec());
                }
                Err(Error::Message(format!("no such file: {path}")))
            }
        }
    }

    async fn create(&self, path: &str) -> Result<(), Error> {
        self.nodes.lock().expect("lab").insert(
            normalize(path),
            Node {
                dir: false,
                data: Vec::new(),
                mode: 0o644,
            },
        );
        Ok(())
    }

    async fn write_at(&self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        let previous = self.inflight.fetch_add(1, Ordering::SeqCst);
        if previous > 0 {
            self.overlapped.store(true, Ordering::SeqCst);
        }
        let delay = *self.delay.lock().expect("lab");
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let outcome = if self.discard.load(Ordering::SeqCst) {
            self.written.fetch_add(data.len() as u64, Ordering::SeqCst);
            Ok(())
        } else {
            let mut nodes = self.nodes.lock().expect("lab");
            match nodes.get_mut(&normalize(path)) {
                Some(Node {
                    dir: false,
                    data: body,
                    ..
                }) => {
                    let start = usize::try_from(offset).unwrap_or(usize::MAX);
                    if body.len() < start {
                        body.resize(start, 0);
                    }
                    let end = start + data.len();
                    if body.len() < end {
                        body.resize(end, 0);
                    }
                    body[start..end].copy_from_slice(data);
                    Ok(())
                }
                _ => Err(Error::Message(format!("no such file: {path}"))),
            }
        };
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        outcome
    }

    async fn set_meta(
        &self,
        path: &str,
        mode: Option<u32>,
        _mtime: Option<SystemTime>,
    ) -> Result<(), Error> {
        if self.fail_meta.load(Ordering::SeqCst) {
            return Err(Error::Message(format!("injected metadata failure {path}")));
        }
        if let Some(mode) = mode
            && let Some(node) = self.nodes.lock().expect("lab").get_mut(&normalize(path))
        {
            node.mode = mode & 0o777;
        }
        Ok(())
    }
}

async fn copy_ok(src: &Path, dst: &Path) {
    copy_file(
        &LocalFs,
        &LocalFs,
        &path_str(src),
        &path_str(dst),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await
    .unwrap_or_else(|error| panic!("copy {} -> {}: {error}", src.display(), dst.display()));
}

fn files_equal(left: &Path, right: &Path) -> bool {
    use std::io::Read;
    let mut a = std::fs::File::open(left).expect("open left");
    let mut b = std::fs::File::open(right).expect("open right");
    let mut abuf = vec![0_u8; 1024 * 1024];
    let mut bbuf = vec![0_u8; 1024 * 1024];
    loop {
        let n = a.read(&mut abuf).expect("read left");
        let mut got = 0;
        while got < n {
            let m = b.read(&mut bbuf[got..n]).expect("read right");
            if m == 0 {
                return false;
            }
            got += m;
        }
        if n == 0 {
            return b.read(&mut bbuf).expect("tail") == 0;
        }
        if abuf[..n] != bbuf[..n] {
            return false;
        }
    }
}

#[cfg(target_os = "linux")]
fn proc_kb(key: &str) -> u64 {
    let text = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            return rest
                .split_whitespace()
                .next()
                .unwrap_or("0")
                .parse()
                .unwrap_or(0);
        }
    }
    0
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
}

#[tokio::test]
async fn qa_local_roundtrip_boundaries_text_binary_names_and_tree() {
    let lengths = [
        0,
        1,
        2,
        CHUNK - 1,
        CHUNK,
        CHUNK + 1,
        (2 * CHUNK) - 1,
        2 * CHUNK,
        (2 * CHUNK) + 1,
    ];
    for len in lengths {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.bin");
        let dst = dir.path().join("out.bin");
        let bytes = pattern(len);
        std::fs::write(&src, &bytes).unwrap();
        copy_ok(&src, &dst).await;
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            bytes,
            "roundtrip length {len}"
        );
        assert!(
            !Path::new(&format!("{}.part", path_str(&dst))).exists(),
            ".part left behind for length {len}"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let text = dir.path().join("notes.txt");
    std::fs::write(&text, "line\nline\n").unwrap();
    let text_out = dir.path().join("notes-out.txt");
    copy_ok(&text, &text_out).await;
    assert_eq!(std::fs::read(&text_out).unwrap(), b"line\nline\n");

    let weird = dir.path().join("my file 文件.txt");
    std::fs::write(&weird, "ok").unwrap();
    let weird_out = dir.path().join("out dir");
    std::fs::create_dir(&weird_out).unwrap();
    let dest = weird_out.join("my file 文件.txt");
    copy_ok(&weird, &dest).await;
    assert_eq!(std::fs::read(&dest).unwrap(), b"ok");

    let newline = dir.path().join("a\nb.txt");
    std::fs::write(&newline, b"nl").unwrap();
    let newline_out = dir.path().join("nl-out");
    copy_ok(&newline, &newline_out).await;
    assert_eq!(std::fs::read(&newline_out).unwrap(), b"nl");

    let long_ok = dir.path().join("a".repeat(200));
    std::fs::write(&long_ok, b"L").unwrap();
    let long_dest = dir.path().join("b".repeat(200));
    copy_ok(&long_ok, &long_dest).await;
    assert_eq!(std::fs::read(&long_dest).unwrap(), b"L");

    let too_long = dir.path().join("c".repeat(256));
    let err = copy_file(
        &LocalFs,
        &LocalFs,
        &path_str(&long_ok),
        &path_str(&too_long),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await
    .expect_err("256-byte filename must fail, not panic");
    let message = err.to_string();
    assert!(
        message.to_ascii_lowercase().contains("file name")
            || message.to_ascii_lowercase().contains("too long")
            || message.to_ascii_lowercase().contains("os error"),
        "unexpected error for an overlong name: {message}"
    );

    let nul = copy_file(
        &LocalFs,
        &LocalFs,
        &path_str(&long_ok),
        &format!("{}\0nope", dir.path().display()),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await;
    assert!(
        nul.is_err(),
        "NUL path must be an error, not a panic: {nul:?}"
    );

    let tree = dir.path().join("proj");
    std::fs::create_dir_all(tree.join("sub")).unwrap();
    for index in 0..40 {
        std::fs::write(tree.join(format!("f{index}.bin")), pattern(32 + index)).unwrap();
    }
    std::fs::write(tree.join("sub/nested.txt"), b"nested").unwrap();
    let forest = dir.path().join("forest");
    std::fs::create_dir(&forest).unwrap();
    let planned = plan_copies(&LocalFs, &LocalFs, &path_str(&tree), &path_str(&forest))
        .await
        .unwrap();
    assert_eq!(planned.files.len(), 41);
    for (src, destination, _) in planned.files {
        copy_file(
            &LocalFs,
            &LocalFs,
            &src,
            &destination,
            &CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap();
    }
    assert_eq!(
        std::fs::read(forest.join("proj/sub/nested.txt")).unwrap(),
        b"nested"
    );
    assert_eq!(
        std::fs::read(forest.join("proj/f0.bin")).unwrap(),
        pattern(32)
    );

    let empty = dir.path().join("empty-src");
    std::fs::create_dir(&empty).unwrap();
    let planned = plan_copies(&LocalFs, &LocalFs, &path_str(&empty), &path_str(&forest))
        .await
        .unwrap();
    assert!(planned.files.is_empty());
    assert!(planned.made_directory);
    assert!(
        std::fs::metadata(forest.join("empty-src"))
            .unwrap()
            .is_dir()
    );
}

#[tokio::test]
async fn qa_same_file_twice_and_onto_itself() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.bin");
    let dst = dir.path().join("b.bin");
    std::fs::write(&src, b"one").unwrap();
    copy_ok(&src, &dst).await;
    std::fs::write(&src, b"two").unwrap();
    copy_ok(&src, &dst).await;
    assert_eq!(std::fs::read(&dst).unwrap(), b"two");
    copy_ok(&dst, &dst).await;
    assert_eq!(std::fs::read(&dst).unwrap(), b"two");
    assert!(!Path::new(&format!("{}.part", path_str(&dst))).exists());
}

#[tokio::test]
async fn qa_cancel_keeps_the_original_and_deletes_the_part() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("big.bin");
    let dst = dir.path().join("kept.bin");
    std::fs::write(&src, pattern(CHUNK + 32)).unwrap();
    std::fs::write(&dst, b"OLD").unwrap();
    let cancel = CancellationToken::new();
    let flag = cancel.clone();
    let err = copy_file(
        &LocalFs,
        &LocalFs,
        &path_str(&src),
        &path_str(&dst),
        &cancel,
        |done, _| {
            if done > 0 {
                flag.cancel();
            }
        },
    )
    .await
    .expect_err("cancel");
    assert!(err.to_string().contains("cancelled"), "{err}");
    assert_eq!(std::fs::read(&dst).unwrap(), b"OLD");
    assert!(!Path::new(&format!("{}.part", path_str(&dst))).exists());
}

#[tokio::test]
async fn qa_read_error_keeps_destination_and_deletes_part() {
    let src = LabFs::new();
    src.insert_file("/in", &pattern(64));
    *src.payload.lock().unwrap() = pattern(64);
    *src.force_size.lock().unwrap() = Some((CHUNK * 2) as u64);
    *src.read_mode.lock().unwrap() = ReadMode::FailOnSecond;
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("kept.bin");
    std::fs::write(&dst, b"OLD").unwrap();
    let err = copy_file(
        &src,
        &LocalFs,
        "/in",
        &path_str(&dst),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await
    .expect_err("injected read");
    assert!(err.to_string().contains("injected read failure"), "{err}");
    assert_eq!(std::fs::read(&dst).unwrap(), b"OLD");
    assert!(!Path::new(&format!("{}.part", path_str(&dst))).exists());
}

#[tokio::test]
async fn qa_permission_denied_is_surfaced_and_part_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("in.bin");
    std::fs::write(&src, b"data").unwrap();
    let ro = dir.path().join("ro");
    std::fs::create_dir(&ro).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    let dst = ro.join("out.bin");
    let err = copy_file(
        &LocalFs,
        &LocalFs,
        &path_str(&src),
        &path_str(&dst),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await
    .expect_err("permission");
    let message = err.to_string().to_ascii_lowercase();
    assert!(
        message.contains("permission") || message.contains("denied"),
        "{err}"
    );
    assert!(!dst.exists());
    assert!(!Path::new(&format!("{}.part", path_str(&dst))).exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).ok();
    }
}

#[tokio::test]
async fn qa_parent_component_that_is_a_file_fails() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("in.bin");
    std::fs::write(&src, b"data").unwrap();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a dir").unwrap();
    let dst = blocker.join("out.bin");
    let err = copy_file(
        &LocalFs,
        &LocalFs,
        &path_str(&src),
        &path_str(&dst),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await
    .expect_err("file used as a directory");
    assert!(
        !err.to_string().is_empty(),
        "empty error for a file parent: {err}"
    );
}

#[tokio::test]
async fn qa_mode_is_preserved_on_the_happy_path() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("in.bin");
    let dst = dir.path().join("out.bin");
    std::fs::write(&src, b"mode").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o700)).unwrap();
        copy_ok(&src, &dst).await;
        let mode = std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "destination mode was {mode:o}");
    }
}

#[tokio::test]
async fn qa_short_read_must_not_report_success() {
    let src = LabFs::new();
    src.insert_file("/in", b"short");
    *src.payload.lock().unwrap() = b"short".to_vec();
    *src.force_size.lock().unwrap() = Some(1000);
    *src.read_mode.lock().unwrap() = ReadMode::PrefixThenEof;
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("out.bin");
    let result = copy_file(
        &src,
        &LocalFs,
        "/in",
        &path_str(&dst),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await;
    let got = std::fs::read(&dst).unwrap_or_default();
    assert!(
        result.is_err(),
        "declared 1000 bytes but read {} and copy returned Ok; bytes={got:?}",
        got.len()
    );
}

#[tokio::test]
async fn qa_untrusted_size_does_not_allocate_the_declared_length() {
    let src = LabFs::new();
    src.insert_file("/in", b"");
    *src.force_size.lock().unwrap() = Some(u64::MAX);
    *src.read_mode.lock().unwrap() = ReadMode::PrefixThenEof;
    *src.payload.lock().unwrap() = Vec::new();
    let dst = LabFs::new();
    dst.insert_dir("/out");
    #[cfg(target_os = "linux")]
    let before = proc_kb("VmRSS:");
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        copy_file(
            &src,
            &dst,
            "/in",
            "/out/file",
            &CancellationToken::new(),
            |_, _| {},
        ),
    )
    .await;
    assert!(
        result.is_ok(),
        "declared size u64::MAX hung the copy ({:?})",
        started.elapsed()
    );
    #[cfg(target_os = "linux")]
    {
        let after = proc_kb("VmRSS:");
        let grew = after.saturating_sub(before);
        assert!(
            grew < 64 * 1024,
            "RSS grew by {grew} KiB for a declared size of u64::MAX"
        );
    }
    let _ = result.unwrap();
}

#[tokio::test]
async fn qa_zero_size_infinite_reader_must_stop() {
    let src = LabFs::new();
    src.insert_file("/dev/zero-like", b"");
    *src.force_size.lock().unwrap() = Some(0);
    *src.read_mode.lock().unwrap() = ReadMode::Forever;
    let dst = LabFs::new();
    dst.discard.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_millis(400),
        copy_file(
            &src,
            &dst,
            "/dev/zero-like",
            "/out",
            &CancellationToken::new(),
            |_, _| {},
        ),
    )
    .await;
    let written = dst.written.load(Ordering::SeqCst);
    assert!(
        result.is_ok(),
        "size 0 with a non-empty read never returned; wrote {written} bytes in {:?}",
        started.elapsed()
    );
    let inner = result.unwrap();
    assert!(
        inner.is_err(),
        "size-0 infinite reader was treated as success after {written} bytes: {inner:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn qa_dev_zero_copy_must_stop() {
    let meta = LocalFs.stat("/dev/zero").await.expect("stat /dev/zero");
    assert_eq!(meta.size, 0, "/dev/zero size was {}", meta.size);
    let dst = LabFs::new();
    dst.discard.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_millis(400),
        copy_file(
            &LocalFs,
            &dst,
            "/dev/zero",
            "/out",
            &CancellationToken::new(),
            |_, _| {},
        ),
    )
    .await;
    let written = dst.written.load(Ordering::SeqCst);
    assert!(
        result.is_ok(),
        "copy of /dev/zero hung after {written} bytes ({:?})",
        started.elapsed()
    );
    assert!(
        result.unwrap().is_err(),
        "/dev/zero copy succeeded after writing {written} bytes"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn qa_fifo_copy_must_not_block() {
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("pipe");
    let c_path = std::ffi::CString::new(path_str(&fifo)).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) }, 0);
    // Held only so the write-open in Drop can finish. The copy itself must
    // fail from lstat, before any read.
    let _reader = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .expect("nonblocking fifo reader")
    };
    let unblock = fifo.clone();
    struct OpenWrite(PathBuf);
    impl Drop for OpenWrite {
        fn drop(&mut self) {
            let _ = std::fs::OpenOptions::new().write(true).open(&self.0);
        }
    }
    let _guard = OpenWrite(unblock);
    let dst = dir.path().join("out");
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        copy_file(
            &LocalFs,
            &LocalFs,
            &path_str(&fifo),
            &path_str(&dst),
            &CancellationToken::new(),
            |_, _| {},
        ),
    )
    .await;
    assert!(
        result.is_ok(),
        "copying a fifo with no writer blocked the transfer"
    );
    assert!(
        result.unwrap().is_err(),
        "fifo copy should fail instead of waiting for a writer"
    );
}

#[tokio::test]
async fn qa_hostile_directory_entry_cannot_escape_the_destination() {
    let payload = b"PWNED-TRAVERSAL".to_vec();
    let src = LabFs::new();
    src.insert_dir("/inbox");
    *src.payload.lock().unwrap() = payload.clone();
    src.stat_missing_as_file.store(true, Ordering::SeqCst);
    *src.hostile.lock().unwrap() = Some((
        "/inbox".to_string(),
        vec![file_entry("../../escaped.bin", payload.len() as u64)],
    ));
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let planned = plan_copies(&src, &LocalFs, "/inbox", &path_str(&dest))
        .await
        .expect("plan");
    assert_eq!(planned.files.len(), 1, "planned {:?}", planned.files);
    let (source, destination, _) = planned.files[0].clone();
    let copied = copy_file(
        &src,
        &LocalFs,
        &source,
        &destination,
        &CancellationToken::new(),
        |_, _| {},
    )
    .await;
    let escape = dir.path().join("escaped.bin");
    let leaked = escape.is_file();
    assert!(
        copied.is_err(),
        "a directory entry named '../..' was accepted ({copied:?}); escape exists={leaked} dest={destination}"
    );
    assert!(
        !leaked,
        "payload written outside the destination at {}",
        escape.display()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn qa_symlink_outside_the_tree_is_not_copied_as_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret");
    std::fs::write(&secret, b"TOP-SECRET").unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::os::unix::fs::symlink(&secret, src.join("link")).unwrap();
    let dest = dir.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let planned = plan_copies(&LocalFs, &LocalFs, &path_str(&src), &path_str(&dest))
        .await
        .unwrap();
    for (source, destination, _) in planned.files {
        let _ = copy_file(
            &LocalFs,
            &LocalFs,
            &source,
            &destination,
            &CancellationToken::new(),
            |_, _| {},
        )
        .await;
    }
    let copied = dest.join("src/link");
    if copied.exists() {
        let meta = std::fs::symlink_metadata(&copied).unwrap();
        let bytes = std::fs::read(&copied).unwrap_or_default();
        assert!(
            meta.file_type().is_symlink(),
            "symlink was materialized as a regular file containing {:?}",
            String::from_utf8_lossy(&bytes)
        );
        assert_ne!(bytes, b"TOP-SECRET");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn qa_directory_symlink_does_not_recurse_forever() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::os::unix::fs::symlink(&src, src.join("loop")).unwrap();
    let dest = dir.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        let planned = plan_copies(&LocalFs, &LocalFs, &path_str(&src), &path_str(&dest)).await?;
        for (source, destination, _) in planned.files {
            let _ = copy_file(
                &LocalFs,
                &LocalFs,
                &source,
                &destination,
                &CancellationToken::new(),
                |_, _| {},
            )
            .await;
        }
        Ok::<(), Error>(())
    })
    .await;
    assert!(result.is_ok(), "directory symlink recurse hung");
}

#[tokio::test]
async fn qa_rename_failure_must_not_delete_the_existing_destination() {
    let src_dir = tempfile::tempdir().unwrap();
    let src = src_dir.path().join("new.bin");
    std::fs::write(&src, b"NEW").unwrap();
    let dst = LabFs::new();
    dst.insert_dir("/out");
    dst.insert_file("/out/file", b"ORIGINAL");
    dst.fail_rename.store(true, Ordering::SeqCst);
    let err = copy_file(
        &LocalFs,
        &dst,
        &path_str(&src),
        "/out/file",
        &CancellationToken::new(),
        |_, _| {},
    )
    .await
    .expect_err("rename");
    assert!(err.to_string().contains("injected rename"), "{err}");
    assert_eq!(
        dst.snapshot("/out/file").as_deref(),
        Some(b"ORIGINAL".as_slice()),
        "existing destination was destroyed when rename failed"
    );
}

#[tokio::test]
async fn qa_metadata_failure_is_surfaced() {
    let src = LabFs::new();
    src.insert_file("/in", b"body");
    if let Some(node) = src.nodes.lock().unwrap().get_mut("/in") {
        node.mode = 0o600;
    }
    let dst = LabFs::new();
    dst.fail_meta.store(true, Ordering::SeqCst);
    let result = copy_file(
        &src,
        &dst,
        "/in",
        "/out",
        &CancellationToken::new(),
        |_, _| {},
    )
    .await;
    let err = result.expect_err("set_meta failure must fail the copy");
    assert!(
        err.to_string().contains("metadata"),
        "metadata error was dropped: {err}"
    );
    assert!(
        dst.mode_of("/out").is_none(),
        "metadata failure still published the destination"
    );
}

#[tokio::test]
async fn qa_concurrent_transfers_to_the_same_destination_do_not_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.bin");
    let right = dir.path().join("right.bin");
    std::fs::write(&left, vec![0x11_u8; CHUNK * 2]).unwrap();
    std::fs::write(&right, vec![0x22_u8; CHUNK * 2]).unwrap();
    let dst = Arc::new(LabFs::new());
    *dst.delay.lock().unwrap() = Duration::from_millis(40);
    let mut jobs = Vec::new();
    for (id, source) in [(1_u64, path_str(&left)), (2, path_str(&right))] {
        jobs.push(TransferJob {
            id,
            direction: TransferDirection::Upload,
            source,
            destination: "/out/same.bin".to_string(),
            host: "h".to_string(),
            total: (CHUNK * 2) as u64,
            done: 0,
            state: TransferState::Queued,
            error: None,
            started: None,
        });
    }
    let (tx, mut rx) = mpsc::channel(32);
    run_queue(
        Arc::new(LocalFs),
        Arc::clone(&dst),
        jobs,
        CancellationToken::new(),
        tx,
    )
    .await;
    let mut states = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let TransferEvent::Finished { state, error, .. } = event {
            states.push((state, error));
        }
    }
    assert!(
        !dst.overlapped.load(Ordering::SeqCst),
        "two copies wrote the same destination concurrently; finishes={states:?} bytes={:?}",
        dst.snapshot("/out/same.bin").map(|bytes| (
            bytes.len(),
            bytes.first().copied(),
            bytes.get(CHUNK).copied()
        ))
    );
}

#[test]
fn qa_normalize_keeps_relative_parent_segments() {
    assert_eq!(normalize("../secret"), "../secret");
    assert_eq!(normalize("a/../../secret"), "../secret");
    assert_eq!(normalize("/a/../../secret"), "/secret");
    assert_eq!(normalize("/safe/../../etc/passwd"), "/etc/passwd");
}

#[tokio::test]
async fn qa_medium_file_matches_and_does_not_resident_the_whole_file() {
    let len = 32 * 1024 * 1024;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("medium.bin");
    let dst = dir.path().join("out.bin");
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&src).unwrap();
        let chunk = pattern(1024 * 1024);
        for _ in 0..(len / chunk.len()) {
            file.write_all(&chunk).unwrap();
        }
        file.flush().unwrap();
    }
    #[cfg(target_os = "linux")]
    let before = proc_kb("VmRSS:");
    let started = Instant::now();
    copy_ok(&src, &dst).await;
    let elapsed = started.elapsed();
    assert!(files_equal(&src, &dst), "32MiB mismatch");
    eprintln!(
        "QA medium {len} bytes in {:?} ({:.1} MiB/s)",
        elapsed,
        (len as f64 / elapsed.as_secs_f64()) / (1024.0 * 1024.0)
    );
    #[cfg(target_os = "linux")]
    {
        let after = proc_kb("VmRSS:");
        let grew = after.saturating_sub(before);
        eprintln!(
            "QA medium RSS before {before} KiB after {after} KiB grew {grew} KiB, HWM {} KiB",
            proc_kb("VmHWM:")
        );
        assert!(
            grew < 64 * 1024,
            "32MiB copy grew RSS by {grew} KiB; transfer looks buffered"
        );
    }
}

#[tokio::test]
async fn qa_large_file_streams() {
    if std::env::var("VCON_QA_LARGE").ok().as_deref() != Some("1") {
        eprintln!("SKIP qa_large_file_streams (set VCON_QA_LARGE=1)");
        return;
    }
    let len: u64 = 2 * 1024 * 1024 * 1024;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("large.bin");
    let dst = dir.path().join("out.bin");
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&src).unwrap();
        let chunk = pattern(1024 * 1024);
        let mut left = len;
        while left > 0 {
            let n = std::cmp::min(left, chunk.len() as u64) as usize;
            file.write_all(&chunk[..n]).unwrap();
            left -= n as u64;
        }
        file.flush().unwrap();
    }
    #[cfg(target_os = "linux")]
    let before = proc_kb("VmRSS:");
    let started = Instant::now();
    copy_ok(&src, &dst).await;
    let elapsed = started.elapsed();
    assert!(files_equal(&src, &dst), "2GiB mismatch");
    eprintln!(
        "QA large {len} bytes in {:?} ({:.1} MiB/s)",
        elapsed,
        (len as f64 / elapsed.as_secs_f64()) / (1024.0 * 1024.0)
    );
    #[cfg(target_os = "linux")]
    {
        let after = proc_kb("VmRSS:");
        let grew = after.saturating_sub(before);
        eprintln!(
            "QA large RSS before {before} KiB after {after} KiB grew {grew} KiB, HWM {} KiB",
            proc_kb("VmHWM:")
        );
        assert!(grew < 200 * 1024, "2GiB copy grew RSS by {grew} KiB");
    }
}

#[test]
fn qa_inventory_write_keeps_a_one_generation_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hosts.toml");
    write_atomic(&path, "version-one\n").unwrap();
    write_atomic(&path, "version-two\n").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "version-two\n");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("hosts.toml.bak")).unwrap(),
        "version-one\n"
    );
}

#[tokio::test]
async fn qa_sftp_arguments_do_not_carry_a_password_and_batch_unauthenticated_sessions() {
    let host = sample_host(22, None, BTreeMap::new());
    let args = sftp_arguments_auth(
        &host,
        std::slice::from_ref(&host),
        Some(5),
        false,
        None,
        false,
    );
    let joined = args.join(" ");
    assert!(
        joined.contains("BatchMode=yes"),
        "unauthenticated sftp argv missing BatchMode: {joined}"
    );
    assert!(
        !joined.contains("StrictHostKeyChecking=no"),
        "sftp argv disables host key checking: {joined}"
    );
    assert!(!joined.contains("s3cret-password"));
    let authed = sftp_arguments_auth(
        &host,
        std::slice::from_ref(&host),
        Some(5),
        false,
        None,
        true,
    );
    let authed_line = authed.join(" ");
    assert!(!authed_line.contains("BatchMode=yes"), "{authed_line}");
    assert!(!authed_line.contains("s3cret-password"));
    assert!(authed_line.contains("NumberOfPasswordPrompts=1"));
}

fn sample_host(
    port: u16,
    identity: Option<String>,
    ssh_options: BTreeMap<String, String>,
) -> EffectiveHost {
    EffectiveHost {
        name: "fixture".into(),
        address: "127.0.0.1".into(),
        user: "qa".into(),
        port,
        identity_file: identity,
        jump: None,
        tags: Vec::new(),
        notes: None,
        ssh_options,
        disabled: false,
        connect_timeout: Some(5),
        tunnels: Vec::new(),
    }
}

fn sftp_server() -> Option<PathBuf> {
    [
        "/usr/lib/openssh/sftp-server",
        "/usr/libexec/openssh/sftp-server",
        "/usr/libexec/sftp-server",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

struct Sshd {
    dir: tempfile::TempDir,
    port: u16,
    key: PathBuf,
    known: PathBuf,
    user: String,
    child: std::process::Child,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Sshd {
    fn start() -> Option<Self> {
        if !Path::new("/usr/sbin/sshd").is_file() {
            eprintln!("SKIP sshd missing");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let host_key = dir.path().join("host");
        let user_key = dir.path().join("id");
        assert!(
            std::process::Command::new("ssh-keygen")
                .args(["-t", "ed25519", "-f", &path_str(&host_key), "-N", "", "-q"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            std::process::Command::new("ssh-keygen")
                .args(["-t", "ed25519", "-f", &path_str(&user_key), "-N", "", "-q"])
                .status()
                .unwrap()
                .success()
        );
        let user = String::from_utf8(
            std::process::Command::new("id")
                .arg("-un")
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let user = user.trim().to_string();
        let authorized = dir.path().join("authorized_keys");
        std::fs::copy(dir.path().join("id.pub"), &authorized).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&authorized, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::set_permissions(&user_key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let config = dir.path().join("sshd_config");
        std::fs::write(
            &config,
            format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile {}\nAuthorizedKeysFile {}\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\nPubkeyAuthentication yes\nAllowUsers {user}\nSubsystem sftp internal-sftp\n",
                host_key.display(),
                dir.path().join("sshd.pid").display(),
                authorized.display(),
            ),
        )
        .unwrap();
        let log = std::fs::File::create(dir.path().join("sshd.log")).unwrap();
        let child = std::process::Command::new("/usr/sbin/sshd")
            .args(["-f", &path_str(&config), "-D", "-e"])
            .stdout(std::process::Stdio::from(log.try_clone().unwrap()))
            .stderr(std::process::Stdio::from(log))
            .spawn()
            .unwrap();
        let mut ready = false;
        for _ in 0..50 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !ready {
            panic!(
                "sshd: {}",
                std::fs::read_to_string(dir.path().join("sshd.log")).unwrap_or_default()
            );
        }
        let pubkey = String::from_utf8(
            std::process::Command::new("ssh-keygen")
                .args(["-y", "-f", &path_str(&host_key)])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let known = dir.path().join("known_hosts");
        std::fs::write(&known, format!("[127.0.0.1]:{port} {}", pubkey.trim())).unwrap();
        Some(Self {
            dir,
            port,
            key: user_key,
            known,
            user,
            child,
        })
    }

    fn host(&self) -> EffectiveHost {
        let mut options = BTreeMap::new();
        options.insert("UserKnownHostsFile".into(), path_str(&self.known));
        options.insert("GlobalKnownHostsFile".into(), "/dev/null".into());
        options.insert("StrictHostKeyChecking".into(), "yes".into());
        options.insert("IdentitiesOnly".into(), "yes".into());
        options.insert("HashKnownHosts".into(), "no".into());
        EffectiveHost {
            name: "fixture".into(),
            address: "127.0.0.1".into(),
            user: self.user.clone(),
            port: self.port,
            identity_file: Some(path_str(&self.key)),
            jump: None,
            tags: Vec::new(),
            notes: None,
            ssh_options: options,
            disabled: false,
            connect_timeout: Some(5),
            tunnels: Vec::new(),
        }
    }
}

async fn connect_sftp(sshd: &Sshd) -> SftpFs {
    let host = sshd.host();
    let args = sftp_arguments_auth(
        &host,
        std::slice::from_ref(&host),
        Some(5),
        false,
        None,
        false,
    );
    // A developer `~/.ssh/config` must not redirect this loopback fixture.
    let home = path_str(sshd.dir.path());
    SftpFs::connect_with_env(find_ssh().expect("ssh"), args, &[("HOME".into(), home)])
        .await
        .expect("sftp connect")
}

#[tokio::test]
async fn qa_sftp_roundtrip_matrix_over_local_sshd() {
    let Some(sshd) = Sshd::start() else {
        return;
    };
    let session = connect_sftp(&sshd).await;
    let dir = tempfile::tempdir().unwrap();
    let remote = dir.path().join("remote");
    std::fs::create_dir(&remote).unwrap();
    for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK + 3] {
        let src = dir.path().join(format!("src-{len}.bin"));
        let dst = remote.join(format!("dst-{len}.bin"));
        let bytes = pattern(len);
        std::fs::write(&src, &bytes).unwrap();
        copy_file(
            &LocalFs,
            &session,
            &path_str(&src),
            &path_str(&dst),
            &CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap_or_else(|error| panic!("upload {len}: {error}"));
        assert_eq!(std::fs::read(&dst).unwrap(), bytes, "upload {len}");
        assert!(!Path::new(&format!("{}.part", path_str(&dst))).exists());
        let back = dir.path().join(format!("back-{len}.bin"));
        copy_file(
            &session,
            &LocalFs,
            &path_str(&dst),
            &path_str(&back),
            &CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap_or_else(|error| panic!("download {len}: {error}"));
        assert_eq!(std::fs::read(&back).unwrap(), bytes, "download {len}");
    }
    let named = dir.path().join("my file 文件.txt");
    std::fs::write(&named, "name").unwrap();
    let named_dst = remote.join("my file 文件.txt");
    copy_file(
        &LocalFs,
        &session,
        &path_str(&named),
        &path_str(&named_dst),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await
    .expect("unicode upload");
    assert_eq!(std::fs::read(&named_dst).unwrap(), b"name");

    let tree = dir.path().join("proj");
    std::fs::create_dir_all(tree.join("sub")).unwrap();
    std::fs::write(tree.join("a.txt"), "A").unwrap();
    std::fs::write(tree.join("sub/b.txt"), "B").unwrap();
    let tree_dest = remote.join("tree");
    std::fs::create_dir(&tree_dest).unwrap();
    let planned = plan_copies(&LocalFs, &session, &path_str(&tree), &path_str(&tree_dest))
        .await
        .unwrap();
    for (src, destination, _) in planned.files {
        copy_file(
            &LocalFs,
            &session,
            &src,
            &destination,
            &CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap();
    }
    assert_eq!(std::fs::read(tree_dest.join("proj/a.txt")).unwrap(), b"A");
    assert_eq!(
        std::fs::read(tree_dest.join("proj/sub/b.txt")).unwrap(),
        b"B"
    );

    let big = dir.path().join("cancel.bin");
    std::fs::write(&big, pattern(CHUNK * 3)).unwrap();
    let cancel_dst = remote.join("cancel.bin");
    std::fs::write(&cancel_dst, b"OLD").unwrap();
    let cancel = CancellationToken::new();
    let flag = cancel.clone();
    let err = copy_file(
        &LocalFs,
        &session,
        &path_str(&big),
        &path_str(&cancel_dst),
        &cancel,
        |done, _| {
            if done > 0 {
                flag.cancel();
            }
        },
    )
    .await
    .expect_err("sftp cancel");
    assert!(err.to_string().contains("cancelled"), "{err}");
    assert_eq!(std::fs::read(&cancel_dst).unwrap(), b"OLD");
    assert!(!Path::new(&format!("{}.part", path_str(&cancel_dst))).exists());
    session.close().await.expect("close sftp");
}

#[tokio::test]
async fn qa_sftp_symlink_is_not_followed() {
    let Some(sshd) = Sshd::start() else {
        return;
    };
    let session = connect_sftp(&sshd).await;
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret");
    std::fs::write(&secret, b"TOP-SECRET").unwrap();
    let link = dir.path().join("link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&secret, &link).unwrap();
    let dest = dir.path().join("copied");
    let result = copy_file(
        &session,
        &LocalFs,
        &path_str(&link),
        &path_str(&dest),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await;
    if dest.exists() {
        let bytes = std::fs::read(&dest).unwrap_or_default();
        let meta = std::fs::symlink_metadata(&dest).unwrap();
        assert!(
            meta.file_type().is_symlink() && bytes != b"TOP-SECRET",
            "sftp download followed the symlink ({result:?}, bytes {:?})",
            String::from_utf8_lossy(&bytes)
        );
    }
    session.close().await.ok();
}

#[tokio::test]
async fn qa_sftp_handshake_rejects_garbage_and_does_not_hang() {
    let hung = tokio::time::timeout(Duration::from_secs(5), async {
        SftpFs::connect_with_env(
            PathBuf::from("/bin/sh"),
            vec!["-c".into(), "printf 'this-is-not-sftp\\n'".into()],
            &[],
        )
        .await
    })
    .await;
    assert!(hung.is_ok(), "malformed SFTP handshake hung");
    assert!(
        hung.unwrap().is_err(),
        "malformed SFTP handshake was accepted"
    );
}

#[tokio::test]
async fn qa_sftp_handshake_times_out_on_a_stalled_peer() {
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        SftpFs::connect_with_env_timeout(
            PathBuf::from("/bin/sleep"),
            vec!["30".into()],
            &[],
            Duration::from_secs(1),
        ),
    )
    .await;
    assert!(
        result.is_ok(),
        "SFTP handshake was still pending after 3s against a peer that never speaks"
    );
    assert!(result.unwrap().is_err());
}

#[tokio::test]
async fn qa_sftp_connection_drop_does_not_commit_a_partial_file() {
    let Some(server) = sftp_server() else {
        eprintln!("SKIP sftp-server missing");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("drop.py");
    let body = r#"#!/usr/bin/env python3
import os, select, subprocess, sys
limit = int(os.environ.get("QA_DROP_AFTER", "120000"))
proc = subprocess.Popen([SFTP_SERVER], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
got = 0
client = sys.stdin.fileno()
server_out = proc.stdout.fileno()
while True:
    readable, _, _ = select.select([client, server_out], [], [], 10)
    if not readable:
        proc.kill()
        raise SystemExit
    for fd in readable:
        if fd == client:
            data = os.read(client, 65536)
            if not data:
                proc.kill()
                raise SystemExit
            got += len(data)
            proc.stdin.write(data)
            proc.stdin.flush()
            if got >= limit:
                proc.kill()
                raise SystemExit
        else:
            data = os.read(server_out, 65536)
            if not data:
                raise SystemExit
            os.write(sys.stdout.fileno(), data)
"#
    .replace("SFTP_SERVER", &format!("{:?}", server));
    std::fs::write(&script, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let session = SftpFs::connect_with_env(
        PathBuf::from("python3"),
        vec![path_str(&script)],
        &[("QA_DROP_AFTER".into(), "120000".into())],
    )
    .await
    .expect("proxy handshake");
    let src = dir.path().join("big.bin");
    let payload = pattern(2 * 1024 * 1024);
    std::fs::write(&src, &payload).unwrap();
    let sentinel = dir.path().join("keep-local.txt");
    std::fs::write(&sentinel, b"UNTOUCHED").unwrap();
    let dst = dir.path().join("remote.bin");
    let part = PathBuf::from(format!("{}.part", path_str(&dst)));
    let result = copy_file(
        &LocalFs,
        &session,
        &path_str(&src),
        &path_str(&dst),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await;
    let error = result.expect_err("a dropped connection must fail the copy");
    let message = error.to_string().to_ascii_lowercase();
    assert!(
        message.contains("connection"),
        "error should mention the dropped connection: {error}"
    );
    assert!(!dst.exists(), "partial destination was committed ({error})");
    assert_eq!(
        std::fs::read(&sentinel).unwrap(),
        b"UNTOUCHED",
        "cleanup touched an unrelated local file"
    );
    session.close().await.ok();
    // The dead server can leave its `.part`. The next transfer must replace it.
    if !part.exists() {
        std::fs::write(&part, b"STALE").unwrap();
    }
    let retry = SftpFs::connect_with_env(server, Vec::new(), &[])
        .await
        .expect("retry handshake");
    copy_file(
        &LocalFs,
        &retry,
        &path_str(&src),
        &path_str(&dst),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await
    .expect("retry after a stale .part");
    assert_eq!(std::fs::read(&dst).unwrap(), payload);
    assert!(
        !part.exists(),
        "successful retry left the stale .part in place"
    );
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"UNTOUCHED");
    retry.close().await.ok();
}

#[tokio::test]
async fn qa_disk_full_surfaces_and_removes_the_part() {
    let dir = tempfile::tempdir().unwrap();
    let mnt = dir.path().join("mnt");
    std::fs::create_dir(&mnt).unwrap();
    let mounted = std::process::Command::new("sudo")
        .args([
            "-n",
            "mount",
            "-t",
            "tmpfs",
            "-o",
            "size=1m",
            "tmpfs",
            &path_str(&mnt),
        ])
        .status();
    let Ok(status) = mounted else {
        eprintln!("SKIP disk full: sudo mount unavailable");
        return;
    };
    if !status.success() {
        eprintln!("SKIP disk full: mount failed with {status}");
        return;
    }
    struct Umount(PathBuf);
    impl Drop for Umount {
        fn drop(&mut self) {
            let path = self.0.to_string_lossy().into_owned();
            let _ = std::process::Command::new("sudo")
                .args(["-n", "umount", &path])
                .status();
        }
    }
    let _umount = Umount(mnt.clone());
    let src = dir.path().join("big.bin");
    std::fs::write(&src, vec![7_u8; 3 * 1024 * 1024]).unwrap();
    let dst = mnt.join("out.bin");
    let err = copy_file(
        &LocalFs,
        &LocalFs,
        &path_str(&src),
        &path_str(&dst),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await
    .expect_err("disk full must fail");
    let message = err.to_string().to_ascii_lowercase();
    assert!(
        message.contains("space") || message.contains("enospc") || message.contains("os error 28"),
        "disk full error was {err}"
    );
    assert!(
        !dst.exists(),
        "partial final file left on a full filesystem"
    );
    assert!(
        !Path::new(&format!("{}.part", path_str(&dst))).exists(),
        "partial .part left on a full filesystem"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn qa_many_copies_do_not_leak_file_descriptors() {
    let dir = tempfile::tempdir().unwrap();
    let before = std::fs::read_dir("/proc/self/fd").unwrap().count();
    for index in 0..30 {
        let src = dir.path().join(format!("s{index}"));
        let dst = dir.path().join(format!("d{index}"));
        std::fs::write(&src, pattern(128)).unwrap();
        copy_ok(&src, &dst).await;
    }
    let after = std::fs::read_dir("/proc/self/fd").unwrap().count();
    assert!(
        after < before + 20,
        "fd count {before} -> {after} after 30 copies"
    );
}

proptest::proptest! {
    #[test]
    fn qa_random_bytes_roundtrip(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..8192)) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let src = dir.path().join("in");
            let dst = dir.path().join("out");
            std::fs::write(&src, &data).unwrap();
            copy_ok(&src, &dst).await;
            assert_eq!(std::fs::read(&dst).unwrap(), data);
        });
    }

    #[test]
    fn qa_normalize_does_not_panic(chars in proptest::collection::vec(proptest::prelude::any::<char>(), 0..24)) {
        let raw: String = chars.into_iter().collect();
        let normalized = normalize(&raw);
        if raw.starts_with('/') {
            assert!(normalized.starts_with('/'), "{raw} -> {normalized}");
        }
        let _ = crate::infra::remote_fs::join(&normalized, "name");
        let _ = crate::infra::remote_fs::parent_of(&normalized);
        let _ = crate::infra::remote_fs::file_name(&normalized);
    }
}

fn arm_hostile(name: &str, payload: &[u8]) -> LabFs {
    let src = LabFs::new();
    src.insert_dir("/inbox");
    *src.payload.lock().unwrap() = payload.to_vec();
    src.stat_missing_as_file.store(true, Ordering::SeqCst);
    *src.hostile.lock().unwrap() = Some((
        "/inbox".to_string(),
        vec![file_entry(name, payload.len() as u64)],
    ));
    src
}

async fn copy_hostile(name: &str, dest: &Path) -> Result<(), Error> {
    let src = arm_hostile(name, b"PWNED");
    let planned = plan_copies(&src, &LocalFs, "/inbox", &path_str(dest)).await?;
    for (source, destination, _) in planned.files {
        copy_file(
            &src,
            &LocalFs,
            &source,
            &destination,
            &CancellationToken::new(),
            |_, _| {},
        )
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn qa_retest_symlink_component_in_listing_name_cannot_escape() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let dest = dir.path().join("dest");
    let inbox = dest.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::os::unix::fs::symlink(&outside, inbox.join("sub")).unwrap();
    let copied = copy_hostile("sub/pwned.txt", &dest).await;
    let leaked = outside.join("pwned.txt");
    assert!(
        !leaked.exists(),
        "listing name sub/pwned.txt wrote through a symlinked directory ({copied:?}) at {}",
        leaked.display()
    );
}

#[tokio::test]
async fn qa_retest_absolute_backslash_and_dot_names_stay_inside_dest() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let outside_marker = dir.path().join("escaped.bin");
    for name in [
        "/tmp/escaped.bin",
        r"..\..\escaped.bin",
        "%2e%2e/%2e%2e/escaped.bin",
        ".",
    ] {
        let _ = copy_hostile(name, &dest).await;
        assert!(
            !outside_marker.exists(),
            "name {name:?} created {}",
            outside_marker.display()
        );
    }
    // Payload must not appear as a sibling of dest.
    let mut leaked = Vec::new();
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let entry = entry.unwrap();
        if entry.path() == dest {
            continue;
        }
        if entry.path().is_file() {
            leaked.push(entry.path());
        }
    }
    assert!(
        leaked.is_empty(),
        "hostile names wrote siblings of dest: {leaked:?}"
    );
}

#[tokio::test]
async fn qa_retest_failed_cleanup_does_not_unlink_a_local_part_file() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("notes.txt");
    let part = dir.path().join("notes.txt.part");
    std::fs::write(&part, b"LOCAL-KEEP").unwrap();
    let src = LabFs::new();
    src.insert_file("/in", b"remote-body");
    let dst = LabFs::new();
    dst.fail_meta.store(true, Ordering::SeqCst);
    dst.fail_remove.store(true, Ordering::SeqCst);
    let _ = copy_file(
        &src,
        &dst,
        "/in",
        &path_str(&dest),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await;
    let kept = std::fs::read(&part).unwrap_or_default();
    assert_eq!(
        kept, b"LOCAL-KEEP",
        "failed transfer cleanup deleted a local file at the remote .part path"
    );
}

#[tokio::test]
async fn qa_retest_equivalent_destination_paths_do_not_interleave() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.bin");
    let right = dir.path().join("right.bin");
    std::fs::write(&left, vec![0x11_u8; CHUNK * 2]).unwrap();
    std::fs::write(&right, vec![0x22_u8; CHUNK * 2]).unwrap();
    let dst = Arc::new(LabFs::new());
    *dst.delay.lock().unwrap() = Duration::from_millis(40);
    let jobs = vec![
        TransferJob {
            id: 1,
            direction: TransferDirection::Upload,
            source: path_str(&left),
            destination: "/out/same.bin".to_string(),
            host: "h".to_string(),
            total: (CHUNK * 2) as u64,
            done: 0,
            state: TransferState::Queued,
            error: None,
            started: None,
        },
        TransferJob {
            id: 2,
            direction: TransferDirection::Upload,
            source: path_str(&right),
            destination: "/out/./same.bin".to_string(),
            host: "h".to_string(),
            total: (CHUNK * 2) as u64,
            done: 0,
            state: TransferState::Queued,
            error: None,
            started: None,
        },
    ];
    let (tx, _rx) = mpsc::channel(32);
    run_queue(
        Arc::new(LocalFs),
        Arc::clone(&dst),
        jobs,
        CancellationToken::new(),
        tx,
    )
    .await;
    assert!(
        !dst.overlapped.load(Ordering::SeqCst),
        "equivalent destination paths were not serialized; bytes={:?}",
        dst.snapshot("/out/same.bin").map(|bytes| (
            bytes.len(),
            bytes.first().copied(),
            bytes.get(CHUNK).copied()
        ))
    );
}

#[tokio::test]
async fn qa_retest_sftp_slow_handshake_is_not_killed() {
    let Some(server) = sftp_server() else {
        eprintln!("SKIP sftp-server missing");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("slow.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nsleep 3\nexec {}\n", server.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let connected = tokio::time::timeout(
        Duration::from_secs(15),
        SftpFs::connect_with_env(script, Vec::new(), &[]),
    )
    .await;
    match connected {
        Ok(Ok(_session)) => {}
        Ok(Err(error)) => panic!(
            "a peer that speaks SFTP after 3s (under the 30s connect budget) was rejected: {error}"
        ),
        Err(_) => panic!("slow SFTP handshake still pending after 15s"),
    }
}

#[tokio::test]
async fn qa_retest_declared_u64_max_does_not_copy_forever() {
    let src = LabFs::new();
    src.insert_file("/in", b"");
    *src.force_size.lock().unwrap() = Some(u64::MAX);
    *src.read_mode.lock().unwrap() = ReadMode::PrefixThenEof;
    *src.payload.lock().unwrap() = Vec::new();
    let dst = LabFs::new();
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        copy_file(
            &src,
            &dst,
            "/in",
            "/out",
            &CancellationToken::new(),
            |_, _| {},
        ),
    )
    .await;
    assert!(
        result.is_ok(),
        "u64::MAX size hung the copy ({:?})",
        started.elapsed()
    );
    assert!(
        result.unwrap().is_err(),
        "u64::MAX size with an immediate EOF was reported as success"
    );
}

/// `~/Downloads` → an external volume. The link's parent is the home directory
/// and the target is outside that parent. The user picked this destination.
#[cfg(unix)]
#[tokio::test]
async fn qa_retest3_symlink_root_outside_its_parent_is_usable() {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let volume = base.path().join("Volumes/Ext/Downloads");
    std::fs::create_dir_all(&volume).unwrap();
    let link = home.join("Downloads");
    std::os::unix::fs::symlink(&volume, &link).unwrap();
    let src = base.path().join("payload.bin");
    std::fs::write(&src, b"picked-by-user").unwrap();
    let dest = link.join("payload.bin");
    let result = copy_file(
        &LocalFs,
        &LocalFs,
        &path_str(&src),
        &path_str(&dest),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await;
    assert!(
        result.is_ok(),
        "a user-picked destination root whose target leaves the link's parent must succeed: {result:?}"
    );
    assert_eq!(
        std::fs::read(volume.join("payload.bin")).unwrap(),
        b"picked-by-user"
    );
}

/// A symlink created under the resolved destination must not receive the file.
#[cfg(unix)]
#[tokio::test]
async fn qa_retest3_child_symlink_cannot_escape_the_resolved_root() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("Downloads");
    let secret = base.path().join("secret");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir(&secret).unwrap();
    std::os::unix::fs::symlink(&secret, root.join("leak")).unwrap();
    let src = base.path().join("payload.bin");
    std::fs::write(&src, b"must-stay").unwrap();
    let err = copy_file(
        &LocalFs,
        &LocalFs,
        &path_str(&src),
        &path_str(&root.join("leak/pwned.txt")),
        &CancellationToken::new(),
        |_, _| {},
    )
    .await
    .expect_err("a child symlink that leaves the resolved root must be refused");
    assert!(
        !secret.join("pwned.txt").exists() && !secret.join("pwned.txt.part").exists(),
        "child symlink escaped the resolved root ({err})"
    );
}

/// `create`, `write_at`, and `set_meta` must not follow a symlink at the final
/// path component. A parent swapped after the file is opened must not redirect
/// the bytes already going to that descriptor.
#[cfg(unix)]
#[tokio::test]
async fn qa_retest3_nofollow_final_component_and_parent_swap_after_open() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret"), b"SECRET").unwrap();
    let link = dir.path().join("out.bin");
    std::os::unix::fs::symlink(outside.join("secret"), &link).unwrap();
    let link_str = path_str(&link);
    let created = LocalFs.create(&link_str).await;
    assert!(
        created.is_err(),
        "create followed a symlink at the final component ({created:?})"
    );
    assert_eq!(std::fs::read(outside.join("secret")).unwrap(), b"SECRET");
    let wrote = LocalFs.write_at(&link_str, 0, b"PWNED").await;
    assert!(
        wrote.is_err(),
        "write_at followed a symlink at the final component ({wrote:?})"
    );
    let meta = LocalFs.set_meta(&link_str, Some(0o100777), None).await;
    assert!(
        meta.is_err(),
        "set_meta followed a symlink at the final component ({meta:?})"
    );
    assert_eq!(std::fs::read(outside.join("secret")).unwrap(), b"SECRET");

    let parent = dir.path().join("inbox");
    std::fs::create_dir(&parent).unwrap();
    let part = parent.join("file.bin.part");
    let part_str = path_str(&part);
    LocalFs.create(&part_str).await.unwrap();
    let held = dir.path().join("inbox-real");
    std::fs::rename(&parent, &held).unwrap();
    std::os::unix::fs::symlink(&outside, &parent).unwrap();
    LocalFs.write_at(&part_str, 0, b"BYTES").await.unwrap();
    LocalFs
        .set_meta(&part_str, Some(0o100600), None)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(held.join("file.bin.part")).unwrap(),
        b"BYTES",
        "a parent swapped after open redirected the held descriptor"
    );
    assert!(
        !outside.join("file.bin.part").exists(),
        "bytes landed in the swapped parent symlink"
    );
    let _ = LocalFs.release(&part_str).await;
}

/// Opening the parent follows a symlink. `copy_file` refuses that shape when
/// the link is already in place; this checks a swap that lands on the open.
#[cfg(unix)]
#[tokio::test]
async fn qa_retest3_parent_swapped_for_a_symlink_during_copy() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let outside = dir.path().join("Volumes/Ext");
    std::fs::create_dir_all(&outside).unwrap();
    let via = home.join("Downloads");
    std::os::unix::fs::symlink(&outside, &via).unwrap();
    let planted = via.join("direct.bin");
    LocalFs.create(&path_str(&planted)).await.unwrap();
    LocalFs
        .write_at(&path_str(&planted), 0, b"VIA")
        .await
        .unwrap();
    let _ = LocalFs.release(&path_str(&planted)).await;
    assert_eq!(
        std::fs::read(outside.join("direct.bin")).unwrap(),
        b"VIA",
        "create/write_at did not follow a parent symlink"
    );
    std::fs::remove_file(&via).unwrap();
    std::fs::remove_file(outside.join("direct.bin")).unwrap();

    let parent = home.join("inbox");
    let held = home.join("inbox-real");
    std::fs::create_dir(&parent).unwrap();
    let src = dir.path().join("src.bin");
    std::fs::write(&src, b"race-bytes").unwrap();
    let dest = parent.join("race.bin");
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let parent_flip = parent.clone();
    let held_flip = held.clone();
    let outside_flip = outside.clone();
    let spinner = std::thread::spawn(move || {
        while !stop_flag.load(Ordering::Relaxed) {
            let _ = std::fs::rename(&parent_flip, &held_flip);
            let _ = std::os::unix::fs::symlink(&outside_flip, &parent_flip);
            let _ = std::fs::remove_file(&parent_flip);
            let _ = std::fs::rename(&held_flip, &parent_flip);
        }
    });
    for _ in 0..400 {
        let _ = copy_file(
            &LocalFs,
            &LocalFs,
            &path_str(&src),
            &path_str(&dest),
            &CancellationToken::new(),
            |_, _| {},
        )
        .await;
        let leaked = outside.join("race.bin").exists() || outside.join("race.bin.part").exists();
        if leaked {
            stop.store(true, Ordering::Relaxed);
            let _ = spinner.join();
            let bytes = std::fs::read(outside.join("race.bin"))
                .or_else(|_| std::fs::read(outside.join("race.bin.part")))
                .unwrap_or_default();
            panic!(
                "parent swap during copy wrote {} bytes outside the destination",
                bytes.len()
            );
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = spinner.join();
}
