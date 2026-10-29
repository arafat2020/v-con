//! Terminal lifecycle.
//!
//! [`setup`] enables raw mode, the alternate screen, and bracketed paste.
//! [`restore`] reverses that and is safe to call twice. [`suspend`] and
//! [`resume`] wrap the same teardown around an ssh session or `$EDITOR`.
//! A panic hook calls restore before the default hook prints, so a panic does
//! not leave the terminal in raw mode.

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static RESTORED: AtomicBool = AtomicBool::new(true);
static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// The ratatui terminal backed by stdout.
pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Enter the TUI. `mouse` also enables mouse capture.
pub fn setup(mouse: bool) -> io::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Clear after entering the alternate screen so cargo/build scrollback does
    // not leave the viewport scrolled away from the first frame.
    execute!(
        stdout,
        EnterAlternateScreen,
        Clear(ClearType::All),
        MoveTo(0, 0),
        EnableBracketedPaste,
        Hide
    )?;
    if mouse {
        execute!(stdout, EnableMouseCapture)?;
    }
    let _ = stdout.flush();
    RESTORED.store(false, Ordering::SeqCst);
    install_panic_hook();
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    terminal.clear()?;
    Ok(terminal)
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

/// Leave raw mode and the alternate screen so a child can use the terminal.
///
/// The panic hook treats the terminal as already restored. [`resume`] puts the
/// TUI back afterwards.
pub fn suspend(terminal: &mut Tui) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        Show
    )?;
    let _ = io::stdout().flush();
    RESTORED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Return to the TUI after a child process exits and force a full redraw.
pub fn resume(terminal: &mut Tui, mouse: bool) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        Hide
    )?;
    if mouse {
        execute!(terminal.backend_mut(), EnableMouseCapture)?;
    }
    RESTORED.store(false, Ordering::SeqCst);
    terminal.clear()
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
