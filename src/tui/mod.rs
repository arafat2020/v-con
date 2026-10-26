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
use crate::infra::ssh::backend::SshBackend;
use crate::infra::ssh::system::SystemSsh;
use crate::infra::state_store;
use crate::paths::Paths;
use crate::services::exec::{self as job, ExecLaunch};
use crate::services::inventory::InventoryBundle;
use crate::tui::action::{Action, Handoff, UiCommand};
use crate::tui::app::App;
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
            break;
        }
        let mut effects = Effects::default();
        tokio::select! {
            biased;
            event = events.next() => {
                match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat => {
                        let commands = app.on_key(key.code, key.modifiers);
                        effects = apply_commands(app, commands);
                    }
                    Some(Ok(Event::Resize(width, height))) => app.note_size(width, height),
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(Error::Message(error.to_string())),
                    None => break,
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
            _ = tick.tick() => {
                let _ = app.update(Action::Tick);
            }
            _ = render.tick(), if app.dirty => {
                terminal
                    .draw(|frame| app.draw(frame))
                    .map_err(|error| Error::Message(error.to_string()))?;
                app.dirty = false;
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
    }
    Ok(())
}

struct LiveExec {
    cancel: CancellationToken,
    events: mpsc::Receiver<job::JobEvent>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct Effects {
    handoff: Option<Handoff>,
    start: Option<ExecLaunch>,
    cancel: bool,
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

fn apply_commands(app: &mut App, commands: Vec<UiCommand>) -> Effects {
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
        }
    }
    effects
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
