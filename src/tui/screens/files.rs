//! Two-pane local and remote file browser.

use crate::domain::host::EffectiveHost;
use crate::infra::remote_fs::{DirEntry, EntryKind, join, parent_of};
use crate::services::files::{
    self as file_svc, ConflictAction, SortKey, TransferDirection, TransferJob, TransferQueue,
    TransferState, format_age, format_mode, format_size,
};
use crate::tui::theme::Theme;
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::SystemTime;

/// Which pane has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneFocus {
    /// Local left pane.
    Local,
    /// Remote right pane.
    Remote,
}

/// One side of the browser.
#[derive(Debug)]
pub struct Pane {
    /// Absolute path shown in the title.
    pub path: String,
    /// Current listing.
    pub entries: Vec<DirEntry>,
    /// Cursor index into `entries`.
    pub cursor: usize,
    /// Marked basenames.
    pub marks: BTreeSet<String>,
    /// Filter text without `/`.
    pub filter: String,
    /// True while typing a filter.
    pub filtering: bool,
}

impl Pane {
    /// Empty pane at `path`.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            entries: Vec::new(),
            cursor: 0,
            marks: BTreeSet::new(),
            filter: String::new(),
            filtering: false,
        }
    }

    /// Visible rows after the text filter.
    pub fn visible(&self) -> Vec<&DirEntry> {
        if self.filter.is_empty() {
            return self.entries.iter().collect();
        }
        let q = self.filter.to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|entry| entry.name.to_ascii_lowercase().contains(&q))
            .collect()
    }

    /// Entry under the cursor, if any.
    pub fn current(&self) -> Option<&DirEntry> {
        let visible = self.visible();
        visible.get(self.cursor).copied()
    }

    /// Clamp cursor into range.
    pub fn clamp(&mut self) {
        let len = self.visible().len();
        if len == 0 {
            self.cursor = 0;
        } else if self.cursor >= len {
            self.cursor = len - 1;
        }
    }

    /// Move by `delta` rows.
    pub fn move_by(&mut self, delta: isize) {
        let len = self.visible().len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        if delta < 0 {
            self.cursor = self.cursor.saturating_sub((-delta) as usize);
        } else {
            self.cursor = (self.cursor + delta as usize).min(len - 1);
        }
    }
}

/// Pending conflict decision.
#[derive(Debug, Clone)]
pub struct ConflictDialog {
    /// Job id waiting on a decision.
    #[allow(dead_code)]
    pub id: u64,
    /// Destination path.
    pub path: String,
}

/// Pending delete confirmation.
#[derive(Debug, Clone)]
pub struct DeleteConfirm {
    /// Paths that will be removed.
    pub paths: Vec<String>,
    /// True when deleting on the remote pane.
    pub remote: bool,
}

/// Pending mkdir / rename input.
#[derive(Debug, Clone)]
pub struct NamePrompt {
    /// Title.
    pub title: String,
    /// Typed value.
    pub value: String,
    /// What submitting does.
    pub kind: NameKind,
}

/// Kind of name prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    /// Create a directory in the focused pane.
    Mkdir,
    /// Rename the cursor entry.
    Rename,
    /// Jump to a typed path.
    Goto,
}

/// Host picker overlay.
#[derive(Debug, Clone)]
pub struct HostPicker {
    /// Filter text.
    pub query: String,
    /// Cursor into filtered hosts.
    pub cursor: usize,
}

/// Events the app should handle after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesEvent {
    /// Nothing further.
    None,
    /// Reload the focused pane listing.
    ReloadFocus,
    /// Reload both panes.
    ReloadBoth,
    /// Open directory or view file at cursor.
    Activate,
    /// Go up one directory.
    Parent,
    /// Start a copy of marked/cursor entries to the other pane.
    Copy,
    /// Confirm delete.
    ConfirmDelete,
    /// Perform delete.
    DoDelete,
    /// Submit name prompt.
    SubmitName,
    /// Cancel overlays.
    CancelOverlay,
    /// Choose host at picker cursor.
    PickHost,
    /// Conflict choice.
    Conflict(ConflictAction),
    /// Cancel running transfers.
    CancelTransfers,
    /// Toggle transfer panel collapsed.
    ToggleQueue,
}

/// Files screen state.
#[derive(Debug)]
pub struct FilesView {
    /// Local pane.
    pub local: Pane,
    /// Remote pane.
    pub remote: Pane,
    /// Focused pane.
    pub focus: PaneFocus,
    /// Remote host name.
    pub host: Option<String>,
    /// Sort key for both panes.
    pub sort: SortKey,
    /// Show dotfiles.
    pub show_hidden: bool,
    /// Transfer queue.
    pub queue: TransferQueue,
    /// Collapse the transfer panel.
    pub queue_collapsed: bool,
    /// Conflict dialog.
    pub conflict: Option<ConflictDialog>,
    /// Delete confirm.
    pub delete: Option<DeleteConfirm>,
    /// Name prompt.
    pub prompt: Option<NamePrompt>,
    /// Host picker.
    pub picker: Option<HostPicker>,
    /// Read-only file view.
    pub viewer: Option<Viewer>,
    /// Status / error line.
    pub status: Option<String>,
}

/// Scrollable text viewer for remote/local files.
#[derive(Debug, Clone)]
pub struct Viewer {
    /// Title path.
    pub title: String,
    /// Lines of text.
    pub lines: Vec<String>,
    /// Scroll offset.
    pub offset: usize,
}

impl FilesView {
    /// Start with local home and empty remote until a host is chosen.
    pub fn new(local_home: PathBuf) -> Self {
        Self {
            local: Pane::new(local_home.to_string_lossy().into_owned()),
            remote: Pane::new("/"),
            focus: PaneFocus::Local,
            host: None,
            sort: SortKey::Name,
            show_hidden: false,
            queue: TransferQueue::default(),
            queue_collapsed: false,
            conflict: None,
            delete: None,
            prompt: None,
            picker: None,
            viewer: None,
            status: None,
        }
    }

    /// Focused pane.
    pub fn focused(&mut self) -> &mut Pane {
        match self.focus {
            PaneFocus::Local => &mut self.local,
            PaneFocus::Remote => &mut self.remote,
        }
    }

    /// Focused pane (shared).
    pub fn focused_ref(&self) -> &Pane {
        match self.focus {
            PaneFocus::Local => &self.local,
            PaneFocus::Remote => &self.remote,
        }
    }

    /// Other pane.
    pub fn other(&self) -> &Pane {
        match self.focus {
            PaneFocus::Local => &self.remote,
            PaneFocus::Remote => &self.local,
        }
    }

    /// Apply a prepared listing to a pane.
    pub fn set_listing(&mut self, focus: PaneFocus, path: String, entries: Vec<DirEntry>) {
        let pane = match focus {
            PaneFocus::Local => &mut self.local,
            PaneFocus::Remote => &mut self.remote,
        };
        let path_changed = pane.path != path;
        pane.path = path;
        pane.entries = file_svc::prepare_listing(entries, self.sort, self.show_hidden);
        pane.marks.clear();
        if path_changed {
            pane.cursor = 0;
        }
        pane.clamp();
    }

    /// Paths marked or the cursor entry.
    pub fn selected_paths(&self) -> Vec<String> {
        let pane = self.focused_ref();
        if !pane.marks.is_empty() {
            return pane
                .marks
                .iter()
                .map(|name| join(&pane.path, name))
                .collect();
        }
        pane.current()
            .map(|entry| join(&pane.path, &entry.name))
            .into_iter()
            .collect()
    }
}

/// Handle a key on the files screen.
pub fn on_key(view: &mut FilesView, code: KeyCode, hosts: &[EffectiveHost]) -> FilesEvent {
    if let Some(viewer) = &mut view.viewer {
        return match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                view.viewer = None;
                FilesEvent::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                viewer.offset = viewer.offset.saturating_sub(1);
                FilesEvent::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                viewer.offset = viewer.offset.saturating_add(1);
                FilesEvent::None
            }
            KeyCode::PageUp => {
                viewer.offset = viewer.offset.saturating_sub(20);
                FilesEvent::None
            }
            KeyCode::PageDown => {
                viewer.offset = viewer.offset.saturating_add(20);
                FilesEvent::None
            }
            _ => FilesEvent::None,
        };
    }
    if let Some(conflict) = &view.conflict {
        let _ = conflict;
        return match code {
            KeyCode::Char('o') => FilesEvent::Conflict(ConflictAction::Overwrite),
            KeyCode::Char('O') => FilesEvent::Conflict(ConflictAction::OverwriteAll),
            KeyCode::Char('s') => FilesEvent::Conflict(ConflictAction::Skip),
            KeyCode::Char('S') => FilesEvent::Conflict(ConflictAction::SkipAll),
            KeyCode::Char('r') => FilesEvent::Conflict(ConflictAction::Rename),
            KeyCode::Esc => FilesEvent::CancelOverlay,
            _ => FilesEvent::None,
        };
    }
    if view.delete.is_some() {
        return match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => FilesEvent::DoDelete,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => FilesEvent::CancelOverlay,
            _ => FilesEvent::None,
        };
    }
    if let Some(prompt) = &mut view.prompt {
        return match code {
            KeyCode::Esc => FilesEvent::CancelOverlay,
            KeyCode::Enter => FilesEvent::SubmitName,
            KeyCode::Backspace => {
                prompt.value.pop();
                FilesEvent::None
            }
            KeyCode::Char(ch) => {
                prompt.value.push(ch);
                FilesEvent::None
            }
            _ => FilesEvent::None,
        };
    }
    if let Some(picker) = &mut view.picker {
        let names: Vec<&str> = hosts
            .iter()
            .map(|host| host.name.as_str())
            .filter(|name| {
                picker.query.is_empty()
                    || name
                        .to_ascii_lowercase()
                        .contains(&picker.query.to_ascii_lowercase())
            })
            .collect();
        return match code {
            KeyCode::Esc => FilesEvent::CancelOverlay,
            KeyCode::Enter => FilesEvent::PickHost,
            KeyCode::Up | KeyCode::Char('k') => {
                picker.cursor = picker.cursor.saturating_sub(1);
                FilesEvent::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !names.is_empty() {
                    picker.cursor = (picker.cursor + 1).min(names.len() - 1);
                }
                FilesEvent::None
            }
            KeyCode::Backspace => {
                picker.query.pop();
                picker.cursor = 0;
                FilesEvent::None
            }
            KeyCode::Char(ch) => {
                picker.query.push(ch);
                picker.cursor = 0;
                FilesEvent::None
            }
            _ => FilesEvent::None,
        };
    }

    let filtering = view.focused().filtering;
    if filtering {
        return match code {
            KeyCode::Esc => {
                let pane = view.focused();
                pane.filtering = false;
                pane.filter.clear();
                pane.clamp();
                FilesEvent::None
            }
            KeyCode::Enter => {
                view.focused().filtering = false;
                FilesEvent::None
            }
            KeyCode::Backspace => {
                let pane = view.focused();
                pane.filter.pop();
                pane.clamp();
                FilesEvent::None
            }
            KeyCode::Char(ch) => {
                let pane = view.focused();
                pane.filter.push(ch);
                pane.clamp();
                FilesEvent::None
            }
            _ => FilesEvent::None,
        };
    }

    match code {
        KeyCode::Tab => {
            view.focus = match view.focus {
                PaneFocus::Local => PaneFocus::Remote,
                PaneFocus::Remote => PaneFocus::Local,
            };
            FilesEvent::None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            view.focused().move_by(-1);
            FilesEvent::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            view.focused().move_by(1);
            FilesEvent::None
        }
        KeyCode::PageUp => {
            view.focused().move_by(-20);
            FilesEvent::None
        }
        KeyCode::PageDown => {
            view.focused().move_by(20);
            FilesEvent::None
        }
        // Enter / l / → open (dirs navigate, files view). h / ← go up.
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => FilesEvent::Activate,
        KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => FilesEvent::Parent,
        KeyCode::Char(' ') => {
            if let Some(name) = view.focused().current().map(|entry| entry.name.clone()) {
                let pane = view.focused();
                if !pane.marks.remove(&name) {
                    pane.marks.insert(name);
                }
            }
            FilesEvent::None
        }
        KeyCode::Char('c') | KeyCode::F(5) => FilesEvent::Copy,
        KeyCode::Char('d') | KeyCode::F(8) => FilesEvent::ConfirmDelete,
        KeyCode::Char('m') => {
            view.prompt = Some(NamePrompt {
                title: "mkdir".into(),
                value: String::new(),
                kind: NameKind::Mkdir,
            });
            FilesEvent::None
        }
        KeyCode::Char('r') => {
            let name = view
                .focused()
                .current()
                .map(|entry| entry.name.clone())
                .unwrap_or_default();
            view.prompt = Some(NamePrompt {
                title: "rename".into(),
                value: name,
                kind: NameKind::Rename,
            });
            FilesEvent::None
        }
        KeyCode::Char('g') | KeyCode::Char('~') => {
            let path = if matches!(code, KeyCode::Char('~')) {
                if view.focus == PaneFocus::Local {
                    dirs_home()
                } else {
                    "/".into()
                }
            } else {
                view.focused_ref().path.clone()
            };
            view.prompt = Some(NamePrompt {
                title: "go to".into(),
                value: path,
                kind: NameKind::Goto,
            });
            FilesEvent::None
        }
        KeyCode::Char('/') => {
            view.focused().filtering = true;
            FilesEvent::None
        }
        KeyCode::Char('.') => {
            view.show_hidden = !view.show_hidden;
            FilesEvent::ReloadBoth
        }
        KeyCode::Char('s') => {
            view.sort = view.sort.next();
            FilesEvent::ReloadBoth
        }
        KeyCode::Char('H') => {
            view.picker = Some(HostPicker {
                query: String::new(),
                cursor: 0,
            });
            FilesEvent::None
        }
        KeyCode::Char('v') => FilesEvent::Activate, // view handled in activate for files
        KeyCode::Char('t') => FilesEvent::ToggleQueue,
        KeyCode::Char('C') => FilesEvent::CancelTransfers,
        KeyCode::Char('R') => FilesEvent::ReloadFocus,
        _ => FilesEvent::None,
    }
}

fn dirs_home() -> String {
    directories::UserDirs::new()
        .map(|dirs| dirs.home_dir().to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_string())
}

/// Draw the files screen.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    view: &mut FilesView,
    hosts: &[EffectiveHost],
    theme: &Theme,
) {
    let queue_height = if view.queue_collapsed {
        1
    } else {
        (view.queue.jobs.len().min(4) as u16)
            .saturating_add(2)
            .max(3)
    };
    let chunks =
        Layout::vertical([Constraint::Min(0), Constraint::Length(queue_height)]).split(area);
    let panes = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[0]);
    let sort = view.sort.label();
    draw_pane(
        frame,
        panes[0],
        &view.local,
        &format!("Local [{sort}]"),
        view.focus == PaneFocus::Local,
        theme,
    );
    let remote_title = match &view.host {
        Some(name) => format!("{name} [{sort}]"),
        None => format!("Remote (H to pick host) [{sort}]"),
    };
    draw_pane(
        frame,
        panes[1],
        &view.remote,
        &remote_title,
        view.focus == PaneFocus::Remote,
        theme,
    );
    draw_queue(frame, chunks[1], view, theme);
    if let Some(status) = &view.status {
        frame.render_widget(
            Paragraph::new(status.as_str()).style(theme.muted()),
            Rect {
                x: area.x,
                y: area.y + area.height.saturating_sub(1),
                width: area.width,
                height: 1,
            },
        );
    }

    if let Some(viewer) = &view.viewer {
        draw_viewer(frame, area, viewer, theme);
    }
    if let Some(conflict) = &view.conflict {
        draw_message(
            frame,
            area,
            "Conflict",
            &format!(
                "{}\nexists. o overwrite  O all  s skip  S all  r rename  Esc cancel",
                conflict.path
            ),
            theme,
        );
    }
    if let Some(delete) = &view.delete {
        let list = delete
            .paths
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let extra = if delete.paths.len() > 8 {
            format!("\n…and {} more", delete.paths.len() - 8)
        } else {
            String::new()
        };
        draw_message(
            frame,
            area,
            "Delete?",
            &format!("{list}{extra}\n\ny yes   n/Esc no"),
            theme,
        );
    }
    if let Some(prompt) = &view.prompt {
        draw_message(
            frame,
            area,
            &prompt.title,
            &format!("{}_", prompt.value),
            theme,
        );
    }
    if let Some(picker) = &view.picker {
        let names: Vec<String> = hosts
            .iter()
            .map(|host| host.name.clone())
            .filter(|name| {
                picker.query.is_empty()
                    || name
                        .to_ascii_lowercase()
                        .contains(&picker.query.to_ascii_lowercase())
            })
            .collect();
        let body = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let mark = if index == picker.cursor { ">" } else { " " };
                format!("{mark} {name}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        draw_message(
            frame,
            area,
            &format!("Host /{}", picker.query),
            &body,
            theme,
        );
    }
}

fn draw_pane(
    frame: &mut Frame,
    area: Rect,
    pane: &Pane,
    title: &str,
    focused: bool,
    theme: &Theme,
) {
    let filter = if pane.filtering {
        format!(" /{}_", pane.filter)
    } else if !pane.filter.is_empty() {
        format!(" /{}", pane.filter)
    } else {
        String::new()
    };
    let title = format!(" {title}: {}{filter} ", pane.path);
    let now = SystemTime::now();
    let visible = pane.visible();
    let items: Vec<ListItem> = visible
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let marked = pane.marks.contains(&entry.name);
            let cursor = index == pane.cursor;
            let glyph = match entry.kind {
                EntryKind::Dir => "▸",
                EntryKind::Symlink => "↗",
                _ => " ",
            };
            let mark = if marked {
                "●"
            } else if cursor {
                ">"
            } else {
                " "
            };
            let size = if entry.is_dir() {
                "-".to_string()
            } else {
                format_size(entry.size)
            };
            let line = format!(
                "{mark}{glyph} {:<22} {:>6} {:>4} {}",
                truncate(&entry.name, 22),
                size,
                format_age(entry.mtime, now),
                format_mode(entry.kind, entry.mode),
            );
            let style = if cursor && focused {
                theme.selected()
            } else if entry.is_dir() {
                theme.directory()
            } else if matches!(entry.kind, EntryKind::Symlink) {
                theme.symlink()
            } else {
                theme.text()
            };
            ListItem::new(Line::from(Span::styled(line, style)))
        })
        .collect();
    let block = Block::bordered()
        .title(Span::styled(title, theme.title()))
        .border_style(theme.pane_border(focused));
    let mut state = ListState::default().with_selected(Some(pane.cursor));
    frame.render_stateful_widget(List::new(items).block(block), area, &mut state);
}

fn draw_queue(frame: &mut Frame, area: Rect, view: &FilesView, theme: &Theme) {
    let active = view
        .queue
        .jobs
        .iter()
        .filter(|job| matches!(job.state, TransferState::Queued | TransferState::Running))
        .count();
    let title = format!(
        " Transfers ─ {active} active ─ {} total ",
        view.queue.jobs.len()
    );
    if view.queue_collapsed || view.queue.jobs.is_empty() {
        frame.render_widget(
            Paragraph::new(if view.queue.jobs.is_empty() {
                "no transfers"
            } else {
                "t expand"
            })
            .style(theme.muted())
            .block(
                Block::bordered()
                    .title(Span::styled(title, theme.title()))
                    .border_style(theme.border()),
            ),
            area,
        );
        return;
    }
    let rows = Layout::vertical(
        std::iter::repeat_n(Constraint::Length(1), view.queue.jobs.len().min(4))
            .collect::<Vec<_>>(),
    )
    .split(
        Block::bordered()
            .title(Span::styled(title.clone(), theme.title()))
            .border_style(theme.border())
            .inner(area),
    );
    frame.render_widget(
        Block::bordered()
            .title(Span::styled(title, theme.title()))
            .border_style(theme.border()),
        area,
    );
    for (index, job) in view.queue.jobs.iter().rev().take(4).enumerate() {
        if let Some(row) = rows.get(index) {
            draw_job(frame, *row, job, theme);
        }
    }
}

fn draw_job(frame: &mut Frame, area: Rect, job: &TransferJob, theme: &Theme) {
    let arrow = match job.direction {
        TransferDirection::Upload => "↑",
        TransferDirection::Download => "↓",
    };
    let ratio = if job.total == 0 {
        0.0
    } else {
        job.done as f64 / job.total as f64
    };
    let label = format!(
        "{arrow} {} → {}  {}/{}  {:?}",
        file_name_only(&job.source),
        job.destination,
        format_size(job.done),
        format_size(job.total),
        job.state,
    );
    let style = match job.state {
        TransferState::Failed => theme.error(),
        TransferState::Done => theme.ok(),
        TransferState::Cancelled => theme.muted(),
        _ => theme.text(),
    };
    frame.render_widget(
        Gauge::default()
            .gauge_style(style)
            .ratio(ratio.clamp(0.0, 1.0))
            .label(label),
        area,
    );
}

fn draw_viewer(frame: &mut Frame, area: Rect, viewer: &Viewer, theme: &Theme) {
    let body = viewer
        .lines
        .iter()
        .skip(viewer.offset)
        .take(area.height.saturating_sub(2) as usize)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    draw_message(frame, area, &viewer.title, &body, theme);
}

fn draw_message(frame: &mut Frame, area: Rect, title: &str, body: &str, theme: &Theme) {
    if area.width < 8 || area.height < 4 {
        return;
    }
    let width = area.width.saturating_mul(80) / 100;
    let height = area.height.saturating_mul(70) / 100;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let rect = Rect {
        x,
        y,
        width: width.max(10),
        height: height.max(5),
    };
    // Clear first so the dual-pane list does not bleed through the popup.
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .style(theme.text())
            .block(
                Block::bordered()
                    .title(Span::styled(format!(" {title} "), theme.title()))
                    .title_bottom(Line::styled(
                        " Esc close  ↑↓ scroll ",
                        theme.muted(),
                    ))
                    .border_style(theme.border_focus())
                    .style(theme.bar()),
            ),
        rect,
    );
}

fn truncate(text: &str, width: usize) -> String {
    let mut out = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index >= width {
            break;
        }
        out.push(ch);
    }
    if text.chars().count() > width {
        if let Some((idx, _)) = out.char_indices().nth(width.saturating_sub(1)) {
            out.truncate(idx);
        }
        out.push('…');
    }
    out
}

fn file_name_only(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Parent path helper re-exported for the app.
pub fn go_parent(path: &str) -> String {
    parent_of(path).unwrap_or_else(|| "/".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_switches_and_space_marks() {
        let mut view = FilesView::new(PathBuf::from("/tmp"));
        view.local.entries = vec![DirEntry {
            name: "a.txt".into(),
            kind: EntryKind::File,
            size: 1,
            mtime: None,
            mode: None,
            target: None,
        }];
        assert_eq!(on_key(&mut view, KeyCode::Tab, &[]), FilesEvent::None);
        assert_eq!(view.focus, PaneFocus::Remote);
        view.focus = PaneFocus::Local;
        assert_eq!(on_key(&mut view, KeyCode::Char(' '), &[]), FilesEvent::None);
        assert!(view.local.marks.contains("a.txt"));
    }

    #[test]
    fn l_and_right_activate_left_goes_up() {
        let mut view = FilesView::new(PathBuf::from("/tmp"));
        assert_eq!(
            on_key(&mut view, KeyCode::Char('l'), &[]),
            FilesEvent::Activate
        );
        assert_eq!(on_key(&mut view, KeyCode::Right, &[]), FilesEvent::Activate);
        assert_eq!(on_key(&mut view, KeyCode::Left, &[]), FilesEvent::Parent);
    }

    #[test]
    fn c_and_f5_request_copy() {
        let mut view = FilesView::new(PathBuf::from("/tmp"));
        assert_eq!(on_key(&mut view, KeyCode::Char('c'), &[]), FilesEvent::Copy);
        assert_eq!(on_key(&mut view, KeyCode::F(5), &[]), FilesEvent::Copy);
    }
}
