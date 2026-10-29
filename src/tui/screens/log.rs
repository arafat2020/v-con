//! Audit log browser (screen 5).

use crate::infra::audit_log::AuditEntry;
use crate::tui::theme::Theme;
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::widgets::{Block, Clear, Paragraph, Row, Table, TableState, Wrap};

/// Key result for the log screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEvent {
    /// Nothing to do.
    None,
    /// Re-run an exec/snippet entry (command + hosts).
    Rerun {
        /// Hosts from the entry.
        hosts: Vec<String>,
        /// Command text (may include `[snippet] ` prefix).
        command: String,
    },
}

/// Log screen state.
#[derive(Debug, Clone, Default)]
pub struct LogView {
    /// Loaded entries (oldest → newest).
    pub entries: Vec<AuditEntry>,
    /// Filtered indexes into `entries`.
    pub visible: Vec<usize>,
    pub cursor: usize,
    pub filter_host: String,
    pub filter_action: String,
    pub failed_only: bool,
    pub search: String,
    pub searching: bool,
    pub detail: Option<usize>,
    pub offset: usize,
}

impl LogView {
    /// Replace entries and re-apply filters.
    pub fn load(&mut self, entries: Vec<AuditEntry>) {
        self.entries = entries;
        self.apply_filters();
        if self.cursor >= self.visible.len() && !self.visible.is_empty() {
            self.cursor = self.visible.len() - 1;
        }
    }

    /// Recompute `visible` from filters and search.
    pub fn apply_filters(&mut self) {
        self.visible = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| self.matches(entry))
            .map(|(index, _)| index)
            .collect();
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
    }

    fn matches(&self, entry: &AuditEntry) -> bool {
        if !self.filter_host.is_empty()
            && !entry
                .hosts
                .iter()
                .any(|host| host.contains(&self.filter_host))
        {
            return false;
        }
        if !self.filter_action.is_empty() && !entry.action.contains(&self.filter_action) {
            return false;
        }
        if self.failed_only {
            let failed = entry
                .results
                .as_ref()
                .is_some_and(|results| results.values().any(|code| *code != 0));
            if !failed {
                return false;
            }
        }
        if !self.search.is_empty() {
            let hay = format!(
                "{} {} {} {}",
                entry.action,
                entry.hosts.join(" "),
                entry.command.as_deref().unwrap_or(""),
                entry
                    .results
                    .as_ref()
                    .map(|results| format!("{results:?}"))
                    .unwrap_or_default()
            );
            if !hay
                .to_ascii_lowercase()
                .contains(&self.search.to_ascii_lowercase())
            {
                return false;
            }
        }
        true
    }

    fn selected_entry(&self) -> Option<&AuditEntry> {
        self.visible
            .get(self.cursor)
            .and_then(|index| self.entries.get(*index))
    }
}

/// Handle a key on the log screen.
pub fn on_key(view: &mut LogView, code: KeyCode) -> LogEvent {
    if view.detail.is_some() {
        if matches!(code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
            view.detail = None;
        }
        return LogEvent::None;
    }
    if view.searching {
        match code {
            KeyCode::Esc => {
                view.searching = false;
                view.search.clear();
                view.apply_filters();
            }
            KeyCode::Enter => view.searching = false,
            KeyCode::Backspace => {
                view.search.pop();
                view.apply_filters();
            }
            KeyCode::Char(ch) => {
                view.search.push(ch);
                view.apply_filters();
            }
            _ => {}
        }
        return LogEvent::None;
    }
    match code {
        KeyCode::Up | KeyCode::Char('k') => {
            view.cursor = view.cursor.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if !view.visible.is_empty() {
                view.cursor = (view.cursor + 1).min(view.visible.len() - 1);
            }
        }
        KeyCode::Char('/') => {
            view.searching = true;
        }
        KeyCode::Char('f') => {
            view.failed_only = !view.failed_only;
            view.apply_filters();
        }
        KeyCode::Enter => {
            if let Some(index) = view.visible.get(view.cursor).copied() {
                view.detail = Some(index);
            }
        }
        KeyCode::Char('R') => {
            if let Some(entry) = view.selected_entry()
                && matches!(entry.action.as_str(), "exec" | "snippet")
                && let Some(command) = entry.command.clone()
            {
                let command = command
                    .strip_prefix('[')
                    .and_then(|rest| rest.find(']').map(|i| rest[i + 1..].trim().to_string()))
                    .unwrap_or(command);
                return LogEvent::Rerun {
                    hosts: entry.hosts.clone(),
                    command,
                };
            }
        }
        KeyCode::Char('r') => {
            // refresh is handled by the app
        }
        _ => {}
    }
    LogEvent::None
}

/// Draw the log table.
pub fn draw(frame: &mut Frame, area: Rect, view: &mut LogView, theme: &Theme) {
    let title = {
        let mut parts = vec![format!("Log ({} entries)", view.visible.len())];
        if view.failed_only {
            parts.push("failed".into());
        }
        if !view.search.is_empty() || view.searching {
            parts.push(format!("/{}", view.search));
        }
        format!(" {} ", parts.join(" · "))
    };
    let header =
        Row::new(vec!["TIME", "ACTION", "HOSTS", "COMMAND", "RESULT"]).style(theme.muted());
    let rows = view.visible.iter().map(|index| {
        let entry = &view.entries[*index];
        let time = entry.ts.format("%m-%d %H:%M").to_string();
        let hosts = entry.hosts.join(",");
        let command = entry.command.as_deref().unwrap_or("-");
        let result = entry
            .results
            .as_ref()
            .map(|results| {
                if results.values().all(|code| *code == 0) {
                    "ok".into()
                } else {
                    "fail".into()
                }
            })
            .unwrap_or_else(|| "-".into());
        Row::new(vec![
            time,
            entry.action.clone(),
            hosts,
            truncate(command, 40),
            result,
        ])
    });
    let widths = [
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Length(16),
        Constraint::Min(12),
        Constraint::Length(6),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::bordered().title(title))
        .row_highlight_style(theme.selected());
    let mut state = TableState::default()
        .with_selected(Some(view.cursor))
        .with_offset(view.offset);
    frame.render_stateful_widget(table, area, &mut state);
    view.offset = state.offset();

    if let Some(index) = view.detail {
        draw_detail(frame, area, &view.entries[index], theme);
    }
}

fn draw_detail(frame: &mut Frame, area: Rect, entry: &AuditEntry, theme: &Theme) {
    let width = area.width.saturating_sub(6).clamp(40, 80);
    let height = area.height.saturating_sub(6).clamp(8, 18);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let results = entry
        .results
        .as_ref()
        .map(|results| {
            results
                .iter()
                .map(|(h, c)| format!("{h}={c}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "-".into());
    let text = format!(
        "time: {}\naction: {}\nhosts: {}\nselector: {}\ncommand: {}\nduration_ms: {}\n\nresults:\n{}\n\nEsc closes",
        entry.ts,
        entry.action,
        entry.hosts.join(", "),
        entry.selector.as_deref().unwrap_or("-"),
        entry.command.as_deref().unwrap_or("-"),
        entry
            .duration_ms
            .map(|ms| ms.to_string())
            .unwrap_or_else(|| "-".into()),
        results
    );
    frame.render_widget(
        Paragraph::new(text)
            .block(
                Block::bordered()
                    .title(" Audit detail ")
                    .border_style(theme.selected()),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let trimmed: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{trimmed}…")
    }
}
