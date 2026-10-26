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
use crate::paths::Paths;
use crate::services::inventory::InventoryBundle;
use crate::tui::action::{Action, UiCommand};
use crate::tui::app::App;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
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
        tokio::select! {
            biased;
            event = events.next() => {
                match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat => {
                        let commands = app.on_key(key.code, key.modifiers);
                        apply_commands(app, commands);
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
    }
    Ok(())
}

fn apply_commands(app: &mut App, commands: Vec<UiCommand>) {
    for command in commands {
        match command {
            UiCommand::Copy(text) => match copy_to_clipboard(&text) {
                Ok(()) => app.toast("Copied ssh command"),
                Err(error) => app.toast(error),
            },
        }
    }
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_text(text))
        .map_err(|error| error.to_string())
}
