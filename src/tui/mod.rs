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
use crate::services::inventory::InventoryBundle;
use crate::tui::action::{Action, Handoff, UiCommand};
use crate::tui::app::App;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::time::MissedTickBehavior;

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
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    render.set_missed_tick_behavior(MissedTickBehavior::Skip);
    if let Ok(size) = terminal.size() {
        app.note_size(size.width, size.height);
    }

    loop {
        if app.quit() {
            break;
        }
        let mut handoff = None;
        tokio::select! {
            biased;
            event = events.next() => {
                match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat => {
                        let commands = app.on_key(key.code, key.modifiers);
                        handoff = apply_commands(app, commands);
                    }
                    Some(Ok(Event::Resize(width, height))) => app.note_size(width, height),
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(Error::Message(error.to_string())),
                    None => break,
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
        if let Some(handoff) = handoff {
            drop(events);
            run_handoff(terminal, app, handoff).await?;
            events = EventStream::new();
            app.dirty = true;
        }
    }
    Ok(())
}

fn apply_commands(app: &mut App, commands: Vec<UiCommand>) -> Option<Handoff> {
    let mut handoff = None;
    for command in commands {
        match command {
            UiCommand::Copy(text) => match copy_to_clipboard(&text) {
                Ok(()) => app.toast("Copied ssh command"),
                Err(error) => app.toast(error),
            },
            UiCommand::Persist => {
                if let Err(error) = state_store::save(app.state_path(), app.state()) {
                    app.toast(format!("could not save state: {error}"));
                }
            }
            UiCommand::Suspend(next) => handoff = Some(next),
        }
    }
    handoff
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
