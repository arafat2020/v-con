//! Embedded PTY sessions: spawn `ssh -t` and parse output with vt100.
//!
//! The TUI owns a [`SessionManager`] in the event loop. Reader threads push
//! coalesced [`SessionEvent`]s so a flood of output does not flood the UI
//! channel. Screen state is shared via `Arc<Mutex<vt100::Parser>>`.

use crate::error::Error;
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tokio::sync::mpsc;

/// Opaque handle for one live or ended session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

impl SessionId {
    /// Build an id for tests.
    #[cfg(test)]
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Notifications from reader / waiter threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// New bytes were parsed. At most one pending notification per session.
    Output(SessionId),
    /// The child exited. The final screen stays available until [`SessionManager::close`].
    Exited {
        /// Session that ended.
        id: SessionId,
        /// Process exit code when known.
        code: Option<u32>,
    },
}

/// Owns PTY children, parsers, and writer handles.
pub struct SessionManager {
    next_id: AtomicU64,
    scrollback_lines: usize,
    events: mpsc::UnboundedSender<SessionEvent>,
    sessions: Mutex<HashMap<SessionId, Session>>,
}

struct Session {
    #[allow(dead_code)]
    host_name: String,
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    killer: Mutex<Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
    pending: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    #[allow(dead_code)]
    exit_code: Arc<Mutex<Option<u32>>>,
    reader: Option<JoinHandle<()>>,
    waiter: Option<JoinHandle<()>>,
}

impl SessionManager {
    /// Create a manager that sends events on `events`.
    pub fn new(events: mpsc::UnboundedSender<SessionEvent>, scrollback_lines: usize) -> Self {
        Self {
            next_id: AtomicU64::new(1),
            scrollback_lines: scrollback_lines.max(1),
            events,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Open a PTY, spawn `program` with `args`, and start reader / waiter threads.
    pub fn open(
        &self,
        host_name: impl Into<String>,
        program: &Path,
        args: &[String],
        rows: u16,
        cols: u16,
    ) -> Result<SessionId, Error> {
        let host_name = host_name.into();
        let rows = rows.max(1);
        let cols = cols.max(1);
        let argv = pty_ssh_args(args);
        let mut cmd = CommandBuilder::new(program);
        cmd.args(&argv);
        cmd.env("TERM", "xterm-256color");

        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| Error::Message(format!("open pty: {error}")))?;

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|error| Error::Message(format!("spawn session: {error}")))?;
        let killer = child.clone_killer();
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| Error::Message(format!("pty reader: {error}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| Error::Message(format!("pty writer: {error}")))?;

        let id = SessionId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            rows,
            cols,
            self.scrollback_lines,
        )));
        let pending = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let exit_code = Arc::new(Mutex::new(None));
        let events = self.events.clone();

        let reader_parser = Arc::clone(&parser);
        let reader_pending = Arc::clone(&pending);
        let reader_events = events.clone();
        let reader = std::thread::Builder::new()
            .name(format!("vcon-pty-read-{id}"))
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if let Ok(mut parser) = reader_parser.lock() {
                                parser.process(&buf[..n]);
                            }
                            if !reader_pending.swap(true, Ordering::AcqRel) {
                                let _ = reader_events.send(SessionEvent::Output(id));
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|error| Error::Message(format!("pty reader thread: {error}")))?;

        let waiter_alive = Arc::clone(&alive);
        let waiter_exit = Arc::clone(&exit_code);
        let waiter = std::thread::Builder::new()
            .name(format!("vcon-pty-wait-{id}"))
            .spawn(move || {
                let status = child.wait().ok();
                let code = status.map(|status| status.exit_code());
                if let Ok(mut slot) = waiter_exit.lock() {
                    *slot = code;
                }
                waiter_alive.store(false, Ordering::Release);
                let _ = events.send(SessionEvent::Exited { id, code });
            })
            .map_err(|error| Error::Message(format!("pty waiter thread: {error}")))?;

        let session = Session {
            host_name,
            parser,
            writer: Mutex::new(Some(writer)),
            master: Mutex::new(Some(pair.master)),
            killer: Mutex::new(Some(killer)),
            pending,
            alive,
            exit_code,
            reader: Some(reader),
            waiter: Some(waiter),
        };
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, session);
        Ok(id)
    }

    /// Write bytes to the PTY master.
    pub fn write(&self, id: SessionId, bytes: &[u8]) -> Result<(), Error> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(session) = sessions.get(&id) else {
            return Err(Error::Message(format!("unknown session {id}")));
        };
        let mut writer = session
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(writer) = writer.as_mut() else {
            return Err(Error::Message(format!("session {id} has no writer")));
        };
        writer
            .write_all(bytes)
            .and_then(|()| writer.flush())
            .map_err(|error| Error::Message(format!("pty write: {error}")))
    }

    /// Resize the PTY and the vt100 screen.
    pub fn resize(&self, id: SessionId, rows: u16, cols: u16) -> Result<(), Error> {
        let rows = rows.max(1);
        let cols = cols.max(1);
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(session) = sessions.get(&id) else {
            return Err(Error::Message(format!("unknown session {id}")));
        };
        if let Ok(mut parser) = session.parser.lock() {
            parser.screen_mut().set_size(rows, cols);
        }
        let master = session
            .master
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(master) = master.as_ref() {
            master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|error| Error::Message(format!("pty resize: {error}")))?;
        }
        Ok(())
    }

    /// Kill the child and free the session buffers.
    pub fn close(&self, id: SessionId) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(mut session) = sessions.remove(&id) else {
            return;
        };
        if let Some(mut killer) = session
            .killer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = killer.kill();
        }
        *session
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        *session
            .master
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        if let Some(handle) = session.reader.take() {
            let _ = handle.join();
        }
        if let Some(handle) = session.waiter.take() {
            let _ = handle.join();
        }
    }

    /// Close every session.
    pub fn close_all(&self) {
        let ids: Vec<_> = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .copied()
            .collect();
        for id in ids {
            self.close(id);
        }
    }

    /// Clear the coalesced-output flag so the next chunk notifies again.
    pub fn clear_pending(&self, id: SessionId) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
        {
            session.pending.store(false, Ordering::Release);
        }
    }

    /// Inventory name this session was opened for.
    #[allow(dead_code)]
    pub fn host_name(&self, id: SessionId) -> Option<String> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .map(|session| session.host_name.clone())
    }

    /// True while the child is still running.
    #[allow(dead_code)]
    pub fn is_alive(&self, id: SessionId) -> bool {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .is_some_and(|session| session.alive.load(Ordering::Acquire))
    }

    /// Exit code after the child has exited.
    #[allow(dead_code)]
    pub fn exit_code(&self, id: SessionId) -> Option<u32> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .and_then(|session| {
                *session
                    .exit_code
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            })
    }

    /// How many sessions are still held (live or ended).
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// True when no sessions remain.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Count of sessions whose child is still running.
    #[allow(dead_code)]
    pub fn live_count(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .filter(|session| session.alive.load(Ordering::Acquire))
            .count()
    }

    /// Run `f` with the session parser locked.
    pub fn with_parser<R>(&self, id: SessionId, f: impl FnOnce(&vt100::Parser) -> R) -> Option<R> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = sessions.get(&id)?;
        let parser = session.parser.lock().ok()?;
        Some(f(&parser))
    }

    /// Run `f` with a mutable parser lock (scrollback navigation).
    pub fn with_parser_mut<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut vt100::Parser) -> R,
    ) -> Option<R> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = sessions.get(&id)?;
        let mut parser = session.parser.lock().ok()?;
        Some(f(&mut parser))
    }

    /// Snapshot of modes the key encoder needs.
    pub fn key_modes(&self, id: SessionId) -> crate::infra::pty::KeyModes {
        self.with_parser(id, |parser| crate::infra::pty::KeyModes {
            application_cursor: parser.screen().application_cursor(),
        })
        .unwrap_or_default()
    }

    /// Whether the remote enabled bracketed paste.
    pub fn bracketed_paste(&self, id: SessionId) -> bool {
        self.with_parser(id, |parser| parser.screen().bracketed_paste())
            .unwrap_or(false)
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        self.close_all();
    }
}

/// Insert `-t` into ConnectPlan-style args (no `ssh` binary) when missing.
pub fn pty_ssh_args(args: &[String]) -> Vec<String> {
    if args.iter().any(|arg| arg == "-t" || arg == "-tt") {
        return args.to_vec();
    }
    let mut out = Vec::with_capacity(args.len() + 1);
    out.push("-t".to_string());
    out.extend(args.iter().cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn inserts_force_tty_flag() {
        assert_eq!(
            pty_ssh_args(&[String::from("user@host")]),
            vec!["-t".to_string(), "user@host".to_string()]
        );
        assert_eq!(
            pty_ssh_args(&["-t".into(), "user@host".into()]),
            vec!["-t".to_string(), "user@host".to_string()]
        );
        assert_eq!(
            pty_ssh_args(&["-o".into(), "BatchMode=yes".into(), "h".into()]),
            vec![
                "-t".to_string(),
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                "h".to_string()
            ]
        );
    }

    #[test]
    fn local_true_exits_and_reports() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let manager = SessionManager::new(tx, 100);
        let id = manager
            .open("local", Path::new("true"), &[], 24, 80)
            .expect("spawn true");
        let mut exited = None;
        for _ in 0..50 {
            match rx.try_recv() {
                Ok(SessionEvent::Exited { id: got, code }) if got == id => {
                    exited = Some(code);
                    break;
                }
                Ok(SessionEvent::Exited { .. }) => {}
                Ok(SessionEvent::Output(_)) => {}
                Err(mpsc::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
        assert!(exited.is_some(), "expected SessionExited");
        assert!(!manager.is_alive(id));
        assert_eq!(manager.host_name(id).as_deref(), Some("local"));
        manager.close(id);
        assert!(manager.is_empty());
        let _ = PathBuf::from("true");
    }
}
