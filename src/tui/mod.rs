//! The terminal UI.
//!
//! [`run`] owns the terminal: raw mode, the alternate screen, and a panic hook
//! that restores both. The loop selects on keyboard input, a 250 ms tick, and
//! a frame timer. It draws only while [`App::dirty`] is set. Clipboard copies
//! happen here, outside `App::update`, so the state change stays free of I/O.

mod action;
mod app;
mod event;
mod keymap;
mod screens;
mod terminal;
mod theme;
mod widgets;

use crate::config::Config;
use crate::error::Error;
use crate::infra::remote_fs::{LocalFs, RemoteFs, SftpFs, normalize, parent_of, sftp_arguments};
use crate::infra::ssh::backend::SshBackend;
use crate::infra::ssh::system::{self, SystemSsh};
use crate::infra::state_store;
use crate::paths::Paths;
use crate::services::connect::{self};
use crate::services::exec::{self as job, ExecLaunch};
use crate::services::files::{
    TransferDirection, TransferEvent, TransferJob, TransferState, collect_tree, map_destinations,
    run_queue,
};
use crate::services::health::{self as health_svc, HealthControl, HealthEvent};
use crate::services::inventory::InventoryBundle;
use crate::services::sessions::{SessionEvent, SessionManager};
use crate::services::tunnels::{TunnelEvent, TunnelManager};
use crate::tui::action::{
    Action, FilesCommand, Handoff, SessionsCommand, TunnelsCommand, UiCommand,
};
use crate::tui::app::App;
use crate::tui::screens::files::PaneFocus;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

/// Open the Hosts screen and run until the user quits.
///
/// `no_color` forces the bold/reverse styles used when colours are disabled.
pub async fn run(
    bundle: InventoryBundle,
    config: Config,
    paths: Paths,
    no_color: bool,
) -> Result<(), Error> {
    let mut app = App::new(bundle, config, paths, no_color)?;
    let mouse = app.mouse();
    let frame = app.frame_time();
    let mut terminal = terminal::setup(mouse).map_err(|error| Error::Message(error.to_string()))?;
    let result = event_loop(&mut terminal, &mut app, frame).await;
    let _ = terminal::restore();
    result
}

async fn event_loop(
    terminal: &mut terminal::Tui,
    app: &mut App,
    frame: Duration,
) -> Result<(), Error> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut render = tokio::time::interval(frame);
    let mut job: Option<LiveExec> = None;
    let mut health = spawn_health(app);
    let mut live_files = LiveFiles::new();
    let mut live_sessions = LiveSessions::new(app.session_scrollback);
    let mut live_tunnels = LiveTunnels::new(app.ssh_program().cloned());
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    render.set_missed_tick_behavior(MissedTickBehavior::Skip);
    if let Ok(size) = terminal.size() {
        app.note_size(size.width, size.height);
    }

    loop {
        if app.quit() {
            if let Some(live) = job.take() {
                live.cancel.cancel();
                let _ = tokio::time::timeout(Duration::from_secs(2), live.task).await;
            }
            if let Some(live) = health.take() {
                live.cancel.cancel();
                let _ = tokio::time::timeout(Duration::from_secs(2), live.task).await;
            }
            live_files.shutdown().await;
            live_sessions.manager.close_all();
            live_tunnels.manager.stop_all();
            break;
        }
        let mut effects = Effects::default();
        tokio::select! {
            biased;
            event = events.next() => {
                match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat => {
                        let commands = app.on_key(key.code, key.modifiers);
                        effects = apply_commands(app, commands, health.as_ref(), &mut live_files);
                    }
                    Some(Ok(Event::Paste(text))) => {
                        let commands = app.on_paste(&text);
                        effects = apply_commands(app, commands, health.as_ref(), &mut live_files);
                    }
                    Some(Ok(Event::Resize(width, height))) => {
                        app.note_size(width, height);
                        for command in app.session_resize_commands() {
                            if let UiCommand::Sessions(sessions) = command {
                                effects.sessions.push(sessions);
                            }
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(Error::Message(error.to_string())),
                    None => break,
                }
            }
            session_event = live_sessions.events.recv() => {
                if let Some(event) = session_event {
                    apply_session_side_effects(app, &live_sessions, &event);
                    app.apply_session_event(event);
                    while let Ok(event) = live_sessions.events.try_recv() {
                        apply_session_side_effects(app, &live_sessions, &event);
                        app.apply_session_event(event);
                    }
                }
            }
            tunnel_event = live_tunnels.events.recv() => {
                if let Some(event) = tunnel_event {
                    live_tunnels.manager.forget(match &event {
                        TunnelEvent::Exited { id, .. } => *id,
                    });
                    app.apply_tunnel_event(event);
                    app.set_running_tunnels(live_tunnels.manager.list());
                    while let Ok(event) = live_tunnels.events.try_recv() {
                        live_tunnels.manager.forget(match &event {
                            TunnelEvent::Exited { id, .. } => *id,
                        });
                        app.apply_tunnel_event(event);
                        app.set_running_tunnels(live_tunnels.manager.list());
                    }
                }
            }
            exec_event = recv_exec(&mut job) => {
                match exec_event {
                    Some(event) => {
                        app.apply_exec_event(event);
                        if let Some(live) = job.as_mut() {
                            for _ in 0..200 {
                                match live.events.try_recv() {
                                    Ok(event) => app.apply_exec_event(event),
                                    Err(_) => break,
                                }
                            }
                        }
                    }
                    None => {
                        if let Some(live) = job.take() {
                            let _ = live.task.await;
                        }
                    }
                }
            }
            transfer_event = recv_transfer(&mut live_files) => {
                if let Some(event) = transfer_event {
                    app.apply_transfer_event(event);
                    while let Ok(event) = live_files.events.try_recv() {
                        app.apply_transfer_event(event);
                    }
                }
            }
            health_event = recv_health(&mut health) => {
                match health_event {
                    Some(event) => {
                        app.apply_health_event(event);
                        if let Some(live) = health.as_mut() {
                            for _ in 0..200 {
                                match live.events.try_recv() {
                                    Ok(event) => app.apply_health_event(event),
                                    Err(_) => break,
                                }
                            }
                        }
                        if let Err(error) = state_store::save(app.state_path(), app.state()) {
                            app.toast(format!("could not save state: {error}"));
                        }
                    }
                    None => {
                        if let Some(live) = health.take() {
                            let _ = live.task.await;
                        }
                    }
                }
            }
            _ = tick.tick() => {
                let _ = app.update(Action::Tick);
            }
            _ = render.tick(), if app.dirty => {
                terminal
                    .draw(|frame| app.draw(frame, Some(&live_sessions.manager)))
                    .map_err(|error| Error::Message(error.to_string()))?;
                app.dirty = false;
                for command in app.session_resize_commands() {
                    if let UiCommand::Sessions(sessions) = command
                        && let Err(error) =
                            handle_sessions_command(app, &mut live_sessions, sessions)
                    {
                        app.show_error(error.to_string());
                    }
                }
            }
        }
        if effects.cancel
            && let Some(live) = &job
        {
            live.cancel.cancel();
        }
        if let Some(launch) = effects.start {
            if let Some(live) = job.take() {
                live.cancel.cancel();
                let _ = tokio::time::timeout(Duration::from_secs(2), live.task).await;
            }
            job = Some(spawn_exec(launch));
        }
        if let Some(handoff) = effects.handoff {
            drop(events);
            run_handoff(terminal, app, handoff).await?;
            events = EventStream::new();
            app.dirty = true;
        }
        for command in effects.files {
            if let Err(error) = handle_files_command(app, &mut live_files, command).await {
                app.show_error(error.to_string());
            }
        }
        for command in effects.sessions {
            if let Err(error) = handle_sessions_command(app, &mut live_sessions, command) {
                app.show_error(error.to_string());
            }
        }
        for command in effects.tunnels {
            if let Err(error) = handle_tunnels_command(app, &mut live_tunnels, command) {
                app.show_error(error.to_string());
            }
        }
    }
    Ok(())
}

struct LiveTunnels {
    manager: TunnelManager,
    events: mpsc::UnboundedReceiver<TunnelEvent>,
}

impl LiveTunnels {
    fn new(program: Option<std::path::PathBuf>) -> Self {
        let (tx, events) = mpsc::unbounded_channel();
        let program = program.unwrap_or_else(|| std::path::PathBuf::from("ssh"));
        Self {
            manager: TunnelManager::new(program, tx),
            events,
        }
    }
}

const VIEW_CAP: usize = 1024 * 1024;
const SFTP_CONNECT_TIMEOUT: u64 = 30;

struct LiveFiles {
    remote: Option<Arc<SftpFs>>,
    cancel: CancellationToken,
    events: mpsc::Receiver<TransferEvent>,
    events_tx: mpsc::Sender<TransferEvent>,
}

impl LiveFiles {
    fn new() -> Self {
        let (events_tx, events) = mpsc::channel(256);
        Self {
            remote: None,
            cancel: CancellationToken::new(),
            events,
            events_tx,
        }
    }

    async fn shutdown(&mut self) {
        self.cancel.cancel();
        if let Some(remote) = self.remote.take()
            && let Ok(sftp) = Arc::try_unwrap(remote)
        {
            let _ = sftp.close().await;
        }
    }
}

async fn recv_transfer(live: &mut LiveFiles) -> Option<TransferEvent> {
    live.events.recv().await
}

struct LiveExec {
    cancel: CancellationToken,
    events: mpsc::Receiver<job::JobEvent>,
    task: tokio::task::JoinHandle<()>,
}

struct LiveHealth {
    cancel: CancellationToken,
    events: mpsc::Receiver<HealthEvent>,
    control: mpsc::Sender<HealthControl>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct Effects {
    handoff: Option<Handoff>,
    start: Option<ExecLaunch>,
    cancel: bool,
    files: Vec<FilesCommand>,
    sessions: Vec<SessionsCommand>,
    tunnels: Vec<TunnelsCommand>,
}

struct LiveSessions {
    manager: SessionManager,
    events: mpsc::UnboundedReceiver<SessionEvent>,
}

impl LiveSessions {
    fn new(scrollback_lines: usize) -> Self {
        let (tx, events) = mpsc::unbounded_channel();
        Self {
            manager: SessionManager::new(tx, scrollback_lines),
            events,
        }
    }
}

fn spawn_exec(launch: ExecLaunch) -> LiveExec {
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let (tx, events) = mpsc::channel(128);
    let task = tokio::spawn(async move {
        let runner = Arc::new(SystemSsh::new(launch.program));
        job::run_job(
            runner,
            launch.targets,
            launch.timeout,
            launch.parallel,
            token,
            tx,
        )
        .await;
    });
    LiveExec {
        cancel,
        events,
        task,
    }
}

async fn recv_exec(job: &mut Option<LiveExec>) -> Option<job::JobEvent> {
    match job {
        Some(job) => job.events.recv().await,
        None => std::future::pending().await,
    }
}

async fn recv_health(job: &mut Option<LiveHealth>) -> Option<HealthEvent> {
    match job {
        Some(job) => job.events.recv().await,
        None => std::future::pending().await,
    }
}

fn spawn_health(app: &App) -> Option<LiveHealth> {
    let (targets, settings, cache) = app.health_bootstrap()?;
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let (events_tx, events) = mpsc::channel(128);
    let (control, control_rx) = mpsc::channel(32);
    let program = settings.program.clone();
    let task = tokio::spawn(async move {
        let runner = Arc::new(SystemSsh::new(program));
        health_svc::run_scheduler(
            runner, targets, settings, token, control_rx, events_tx, cache,
        )
        .await;
    });
    Some(LiveHealth {
        cancel,
        events,
        control,
        task,
    })
}

fn apply_commands(
    app: &mut App,
    commands: Vec<UiCommand>,
    health: Option<&LiveHealth>,
    _live_files: &mut LiveFiles,
) -> Effects {
    let mut effects = Effects::default();
    for command in commands {
        match command {
            UiCommand::Copy(text) => {
                let label = if app.exec_visible() {
                    "Copied output"
                } else {
                    "Copied ssh command"
                };
                match copy_to_clipboard(&text) {
                    Ok(()) => app.toast(label),
                    Err(error) => app.toast(error),
                }
            }
            UiCommand::Persist => {
                if let Err(error) = state_store::save(app.state_path(), app.state()) {
                    app.toast(format!("could not save state: {error}"));
                }
            }
            UiCommand::Suspend(next) => effects.handoff = Some(next),
            UiCommand::StartExec(launch) => effects.start = Some(launch),
            UiCommand::CancelExec => effects.cancel = true,
            UiCommand::Health(control) => {
                if let Some(live) = health {
                    let _ = live.control.try_send(control);
                }
            }
            UiCommand::Files(files) => effects.files.push(files),
            UiCommand::Sessions(sessions) => effects.sessions.push(sessions),
            UiCommand::Tunnels(tunnels) => effects.tunnels.push(tunnels),
        }
    }
    effects
}

fn apply_session_side_effects(app: &mut App, live: &LiveSessions, event: &SessionEvent) {
    if let SessionEvent::Output(id) = event {
        live.manager.clear_pending(*id);
        let modes = live.manager.key_modes(*id);
        let bracketed = live.manager.bracketed_paste(*id);
        app.refresh_session_modes(*id, modes, bracketed);
    }
}

fn handle_tunnels_command(
    app: &mut App,
    live: &mut LiveTunnels,
    command: TunnelsCommand,
) -> Result<(), Error> {
    match command {
        TunnelsCommand::Start { host, label, spec } => {
            let host_ref = app
                .effective_hosts()
                .iter()
                .find(|candidate| candidate.name == host)
                .ok_or_else(|| Error::Message(format!("no host `{host}`")))?;
            let host_owned = host_ref.clone();
            let hosts = app.effective_hosts().to_vec();
            live.manager.start(
                &host_owned,
                &hosts,
                label.clone(),
                spec.clone(),
                app.multiplexing_enabled(),
                app.control_dir_path(),
            )?;
            app.audit_tunnel_start(&host, &label, &spec);
            app.set_running_tunnels(live.manager.list());
            app.toast(format!("tunnel {label} started"));
        }
        TunnelsCommand::Stop(id) => {
            live.manager.stop(id)?;
            app.set_running_tunnels(live.manager.list());
            app.toast("tunnel stopped");
        }
        TunnelsCommand::StopAll => {
            live.manager.stop_all();
            app.set_running_tunnels(Vec::new());
        }
    }
    Ok(())
}

fn handle_sessions_command(
    app: &mut App,
    live: &mut LiveSessions,
    command: SessionsCommand,
) -> Result<(), Error> {
    match command {
        SessionsCommand::Open {
            host,
            rows,
            cols,
            place,
        } => {
            let id = open_session(app, live, &host, rows, cols)?;
            app.session_opened(id, host, place);
            for command in app.session_resize_commands() {
                if let UiCommand::Sessions(sessions) = command {
                    handle_sessions_command(app, live, sessions)?;
                }
            }
        }
        SessionsCommand::Write { id, bytes } => {
            live.manager.write(id, &bytes)?;
        }
        SessionsCommand::Resize { id, rows, cols } => {
            live.manager.resize(id, rows, cols)?;
        }
        SessionsCommand::Close(id) => {
            live.manager.close(id);
            app.session_closed(id);
        }
        SessionsCommand::CloseAll => {
            let ids: Vec<_> = app.sessions.hosts.keys().copied().collect();
            live.manager.close_all();
            for id in ids {
                app.session_closed(id);
            }
        }
        SessionsCommand::Scroll { id, delta } => {
            live.manager.with_parser_mut(id, |parser| {
                let current = parser.screen().scrollback() as isize;
                let next = (current + delta).max(0) as usize;
                parser.screen_mut().set_scrollback(next);
            });
            app.dirty = true;
        }
        SessionsCommand::Reconnect {
            id,
            host,
            rows,
            cols,
        } => {
            live.manager.close(id);
            let new_id = open_session(app, live, &host, rows, cols)?;
            app.session_reconnected(id, new_id, host);
            live.manager.resize(new_id, rows, cols)?;
        }
    }
    Ok(())
}

fn open_session(
    app: &mut App,
    live: &mut LiveSessions,
    host: &str,
    rows: u16,
    cols: u16,
) -> Result<crate::services::sessions::SessionId, Error> {
    let program = app
        .ssh_program()
        .cloned()
        .or_else(|| system::find_program("ssh", std::env::var_os("PATH").as_deref()))
        .ok_or_else(|| Error::Message(system::missing_ssh_message().to_string()))?;
    let options = connect::session_options(app.multiplexing_enabled(), app.control_dir_path());
    let plan = connect::prepare(app.effective_hosts(), app.state_path(), &options, host)?;
    let _ = state_store::save(app.state_path(), app.state());
    live.manager.open(host, &program, &plan.args, rows, cols)
}

async fn handle_files_command(
    app: &mut App,
    live: &mut LiveFiles,
    command: FilesCommand,
) -> Result<(), crate::error::Error> {
    match command {
        FilesCommand::Refresh { local, remote } => {
            refresh_files_listings(app, live, local, remote).await?;
        }
        FilesCommand::Connect(host) => {
            connect_files_host(app, live, &host).await?;
        }
        FilesCommand::Disconnect => {
            disconnect_files(live).await;
            app.files.host = None;
            app.dirty = true;
        }
        FilesCommand::Copy {
            direction,
            sources,
            dest_dir,
            host,
        } => {
            start_files_copy(app, live, direction, sources, dest_dir, host).await?;
        }
        FilesCommand::CancelTransfers => {
            live.cancel.cancel();
            live.cancel = CancellationToken::new();
            app.toast("cancelled transfers");
        }
        FilesCommand::Delete { paths, remote } => {
            delete_files_paths(live, &paths, remote).await?;
            refresh_files_listings(app, live, !remote, remote).await?;
        }
        FilesCommand::Mkdir { path, remote } => {
            files_fs(live, remote).await?.mkdir(&path).await?;
            app.toast(format!("created {path}"));
        }
        FilesCommand::Rename { from, to, remote } => {
            files_fs(live, remote).await?.rename(&from, &to).await?;
            app.toast(format!("renamed to {to}"));
        }
        FilesCommand::View { path, remote } => {
            view_files_path(app, live, &path, remote).await?;
        }
    }
    Ok(())
}

async fn refresh_files_listings(
    app: &mut App,
    live: &LiveFiles,
    local: bool,
    remote: bool,
) -> Result<(), crate::error::Error> {
    if local {
        let path = app.files.local.path.clone();
        let local_fs = LocalFs;
        match local_fs.list(&path).await {
            Ok(entries) => {
                app.files.status = None;
                app.apply_files_listing(PaneFocus::Local, path, entries);
            }
            Err(error) => {
                app.files.status = Some(error.to_string());
                app.toast(error.to_string());
            }
        }
    }
    if remote {
        let Some(sftp) = live.remote.as_ref() else {
            return Ok(());
        };
        let path = app.files.remote.path.clone();
        match sftp.list(&path).await {
            Ok(entries) => {
                app.files.status = None;
                app.apply_files_listing(PaneFocus::Remote, path, entries);
            }
            Err(error) => {
                app.files.status = Some(error.to_string());
                app.toast(error.to_string());
            }
        }
    }
    Ok(())
}

async fn connect_files_host(
    app: &mut App,
    live: &mut LiveFiles,
    host_name: &str,
) -> Result<(), crate::error::Error> {
    let Some(program) = app.ssh_program().cloned() else {
        return Err(crate::error::Error::Message(
            crate::infra::ssh::system::missing_ssh_message().to_string(),
        ));
    };
    let Some(host) = app
        .effective_hosts()
        .iter()
        .find(|host| host.name == host_name)
        .cloned()
    else {
        return Err(crate::error::Error::Message(format!(
            "unknown host `{host_name}`"
        )));
    };
    disconnect_files(live).await;
    let control = app
        .multiplexing_enabled()
        .then(|| app.control_dir_path().display().to_string());
    let args = sftp_arguments(
        &host,
        app.effective_hosts(),
        Some(SFTP_CONNECT_TIMEOUT),
        app.multiplexing_enabled(),
        control.as_deref(),
    );
    match SftpFs::connect(program, args).await {
        Ok(session) => {
            live.remote = Some(Arc::new(session));
            if let Some(sftp) = live.remote.as_ref()
                && let Ok(home) = sftp.realpath("~").await
            {
                app.files.remote.path = home;
            }
            app.toast(format!("connected to {host_name}"));
        }
        Err(error) => {
            app.toast(error.to_string());
            return Err(error);
        }
    }
    Ok(())
}

async fn disconnect_files(live: &mut LiveFiles) {
    if let Some(remote) = live.remote.take()
        && let Ok(sftp) = Arc::try_unwrap(remote)
    {
        let _ = sftp.close().await;
    }
}

async fn files_fs(live: &LiveFiles, remote: bool) -> Result<FilesFs<'_>, crate::error::Error> {
    if remote {
        let Some(sftp) = live.remote.as_ref() else {
            return Err(crate::error::Error::Message(
                "not connected to a remote host".to_string(),
            ));
        };
        Ok(FilesFs::Remote(sftp))
    } else {
        Ok(FilesFs::Local)
    }
}

enum FilesFs<'a> {
    Local,
    Remote(&'a SftpFs),
}

impl RemoteFs for FilesFs<'_> {
    async fn list(
        &self,
        dir: &str,
    ) -> Result<Vec<crate::infra::remote_fs::DirEntry>, crate::error::Error> {
        match self {
            Self::Local => LocalFs.list(dir).await,
            Self::Remote(fs) => fs.list(dir).await,
        }
    }

    async fn stat(
        &self,
        path: &str,
    ) -> Result<crate::infra::remote_fs::DirEntry, crate::error::Error> {
        match self {
            Self::Local => LocalFs.stat(path).await,
            Self::Remote(fs) => fs.stat(path).await,
        }
    }

    async fn mkdir(&self, path: &str) -> Result<(), crate::error::Error> {
        match self {
            Self::Local => LocalFs.mkdir(path).await,
            Self::Remote(fs) => fs.mkdir(path).await,
        }
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), crate::error::Error> {
        match self {
            Self::Local => LocalFs.rename(from, to).await,
            Self::Remote(fs) => fs.rename(from, to).await,
        }
    }

    async fn remove_file(&self, path: &str) -> Result<(), crate::error::Error> {
        match self {
            Self::Local => LocalFs.remove_file(path).await,
            Self::Remote(fs) => fs.remove_file(path).await,
        }
    }

    async fn remove_dir(&self, path: &str) -> Result<(), crate::error::Error> {
        match self {
            Self::Local => LocalFs.remove_dir(path).await,
            Self::Remote(fs) => fs.remove_dir(path).await,
        }
    }

    async fn realpath(&self, path: &str) -> Result<String, crate::error::Error> {
        match self {
            Self::Local => LocalFs.realpath(path).await,
            Self::Remote(fs) => fs.realpath(path).await,
        }
    }

    async fn read_at(
        &self,
        path: &str,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, crate::error::Error> {
        match self {
            Self::Local => LocalFs.read_at(path, offset, len).await,
            Self::Remote(fs) => fs.read_at(path, offset, len).await,
        }
    }

    async fn create(&self, path: &str) -> Result<(), crate::error::Error> {
        match self {
            Self::Local => LocalFs.create(path).await,
            Self::Remote(fs) => fs.create(path).await,
        }
    }

    async fn write_at(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<(), crate::error::Error> {
        match self {
            Self::Local => LocalFs.write_at(path, offset, data).await,
            Self::Remote(fs) => fs.write_at(path, offset, data).await,
        }
    }

    async fn set_meta(
        &self,
        path: &str,
        mode: Option<u32>,
        mtime: Option<std::time::SystemTime>,
    ) -> Result<(), crate::error::Error> {
        match self {
            Self::Local => LocalFs.set_meta(path, mode, mtime).await,
            Self::Remote(fs) => fs.set_meta(path, mode, mtime).await,
        }
    }
}

async fn delete_files_paths(
    live: &LiveFiles,
    paths: &[String],
    remote: bool,
) -> Result<(), crate::error::Error> {
    let fs = files_fs(live, remote).await?;
    for path in paths {
        let meta = fs.stat(path).await?;
        if meta.is_dir() {
            fs.remove_dir(path).await?;
        } else {
            fs.remove_file(path).await?;
        }
    }
    Ok(())
}

async fn view_files_path(
    app: &mut App,
    live: &LiveFiles,
    path: &str,
    remote: bool,
) -> Result<(), crate::error::Error> {
    let fs = files_fs(live, remote).await?;
    let meta = fs.stat(path).await?;
    if meta.is_dir() {
        return Err(crate::error::Error::Message(
            "cannot view a directory".to_string(),
        ));
    }
    if meta.size > VIEW_CAP as u64 {
        return Err(crate::error::Error::Message(format!(
            "file is larger than {} bytes",
            VIEW_CAP
        )));
    }
    let bytes = fs.read_at(path, 0, VIEW_CAP).await?;
    let text = String::from_utf8(bytes).map_err(|_| {
        crate::error::Error::Message("refusing to view non-text (non-UTF-8) file".to_string())
    })?;
    app.apply_view_text(path.to_string(), text);
    Ok(())
}

async fn start_files_copy(
    app: &mut App,
    live: &mut LiveFiles,
    direction: TransferDirection,
    sources: Vec<String>,
    dest_dir: String,
    host: String,
) -> Result<(), crate::error::Error> {
    let remote = live.remote.clone().ok_or_else(|| {
        crate::error::Error::Message("not connected to a remote host".to_string())
    })?;
    let local = Arc::new(LocalFs);
    let dest_dir = normalize(&dest_dir);
    let mut jobs = Vec::new();
    for source in sources {
        let source = normalize(&source);
        let (entries, root) = match direction {
            TransferDirection::Upload => {
                let meta = LocalFs.stat(&source).await?;
                let root = if meta.is_dir() {
                    source.clone()
                } else {
                    parent_of(&source).unwrap_or_else(|| "/".to_string())
                };
                let entries = collect_tree(&LocalFs, &source).await?;
                (entries, root)
            }
            TransferDirection::Download => {
                let meta = remote.stat(&source).await?;
                let root = if meta.is_dir() {
                    source.clone()
                } else {
                    parent_of(&source).unwrap_or_else(|| "/".to_string())
                };
                let entries = collect_tree(remote.as_ref(), &source).await?;
                (entries, root)
            }
        };
        let mapped = map_destinations(&entries, &root, &dest_dir);
        for (src_path, destination, size) in mapped {
            let id = app.files.queue.ids.next();
            let job = TransferJob {
                id,
                direction,
                source: src_path,
                destination,
                host: host.clone(),
                total: size,
                done: 0,
                state: TransferState::Queued,
                error: None,
                started: None,
            };
            let _ = live
                .events_tx
                .send(TransferEvent::Queued(job.clone()))
                .await;
            app.apply_transfer_event(TransferEvent::Queued(job.clone()));
            jobs.push(job);
        }
    }
    if jobs.is_empty() {
        app.toast("nothing to copy");
        return Ok(());
    }
    let cancel = live.cancel.clone();
    let events_tx = live.events_tx.clone();
    match direction {
        TransferDirection::Upload => {
            let src = Arc::clone(&local);
            let dst = Arc::clone(&remote);
            tokio::spawn(async move {
                run_queue(src, dst, jobs, cancel, events_tx).await;
            });
        }
        TransferDirection::Download => {
            let src = Arc::clone(&remote);
            let dst = Arc::clone(&local);
            tokio::spawn(async move {
                run_queue(src, dst, jobs, cancel, events_tx).await;
            });
        }
    }
    Ok(())
}

async fn run_handoff(
    terminal: &mut terminal::Tui,
    app: &mut App,
    handoff: Handoff,
) -> Result<(), Error> {
    terminal::suspend(terminal).map_err(|error| Error::Message(error.to_string()))?;
    let mouse = app.mouse();
    let joined = tokio::task::spawn_blocking(move || execute_handoff(handoff)).await;
    terminal::resume(terminal, mouse).map_err(|error| Error::Message(error.to_string()))?;
    drain_keys();
    match joined {
        Ok(SessionEnd::Connect(Ok(status))) => app.note_session_exit(&status),
        Ok(SessionEnd::Connect(Err(error))) => app.note_session_error(&error),
        Ok(SessionEnd::Editor(Ok(_))) => app.reload_inventory(),
        Ok(SessionEnd::Editor(Err(error))) => {
            app.show_error(format!("could not start editor: {error}"))
        }
        Err(error) => app.show_error(format!("session task failed: {error}")),
    }
    Ok(())
}

enum SessionEnd {
    Connect(io::Result<std::process::ExitStatus>),
    Editor(io::Result<std::process::ExitStatus>),
}

fn execute_handoff(handoff: Handoff) -> SessionEnd {
    match handoff {
        Handoff::Connect(spec) => {
            println!("{}", spec.banner);
            let _ = io::stdout().flush();
            let ssh = SystemSsh::new(spec.program);
            SessionEnd::Connect(ssh.interactive(&spec.args))
        }
        Handoff::Editor(spec) => {
            let status = Command::new(spec.program)
                .args(spec.args)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
            SessionEnd::Editor(status)
        }
    }
}

fn drain_keys() {
    let timeout = Duration::from_millis(0);
    while crossterm::event::poll(timeout).unwrap_or(false) {
        if crossterm::event::read().is_err() {
            break;
        }
    }
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_text(text))
        .map_err(|error| error.to_string())
}
