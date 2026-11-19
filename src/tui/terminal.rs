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
use ratatui::backend::{Backend, CrosstermBackend};
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
    // Same clear as resume: do not query the cursor. A fullscreen alternate
    // screen has nothing to restore, and ESC[6n fails when its reply is late.
    clear_screen(&mut terminal)?;
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
///
/// The screen is cleared without asking where the cursor is. Ratatui's
/// [`Terminal::clear`] writes `ESC[6n` and waits on stdin for the report.
/// After an ssh handoff that report is not read in time, resume fails with
/// "The cursor position could not be read within a normal duration", and the
/// unread `ESC[row;colR` is left on the tty.
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
    clear_screen(terminal)
}

/// Clear the visible screen and both render buffers.
///
/// Unlike [`Terminal::clear`], this does not query the cursor. Both buffers
/// are reset so the next draw repaints every cell onto the blank screen
/// instead of diffing against the frame from before the handoff.
fn clear_screen<B: Backend>(terminal: &mut Terminal<B>) -> Result<(), B::Error> {
    terminal
        .backend_mut()
        .clear_region(ratatui::backend::ClearType::All)?;
    // `swap_buffers` blanks the inactive buffer and makes it current.
    // A second swap blanks the other one too.
    terminal.swap_buffers();
    terminal.swap_buffers();
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::{TestBackend, WindowSize};
    use ratatui::buffer::Cell;
    use ratatui::layout::{Position, Size};

    /// Backend that fails the cursor-position query `Terminal::clear` sends.
    struct CursorProbe {
        inner: TestBackend,
        queries: u32,
    }

    impl Backend for CursorProbe {
        type Error = io::Error;

        fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            self.inner.draw(content).map_err(|err| match err {})
        }

        fn hide_cursor(&mut self) -> io::Result<()> {
            self.inner.hide_cursor().map_err(|err| match err {})
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            self.inner.show_cursor().map_err(|err| match err {})
        }

        fn get_cursor_position(&mut self) -> io::Result<Position> {
            self.queries += 1;
            Err(io::Error::other(
                "The cursor position could not be read within a normal duration",
            ))
        }

        fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
            self.inner
                .set_cursor_position(position)
                .map_err(|err| match err {})
        }

        fn clear(&mut self) -> io::Result<()> {
            self.inner.clear().map_err(|err| match err {})
        }

        fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> io::Result<()> {
            self.inner
                .clear_region(clear_type)
                .map_err(|err| match err {})
        }

        fn size(&self) -> io::Result<Size> {
            self.inner.size().map_err(|err| match err {})
        }

        fn window_size(&mut self) -> io::Result<WindowSize> {
            self.inner.window_size().map_err(|err| match err {})
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush().map_err(|err| match err {})
        }
    }

    #[test]
    fn resume_clear_repaints_without_querying_the_cursor() {
        let backend = CursorProbe {
            inner: TestBackend::new(4, 1),
            queries: 0,
        };
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget("AAAA", frame.area());
            })
            .unwrap();
        clear_screen(&mut terminal).unwrap();
        assert_eq!(terminal.backend().queries, 0);
        terminal.backend().inner.assert_buffer_lines(["    "]);
        // The same frame must be painted again. If the diff baseline still
        // held "AAAA", this draw would skip every cell and leave the wipe.
        terminal
            .draw(|frame| {
                frame.render_widget("AAAA", frame.area());
            })
            .unwrap();
        assert_eq!(terminal.backend().queries, 0);
        terminal.backend().inner.assert_buffer_lines(["AAAA"]);
    }
}
