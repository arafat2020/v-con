//! Terminal lifecycle.
//!
//! [`setup`] enables raw mode, the alternate screen, and bracketed paste.
//! [`restore`] reverses that and is safe to call twice. A panic hook calls
//! restore before the default hook prints, so a panic does not leave the
//! terminal in raw mode.

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};

static RESTORED: AtomicBool = AtomicBool::new(true);
static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// The ratatui terminal backed by stdout.
pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Enter the TUI. `mouse` also enables mouse capture.
pub fn setup(mouse: bool) -> io::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, Hide)?;
    if mouse {
        execute!(stdout, EnableMouseCapture)?;
    }
    RESTORED.store(false, Ordering::SeqCst);
    install_panic_hook();
    Terminal::new(CrosstermBackend::new(stdout))
}

/// Leave the TUI. A second call does nothing.
pub fn restore() -> io::Result<()> {
    if RESTORED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let _ = disable_raw_mode();
    execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        Show
    )
}

fn install_panic_hook() {
    if HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        previous(info);
    }));
}
