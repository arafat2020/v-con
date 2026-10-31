//! Embedded SSH sessions as sub-tabs and split panes.
//!
//! While a pane is focused, keys go to the PTY except the configurable prefix.
//! After the prefix, a second key switches tabs, splits, closes, or detaches.

use crate::infra::pty::KeyModes;
use crate::services::sessions::SessionId;
use crate::tui::theme::Theme;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tui_term::widget::PseudoTerminal;

const PREFIX_TIMEOUT: Duration = Duration::from_secs(2);

/// Horizontal or vertical split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    /// Side by side.
    Vertical,
    /// Stacked.
    Horizontal,
}

/// Which child of a split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// First pane (left / top).
    First,
    /// Second pane (right / bottom).
    Second,
}

/// Tree of panes inside one tab.
#[derive(Debug, Clone)]
pub enum PaneLayout {
    /// A single session.
    Leaf(SessionId),
    /// Two children sharing the area.
    Split {
        /// Split orientation.
        dir: SplitDir,
        /// Percent of space for the first child (1–99).
        ratio: u16,
        /// Left / top.
        a: Box<PaneLayout>,
        /// Right / bottom.
        b: Box<PaneLayout>,
    },
}

/// One sub-tab on the Sessions screen.
#[derive(Debug, Clone)]
pub struct SessionTab {
    /// Pane tree.
    pub layout: PaneLayout,
    /// Path from the root to the focused leaf.
    pub focus_path: Vec<Side>,
}

/// Result of handling a key while a session pane is focused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionKey {
    /// Nothing to do.
    None,
    /// Forward these bytes to the focused session.
    Write(Vec<u8>),
    /// Open another session for `host` and split the focused pane.
    Split {
        /// Host to connect.
        host: String,
        /// Split orientation.
        dir: SplitDir,
    },
    /// Close the focused session (caller confirms).
    CloseFocused,
    /// Detach focus back to normal vcon navigation.
    Detach,
    /// Reconnect an ended session in place.
    Reconnect {
        /// Session to replace.
        id: SessionId,
        /// Host name.
        host: String,
    },
    /// Ask the shell to close this session after confirm.
    ConfirmClose(SessionId),
    /// Adjust scrollback for the focused session by this many rows (positive = older).
    Scroll(isize),
}

/// UI state for the Sessions screen.
#[derive(Debug, Default)]
pub struct SessionsView {
    /// Open tabs.
    pub tabs: Vec<SessionTab>,
    /// Selected tab index.
    pub active_tab: usize,
    /// When true, keys go to the PTY (except the prefix).
    pub focused: bool,
    /// Waiting for the second key after the prefix.
    pub prefix_until: Option<Instant>,
    /// Scrollback navigation mode.
    pub scrollback: bool,
    /// Sessions with unread output while not the active tab.
    pub unread: HashSet<SessionId>,
    /// Host name per session id (for labels after exit).
    pub hosts: HashMap<SessionId, String>,
    /// Ended sessions still shown with a banner.
    pub ended: HashMap<SessionId, Option<u32>>,
    /// Pending close confirmation.
    pub confirm_close: Option<SessionId>,
    /// Last pane sizes used for resize.
    pub last_sizes: HashMap<SessionId, (u16, u16)>,
    /// Last known application-cursor mode per session.
    pub key_modes: HashMap<SessionId, KeyModes>,
    /// Last known bracketed-paste mode per session.
    pub bracketed_paste: HashMap<SessionId, bool>,
}

impl SessionsView {
    /// Empty view.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of open tabs.
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// True when any session is still running (not in `ended`).
    pub fn has_live(&self) -> bool {
        self.hosts
            .keys()
            .any(|id| !self.ended.contains_key(id) && self.contains(*id))
    }

    /// Whether the layout still references `id`.
    pub fn contains(&self, id: SessionId) -> bool {
        self.tabs.iter().any(|tab| layout_contains(&tab.layout, id))
    }

    /// Focused session on the active tab.
    pub fn focused_id(&self) -> Option<SessionId> {
        let tab = self.tabs.get(self.active_tab)?;
        leaf_at(&tab.layout, &tab.focus_path)
    }

    /// Host for the focused pane.
    pub fn focused_host(&self) -> Option<String> {
        self.focused_id()
            .and_then(|id| self.hosts.get(&id).cloned())
    }

    /// Add a new tab for `id`.
    pub fn open_tab(&mut self, id: SessionId, host: String) {
        self.hosts.insert(id, host);
        self.tabs.push(SessionTab {
            layout: PaneLayout::Leaf(id),
            focus_path: Vec::new(),
        });
        self.active_tab = self.tabs.len() - 1;
        self.focused = true;
        self.scrollback = false;
        self.prefix_until = None;
        self.unread.remove(&id);
    }

    /// Replace `old` with `new` after a reconnect (same pane).
    pub fn replace_session(&mut self, old: SessionId, new: SessionId, host: String) {
        for tab in &mut self.tabs {
            replace_in_layout(&mut tab.layout, old, new);
        }
        self.hosts.remove(&old);
        self.ended.remove(&old);
        self.hosts.insert(new, host);
        self.last_sizes.remove(&old);
        self.unread.remove(&old);
        self.unread.remove(&new);
        self.confirm_close = None;
    }

    /// Split the focused leaf; `new_id` becomes the other pane and gets focus.
    pub fn split_focused(&mut self, new_id: SessionId, host: String, dir: SplitDir) -> bool {
        let Some(old) = self.focused_id() else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return false;
        };
        if !split_leaf(&mut tab.layout, &tab.focus_path, old, new_id, dir) {
            return false;
        }
        tab.focus_path.push(Side::Second);
        self.hosts.insert(new_id, host);
        self.focused = true;
        self.scrollback = false;
        true
    }

    /// Remove a session from the layout; drop empty tabs.
    pub fn remove_session(&mut self, id: SessionId) {
        self.hosts.remove(&id);
        self.ended.remove(&id);
        self.unread.remove(&id);
        self.last_sizes.remove(&id);
        self.key_modes.remove(&id);
        self.bracketed_paste.remove(&id);
        if self.confirm_close == Some(id) {
            self.confirm_close = None;
        }
        let mut kept = Vec::new();
        for mut tab in self.tabs.drain(..) {
            if remove_from_layout(&mut tab.layout, id) {
                tab.focus_path = default_path(&tab.layout);
                kept.push(tab);
            }
        }
        self.tabs = kept;
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len().saturating_sub(1);
        }
        if self.tabs.is_empty() {
            self.focused = false;
            self.scrollback = false;
        }
    }

    /// Mark a session as ended but keep its pane.
    pub fn mark_ended(&mut self, id: SessionId, code: Option<u32>) {
        self.ended.insert(id, code);
        if self.focused_id() != Some(id) {
            self.unread.insert(id);
        }
    }

    /// Note output; mark unread when not the focused pane on the active tab.
    pub fn note_output(&mut self, id: SessionId) {
        let focused_here = self.focused && self.focused_id() == Some(id);
        if focused_here {
            self.unread.remove(&id);
        } else {
            self.unread.insert(id);
        }
    }

    /// Clear unread for the active tab's sessions.
    pub fn mark_active_read(&mut self) {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            for id in collect_ids(&tab.layout) {
                self.unread.remove(&id);
            }
        }
    }

    /// Next / previous tab.
    pub fn cycle_tab(&mut self, forward: bool) {
        if self.tabs.is_empty() {
            return;
        }
        if forward {
            self.active_tab = (self.active_tab + 1) % self.tabs.len();
        } else {
            self.active_tab = if self.active_tab == 0 {
                self.tabs.len() - 1
            } else {
                self.active_tab - 1
            };
        }
        self.mark_active_read();
        self.scrollback = false;
        self.prefix_until = None;
    }

    /// Jump to tab `index` (0-based).
    pub fn jump_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active_tab = index;
            self.mark_active_read();
            self.scrollback = false;
            self.prefix_until = None;
        }
    }

    /// Move focus to a neighbouring pane.
    pub fn move_focus(&mut self, code: KeyCode) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let side = match code {
            KeyCode::Left | KeyCode::Up => Side::First,
            KeyCode::Right | KeyCode::Down => Side::Second,
            _ => return,
        };
        if let Some(path) = neighbour_path(&tab.layout, &tab.focus_path, code) {
            tab.focus_path = path;
        } else if tab.focus_path.last() != Some(&side) {
            // Prefer stepping into the other side of the nearest split.
            if let Some(last) = tab.focus_path.last_mut() {
                *last = side;
            }
        }
        self.mark_active_read();
        self.scrollback = false;
    }

    /// Expire a stale prefix wait on tick.
    pub fn tick_prefix(&mut self) -> bool {
        if let Some(until) = self.prefix_until
            && Instant::now() >= until
        {
            self.prefix_until = None;
            return true;
        }
        false
    }

    /// Handle a key while the Sessions screen may own input.
    pub fn on_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        is_prefix: bool,
        encode: impl FnOnce(KeyCode, KeyModifiers) -> Vec<u8>,
    ) -> SessionKey {
        if let Some(id) = self.confirm_close {
            return match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.confirm_close = None;
                    SessionKey::CloseFocused
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirm_close = None;
                    SessionKey::None
                }
                _ => {
                    let _ = id;
                    SessionKey::None
                }
            };
        }

        if self.scrollback {
            return self.scrollback_key(code);
        }

        if self.prefix_until.is_some() {
            self.prefix_until = None;
            return self.prefix_command(code, modifiers, is_prefix, encode);
        }

        if is_prefix {
            self.prefix_until = Some(Instant::now() + PREFIX_TIMEOUT);
            return SessionKey::None;
        }

        if !self.focused {
            return SessionKey::None;
        }

        // Ended session: Enter reconnects; x closes.
        if let Some(id) = self.focused_id()
            && self.ended.contains_key(&id)
        {
            return match code {
                KeyCode::Enter => {
                    let host = self.hosts.get(&id).cloned().unwrap_or_default();
                    SessionKey::Reconnect { id, host }
                }
                KeyCode::Char('x') => {
                    self.confirm_close = Some(id);
                    SessionKey::ConfirmClose(id)
                }
                _ => SessionKey::None,
            };
        }

        let bytes = encode(code, modifiers);
        if bytes.is_empty() {
            SessionKey::None
        } else {
            SessionKey::Write(bytes)
        }
    }

    fn prefix_command(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        is_prefix: bool,
        encode: impl FnOnce(KeyCode, KeyModifiers) -> Vec<u8>,
    ) -> SessionKey {
        if is_prefix {
            let bytes = encode(code, modifiers);
            return if bytes.is_empty() {
                SessionKey::None
            } else {
                SessionKey::Write(bytes)
            };
        }
        match code {
            KeyCode::Char('n') => {
                self.cycle_tab(true);
                SessionKey::None
            }
            KeyCode::Char('p') => {
                self.cycle_tab(false);
                SessionKey::None
            }
            KeyCode::Char(digit @ '1'..='9') => {
                let index = (digit as u8 - b'1') as usize;
                self.jump_tab(index);
                SessionKey::None
            }
            KeyCode::Char('|') => {
                let Some(host) = self.focused_host() else {
                    return SessionKey::None;
                };
                SessionKey::Split {
                    host,
                    dir: SplitDir::Vertical,
                }
            }
            KeyCode::Char('-') => {
                let Some(host) = self.focused_host() else {
                    return SessionKey::None;
                };
                SessionKey::Split {
                    host,
                    dir: SplitDir::Horizontal,
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                self.move_focus(code);
                SessionKey::None
            }
            KeyCode::Char('x') => {
                if let Some(id) = self.focused_id() {
                    self.confirm_close = Some(id);
                    SessionKey::ConfirmClose(id)
                } else {
                    SessionKey::None
                }
            }
            KeyCode::Char('[') => {
                self.scrollback = true;
                SessionKey::None
            }
            KeyCode::Char('d') => {
                self.focused = false;
                self.scrollback = false;
                SessionKey::Detach
            }
            _ => SessionKey::None,
        }
    }

    fn scrollback_key(&mut self, code: KeyCode) -> SessionKey {
        match code {
            KeyCode::Esc => {
                self.scrollback = false;
                SessionKey::None
            }
            KeyCode::Up | KeyCode::Char('k') => SessionKey::Scroll(1),
            KeyCode::Down | KeyCode::Char('j') => SessionKey::Scroll(-1),
            KeyCode::PageUp => SessionKey::Scroll(10),
            KeyCode::PageDown => SessionKey::Scroll(-10),
            _ => SessionKey::None,
        }
    }

    /// Compute pane rectangles and desired PTY sizes inside `area`.
    pub fn pane_sizes(&self, area: Rect) -> Vec<(SessionId, Rect, u16, u16)> {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        collect_pane_sizes(&tab.layout, area, &mut out);
        out
    }

    /// Diff against `last_sizes` and return sessions that need a resize.
    pub fn take_resizes(&mut self, area: Rect) -> Vec<(SessionId, u16, u16)> {
        let panes = self.pane_sizes(area);
        let mut changed = Vec::new();
        for (id, _rect, rows, cols) in &panes {
            let size = (*rows, *cols);
            if self.last_sizes.get(id) != Some(&size) {
                self.last_sizes.insert(*id, size);
                changed.push((*id, *rows, *cols));
            }
        }
        changed
    }
}

/// Callback used to borrow a session's vt100 screen while rendering a pane.
type ScreenCallback<'a> = dyn FnMut(SessionId, &mut dyn FnMut(&vt100::Screen)) + 'a;

/// Draw the Sessions screen.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    view: &SessionsView,
    theme: &Theme,
    with_screen: &mut ScreenCallback<'_>,
) {
    if view.tabs.is_empty() {
        frame.render_widget(
            Paragraph::new("No sessions yet.\n\nPress o on a host to open one.")
                .style(theme.muted())
                .alignment(Alignment::Center)
                .block(
                    theme
                        .block()
                        .title(Span::styled(" Sessions ", theme.title()))
                        .border_style(theme.border()),
                ),
            area,
        );
        return;
    }
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
    draw_tab_bar(frame, chunks[0], view, theme);
    let Some(tab) = view.tabs.get(view.active_tab) else {
        return;
    };
    draw_layout(
        frame,
        chunks[1],
        &tab.layout,
        &tab.focus_path,
        &[],
        view,
        theme,
        with_screen,
    );
    if view.prefix_until.is_some() {
        let label = Paragraph::new(" PREFIX ").style(
            Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                .fg(theme.highlight),
        );
        let width = 9u16;
        let x = area.x + area.width.saturating_sub(width + 1);
        let y = area.y;
        frame.render_widget(label, Rect::new(x, y, width.min(area.width), 1));
    }
    if view.confirm_close.is_some() {
        let msg = Paragraph::new("Close session? y / n").style(theme.warning());
        let y = area.y + area.height.saturating_sub(1);
        frame.render_widget(msg, Rect::new(area.x, y, area.width, 1));
    }
}

fn draw_tab_bar(frame: &mut Frame, area: Rect, view: &SessionsView, theme: &Theme) {
    let mut spans = Vec::new();
    for (index, tab) in view.tabs.iter().enumerate() {
        let ids = collect_ids(&tab.layout);
        let label = ids
            .first()
            .and_then(|id| view.hosts.get(id))
            .cloned()
            .unwrap_or_else(|| "?".into());
        let ended = ids.iter().all(|id| view.ended.contains_key(id));
        let unread = ids.iter().any(|id| view.unread.contains(id));
        let mut text = format!("{}:{label}", index + 1);
        if ended {
            text.push_str(" (ended)");
        } else if unread {
            text.push('*');
        }
        let style = if index == view.active_tab {
            theme.tab_active()
        } else {
            theme.tab_inactive()
        };
        spans.push(Span::styled(format!(" {text} "), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(theme.bar()), area);
}

#[allow(clippy::too_many_arguments)]
fn draw_layout(
    frame: &mut Frame,
    area: Rect,
    layout: &PaneLayout,
    focus_path: &[Side],
    path: &[Side],
    view: &SessionsView,
    theme: &Theme,
    with_screen: &mut ScreenCallback<'_>,
) {
    match layout {
        PaneLayout::Leaf(id) => {
            let focused = view.focused && path == focus_path;
            draw_pane(frame, area, *id, focused, view, theme, with_screen);
        }
        PaneLayout::Split { dir, ratio, a, b } => {
            let ratio = (*ratio).clamp(1, 99);
            let constraints = [
                Constraint::Percentage(ratio),
                Constraint::Percentage(100 - ratio),
            ];
            let direction = match dir {
                SplitDir::Vertical => Direction::Horizontal,
                SplitDir::Horizontal => Direction::Vertical,
            };
            let chunks = Layout::new(direction, constraints).split(area);
            let mut path_a = path.to_vec();
            path_a.push(Side::First);
            let mut path_b = path.to_vec();
            path_b.push(Side::Second);
            draw_layout(
                frame,
                chunks[0],
                a,
                focus_path,
                &path_a,
                view,
                theme,
                with_screen,
            );
            draw_layout(
                frame,
                chunks[1],
                b,
                focus_path,
                &path_b,
                view,
                theme,
                with_screen,
            );
        }
    }
}

fn draw_pane(
    frame: &mut Frame,
    area: Rect,
    id: SessionId,
    focused: bool,
    view: &SessionsView,
    theme: &Theme,
    with_screen: &mut ScreenCallback<'_>,
) {
    let host = view.hosts.get(&id).map(String::as_str).unwrap_or("?");
    let ended = view.ended.contains_key(&id);
    let title = if ended {
        format!(" {host} (ended) ")
    } else {
        format!(" {host} ")
    };
    let block = theme
        .block()
        .title(Span::styled(title, theme.title()))
        .border_style(theme.pane_border(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if ended {
        with_screen(id, &mut |screen| {
            let term = PseudoTerminal::new(screen);
            frame.render_widget(term, inner);
        });
        let banner = " session ended — Enter to reconnect, x to close ";
        let y = inner.y + inner.height.saturating_sub(1);
        frame.render_widget(
            Paragraph::new(banner).style(theme.warning()),
            Rect::new(inner.x, y, inner.width, 1),
        );
        return;
    }

    with_screen(id, &mut |screen| {
        let term = PseudoTerminal::new(screen);
        frame.render_widget(term, inner);
        if focused && !view.scrollback && !screen.hide_cursor() {
            let (row, col) = screen.cursor_position();
            let x = inner.x.saturating_add(col);
            let y = inner.y.saturating_add(row);
            if x < inner.x.saturating_add(inner.width) && y < inner.y.saturating_add(inner.height) {
                frame.set_cursor_position((x, y));
            }
        }
    });
}

fn layout_contains(layout: &PaneLayout, id: SessionId) -> bool {
    match layout {
        PaneLayout::Leaf(leaf) => *leaf == id,
        PaneLayout::Split { a, b, .. } => layout_contains(a, id) || layout_contains(b, id),
    }
}

fn collect_ids(layout: &PaneLayout) -> Vec<SessionId> {
    match layout {
        PaneLayout::Leaf(id) => vec![*id],
        PaneLayout::Split { a, b, .. } => {
            let mut ids = collect_ids(a);
            ids.extend(collect_ids(b));
            ids
        }
    }
}

fn leaf_at(layout: &PaneLayout, path: &[Side]) -> Option<SessionId> {
    match layout {
        PaneLayout::Leaf(id) => Some(*id),
        PaneLayout::Split { a, b, .. } => {
            let (side, rest) = path.split_first()?;
            match side {
                Side::First => leaf_at(a, rest),
                Side::Second => leaf_at(b, rest),
            }
        }
    }
}

fn replace_in_layout(layout: &mut PaneLayout, old: SessionId, new: SessionId) {
    match layout {
        PaneLayout::Leaf(id) if *id == old => *id = new,
        PaneLayout::Leaf(_) => {}
        PaneLayout::Split { a, b, .. } => {
            replace_in_layout(a, old, new);
            replace_in_layout(b, old, new);
        }
    }
}

fn split_leaf(
    layout: &mut PaneLayout,
    path: &[Side],
    old: SessionId,
    new: SessionId,
    dir: SplitDir,
) -> bool {
    match layout {
        PaneLayout::Leaf(id) if *id == old && path.is_empty() => {
            *layout = PaneLayout::Split {
                dir,
                ratio: 50,
                a: Box::new(PaneLayout::Leaf(old)),
                b: Box::new(PaneLayout::Leaf(new)),
            };
            true
        }
        PaneLayout::Leaf(_) => false,
        PaneLayout::Split { a, b, .. } => {
            let Some((side, rest)) = path.split_first() else {
                return false;
            };
            match side {
                Side::First => split_leaf(a, rest, old, new, dir),
                Side::Second => split_leaf(b, rest, old, new, dir),
            }
        }
    }
}

/// Remove `id`. Returns true if the layout still has sessions.
fn remove_from_layout(layout: &mut PaneLayout, id: SessionId) -> bool {
    match layout {
        PaneLayout::Leaf(leaf) => *leaf != id,
        PaneLayout::Split { a, b, .. } => {
            let a_ok = remove_from_layout(a, id);
            let b_ok = remove_from_layout(b, id);
            match (a_ok, b_ok) {
                (true, true) => true,
                (true, false) => {
                    *layout = a.as_ref().clone();
                    true
                }
                (false, true) => {
                    *layout = b.as_ref().clone();
                    true
                }
                (false, false) => false,
            }
        }
    }
}

fn neighbour_path(layout: &PaneLayout, path: &[Side], code: KeyCode) -> Option<Vec<Side>> {
    // Simple: flip the last matching split side when direction aligns.
    let want = match code {
        KeyCode::Left | KeyCode::Up => Side::First,
        KeyCode::Right | KeyCode::Down => Side::Second,
        _ => return None,
    };
    let horiz = matches!(code, KeyCode::Left | KeyCode::Right);
    let mut cursor = layout;
    let mut new_path = Vec::new();
    for (i, side) in path.iter().enumerate() {
        let PaneLayout::Split { dir, a, b, .. } = cursor else {
            return None;
        };
        let aligns = match dir {
            SplitDir::Vertical => horiz,
            SplitDir::Horizontal => !horiz,
        };
        if aligns && i + 1 == path.len() && *side != want {
            new_path.push(want);
            // descend into default leaf path on that side
            let child = match want {
                Side::First => a.as_ref(),
                Side::Second => b.as_ref(),
            };
            new_path.extend(default_path(child));
            return Some(new_path);
        }
        new_path.push(*side);
        cursor = match side {
            Side::First => a,
            Side::Second => b,
        };
    }
    None
}

fn default_path(layout: &PaneLayout) -> Vec<Side> {
    match layout {
        PaneLayout::Leaf(_) => Vec::new(),
        PaneLayout::Split { a, .. } => {
            let mut path = vec![Side::First];
            path.extend(default_path(a));
            path
        }
    }
}

fn collect_pane_sizes(layout: &PaneLayout, area: Rect, out: &mut Vec<(SessionId, Rect, u16, u16)>) {
    match layout {
        PaneLayout::Leaf(id) => {
            let inner_rows = area.height.saturating_sub(2).max(1);
            let inner_cols = area.width.saturating_sub(2).max(1);
            out.push((*id, area, inner_rows, inner_cols));
        }
        PaneLayout::Split { dir, ratio, a, b } => {
            let ratio = (*ratio).clamp(1, 99);
            let constraints = [
                Constraint::Percentage(ratio),
                Constraint::Percentage(100 - ratio),
            ];
            let direction = match dir {
                SplitDir::Vertical => Direction::Horizontal,
                SplitDir::Horizontal => Direction::Vertical,
            };
            let chunks = Layout::new(direction, constraints).split(area);
            collect_pane_sizes(a, chunks[0], out);
            collect_pane_sizes(b, chunks[1], out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(n: u64) -> SessionId {
        SessionId::from_raw(n)
    }

    #[test]
    fn open_tab_and_cycle() {
        let mut view = SessionsView::new();
        view.open_tab(sid(1), "web-1".into());
        view.open_tab(sid(2), "db-1".into());
        assert_eq!(view.active_tab, 1);
        view.cycle_tab(false);
        assert_eq!(view.active_tab, 0);
        view.jump_tab(1);
        assert_eq!(view.focused_host().as_deref(), Some("db-1"));
    }

    #[test]
    fn split_adds_second_pane() {
        let mut view = SessionsView::new();
        view.open_tab(sid(1), "web-1".into());
        assert!(view.split_focused(sid(2), "web-1".into(), SplitDir::Vertical));
        assert_eq!(view.focused_id(), Some(sid(2)));
        assert_eq!(view.tab_count(), 1);
        assert_eq!(collect_ids(&view.tabs[0].layout).len(), 2);
    }

    #[test]
    fn remove_collapses_split() {
        let mut view = SessionsView::new();
        view.open_tab(sid(1), "a".into());
        view.split_focused(sid(2), "b".into(), SplitDir::Horizontal);
        view.remove_session(sid(2));
        assert_eq!(view.focused_id(), Some(sid(1)));
        assert!(matches!(view.tabs[0].layout, PaneLayout::Leaf(_)));
    }
}
