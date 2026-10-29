//! In-memory filesystem for tests.

use super::{DirEntry, EntryKind, RemoteFs, file_name, join, normalize, parent_of};
use crate::error::Error;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone)]
enum Node {
    File {
        data: Vec<u8>,
        mode: u32,
        mtime: SystemTime,
    },
    Dir {
        mode: u32,
        mtime: SystemTime,
    },
}

/// Mutable tree rooted at `/`.
#[derive(Debug, Default)]
pub struct FakeFs {
    nodes: Mutex<BTreeMap<String, Node>>,
}

impl FakeFs {
    /// Empty tree with only `/`.
    pub fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            "/".to_string(),
            Node::Dir {
                mode: 0o755,
                mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
            },
        );
        Self {
            nodes: Mutex::new(nodes),
        }
    }

    /// Insert a file for test setup.
    pub fn insert_file(&self, path: &str, data: impl Into<Vec<u8>>) {
        let path = normalize(path);
        let mut nodes = self.nodes.lock().expect("fake fs lock");
        ensure_parents(&mut nodes, &path);
        nodes.insert(
            path,
            Node::File {
                data: data.into(),
                mode: 0o644,
                mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(10),
            },
        );
    }

    /// Insert an empty directory.
    pub fn insert_dir(&self, path: &str) {
        let path = normalize(path);
        let mut nodes = self.nodes.lock().expect("fake fs lock");
        ensure_parents(&mut nodes, &path);
        nodes.insert(
            path,
            Node::Dir {
                mode: 0o755,
                mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(10),
            },
        );
    }
}

impl RemoteFs for FakeFs {
    async fn list(&self, dir: &str) -> Result<Vec<DirEntry>, Error> {
        let dir = normalize(dir);
        let nodes = self.nodes.lock().expect("fake fs lock");
        match nodes.get(&dir) {
            Some(Node::Dir { .. }) => {}
            Some(_) => {
                return Err(Error::Message(format!("{dir} is not a directory")));
            }
            None => return Err(Error::Message(format!("no such directory: {dir}"))),
        }
        let prefix = if dir == "/" {
            "/".to_string()
        } else {
            format!("{dir}/")
        };
        let mut entries = Vec::new();
        for (path, node) in nodes.iter() {
            if path == &dir {
                continue;
            }
            if !path.starts_with(&prefix) {
                continue;
            }
            let rest = &path[prefix.len()..];
            if rest.is_empty() || rest.contains('/') {
                continue;
            }
            entries.push(to_entry(rest, node));
        }
        Ok(entries)
    }

    async fn stat(&self, path: &str) -> Result<DirEntry, Error> {
        let path = normalize(path);
        let nodes = self.nodes.lock().expect("fake fs lock");
        let node = nodes
            .get(&path)
            .ok_or_else(|| Error::Message(format!("no such path: {path}")))?;
        Ok(to_entry(file_name(&path), node))
    }

    async fn mkdir(&self, path: &str) -> Result<(), Error> {
        let path = normalize(path);
        let mut nodes = self.nodes.lock().expect("fake fs lock");
        if nodes.contains_key(&path) {
            return Err(Error::Message(format!("already exists: {path}")));
        }
        ensure_parents(&mut nodes, &path);
        nodes.insert(
            path,
            Node::Dir {
                mode: 0o755,
                mtime: SystemTime::now(),
            },
        );
        Ok(())
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let from = normalize(from);
        let to = normalize(to);
        let mut nodes = self.nodes.lock().expect("fake fs lock");
        let node = nodes
            .remove(&from)
            .ok_or_else(|| Error::Message(format!("no such path: {from}")))?;
        ensure_parents(&mut nodes, &to);
        // Move children of directories.
        let child_prefix = if from == "/" {
            "/".to_string()
        } else {
            format!("{from}/")
        };
        let mut children = Vec::new();
        for key in nodes.keys() {
            if key.starts_with(&child_prefix) {
                children.push(key.clone());
            }
        }
        for key in children {
            let suffix = &key[child_prefix.len()..];
            let new_key = join(&to, suffix);
            if let Some(child) = nodes.remove(&key) {
                nodes.insert(new_key, child);
            }
        }
        nodes.insert(to, node);
        Ok(())
    }

    async fn remove_file(&self, path: &str) -> Result<(), Error> {
        let path = normalize(path);
        let mut nodes = self.nodes.lock().expect("fake fs lock");
        match nodes.get(&path) {
            Some(Node::File { .. }) => {
                nodes.remove(&path);
                Ok(())
            }
            Some(_) => Err(Error::Message(format!("{path} is not a file"))),
            None => Err(Error::Message(format!("no such file: {path}"))),
        }
    }

    async fn remove_dir(&self, path: &str) -> Result<(), Error> {
        let path = normalize(path);
        if path == "/" {
            return Err(Error::Message("cannot remove /".to_string()));
        }
        let mut nodes = self.nodes.lock().expect("fake fs lock");
        let prefix = format!("{path}/");
        if nodes.keys().any(|key| key.starts_with(&prefix)) {
            return Err(Error::Message(format!("directory not empty: {path}")));
        }
        match nodes.get(&path) {
            Some(Node::Dir { .. }) => {
                nodes.remove(&path);
                Ok(())
            }
            Some(_) => Err(Error::Message(format!("{path} is not a directory"))),
            None => Err(Error::Message(format!("no such directory: {path}"))),
        }
    }

    async fn realpath(&self, path: &str) -> Result<String, Error> {
        Ok(normalize(path))
    }

    async fn read_at(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        let path = normalize(path);
        let nodes = self.nodes.lock().expect("fake fs lock");
        match nodes.get(&path) {
            Some(Node::File { data, .. }) => {
                let start = offset as usize;
                if start >= data.len() {
                    return Ok(Vec::new());
                }
                let end = (start + len).min(data.len());
                Ok(data[start..end].to_vec())
            }
            _ => Err(Error::Message(format!("no such file: {path}"))),
        }
    }

    async fn create(&self, path: &str) -> Result<(), Error> {
        let path = normalize(path);
        let mut nodes = self.nodes.lock().expect("fake fs lock");
        ensure_parents(&mut nodes, &path);
        nodes.insert(
            path,
            Node::File {
                data: Vec::new(),
                mode: 0o644,
                mtime: SystemTime::now(),
            },
        );
        Ok(())
    }

    async fn write_at(&self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        let path = normalize(path);
        let mut nodes = self.nodes.lock().expect("fake fs lock");
        match nodes.get_mut(&path) {
            Some(Node::File {
                data: body, mtime, ..
            }) => {
                let start = offset as usize;
                if body.len() < start {
                    body.resize(start, 0);
                }
                let end = start + data.len();
                if body.len() < end {
                    body.resize(end, 0);
                }
                body[start..end].copy_from_slice(data);
                *mtime = SystemTime::now();
                Ok(())
            }
            _ => Err(Error::Message(format!("no such file: {path}"))),
        }
    }

    async fn set_meta(
        &self,
        path: &str,
        mode: Option<u32>,
        mtime: Option<SystemTime>,
    ) -> Result<(), Error> {
        let path = normalize(path);
        let mut nodes = self.nodes.lock().expect("fake fs lock");
        match nodes.get_mut(&path) {
            Some(Node::File {
                mode: m, mtime: t, ..
            })
            | Some(Node::Dir { mode: m, mtime: t }) => {
                if let Some(mode) = mode {
                    *m = mode;
                }
                if let Some(mtime) = mtime {
                    *t = mtime;
                }
                Ok(())
            }
            None => Err(Error::Message(format!("no such path: {path}"))),
        }
    }
}

fn ensure_parents(nodes: &mut BTreeMap<String, Node>, path: &str) {
    let mut cursor = path.to_string();
    let mut missing = Vec::new();
    while let Some(parent) = parent_of(&cursor) {
        if nodes.contains_key(&parent) {
            break;
        }
        missing.push(parent.clone());
        cursor = parent;
    }
    for parent in missing.into_iter().rev() {
        nodes.insert(
            parent,
            Node::Dir {
                mode: 0o755,
                mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
            },
        );
    }
}

fn to_entry(name: &str, node: &Node) -> DirEntry {
    match node {
        Node::File { data, mode, mtime } => DirEntry {
            name: name.to_string(),
            kind: EntryKind::File,
            size: data.len() as u64,
            mtime: Some(*mtime),
            mode: Some(*mode),
            target: None,
        },
        Node::Dir { mode, mtime } => DirEntry {
            name: name.to_string(),
            kind: EntryKind::Dir,
            size: 0,
            mtime: Some(*mtime),
            mode: Some(*mode),
            target: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_mkdir_list_and_copy_bytes() {
        let fs = FakeFs::new();
        fs.mkdir("/etc").await.unwrap();
        fs.create("/etc/nginx.conf").await.unwrap();
        fs.write_at("/etc/nginx.conf", 0, b"server {}")
            .await
            .unwrap();
        let entries = fs.list("/etc").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "nginx.conf");
        assert_eq!(
            fs.read_at("/etc/nginx.conf", 0, 64).await.unwrap(),
            b"server {}"
        );
    }
}
