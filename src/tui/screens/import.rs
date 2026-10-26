//! Preview hosts found in `~/.ssh/config` and choose which ones to import.
//!
//! Existing names start unchecked. Checking one means that host is overwritten.
//! Wildcard patterns are shown and cannot be selected.

use crate::domain::host::{Host, Inventory};
use crate::infra::ssh_config::{self, ConfigEntry};
use crate::tui::theme::Theme;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use std::collections::HashSet;
use std::path::Path;

/// One row in the import checklist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRow {
    /// `Host` pattern from the file.
    pub pattern: String,
    /// Address column. Empty for a skipped pattern.
    pub address: String,
    /// User column.
    pub user: String,
    /// `new`, `exists`, or `skipped (wildcard)`.
    pub status: String,
    /// Whether Enter will import this row.
    pub checked: bool,
    /// Host to write. `None` when the row cannot be imported.
    pub host: Option<Host>,
}

/// The import modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportView {
    /// File the rows came from.
    pub source: String,
    /// Checklist, including skipped patterns.
    pub rows: Vec<ImportRow>,
    /// Highlighted row.
    pub cursor: usize,
    /// Include warnings and other notes.
    pub warnings: Vec<String>,
}

/// What the import screen wants the shell to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportEvent {
    /// Stay on the screen.
    Continue,
    /// Close without writing.
    Cancel,
    /// Write these hosts. Checked existing hosts are included so they overwrite.
    Import(Vec<Host>),
}

/// Read `path` and mark hosts that are already in `inventory`.
pub fn load(path: &Path, inventory: &Inventory) -> ImportView {
    let parsed = ssh_config::load(path);
    let existing: HashSet<&str> = inventory
        .hosts
        .iter()
        .map(|host| host.name.as_str())
        .collect();
    let rows = parsed
        .entries
        .into_iter()
        .map(|entry| row_from(entry, &existing))
        .collect();
    ImportView {
        source: path.display().to_string(),
        rows,
        cursor: 0,
        warnings: parsed.warnings,
    }
}

/// Apply a key. `a` toggles every row that can be imported.
pub fn on_key(view: &mut ImportView, code: KeyCode, _modifiers: KeyModifiers) -> ImportEvent {
    match code {
        KeyCode::Esc | KeyCode::Char('q') => ImportEvent::Cancel,
        KeyCode::Up | KeyCode::Char('k') => {
            view.cursor = view.cursor.saturating_sub(1);
            ImportEvent::Continue
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if view.cursor + 1 < view.rows.len() {
                view.cursor += 1;
            }
            ImportEvent::Continue
        }
        KeyCode::Char(' ') => {
            if let Some(row) = view.rows.get_mut(view.cursor)
                && row.host.is_some()
            {
                row.checked = !row.checked;
            }
            ImportEvent::Continue
        }
        KeyCode::Char('a') => {
            let check = view
                .rows
                .iter()
                .any(|row| row.host.is_some() && !row.checked);
            for row in &mut view.rows {
                if row.host.is_some() {
                    row.checked = check;
                }
            }
            ImportEvent::Continue
        }
        KeyCode::Enter => {
            let hosts = view
                .rows
                .iter()
                .filter(|row| row.checked)
                .filter_map(|row| row.host.clone())
                .collect();
            ImportEvent::Import(hosts)
        }
        _ => ImportEvent::Continue,
    }
}

/// Draw the checklist.
pub fn draw(frame: &mut Frame, area: Rect, view: &ImportView, theme: &Theme) {
    if area.width < 8 || area.height < 4 {
        return;
    }
    let width = area.width.saturating_sub(4).clamp(24, 76);
    let height = area.height.saturating_sub(2).clamp(8, 22);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let mut lines = vec![view.source.clone(), String::new()];
    if view.rows.is_empty() {
        lines.push("No hosts found.".to_string());
    }
    for (index, row) in view.rows.iter().enumerate() {
        let marker = if index == view.cursor { ">" } else { " " };
        let mark = if row.host.is_none() {
            "-"
        } else if row.checked {
            "x"
        } else {
            " "
        };
        lines.push(format!(
            "{marker}[{mark}] {:<16} {:<16} {:<10} {}",
            row.pattern, row.address, row.user, row.status
        ));
    }
    if let Some(warning) = view.warnings.first() {
        lines.push(warning.clone());
    }
    lines.push("space toggle  a all  Enter import  Esc cancel".to_string());
    let paragraph = Paragraph::new(lines.join("\n"))
        .block(
            Block::bordered()
                .title(" Import ssh config ")
                .border_style(theme.selected()),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

fn row_from(entry: ConfigEntry, existing: &HashSet<&str>) -> ImportRow {
    if let Some(reason) = entry.skip_reason {
        return ImportRow {
            pattern: entry.pattern,
            address: String::new(),
            user: String::new(),
            status: reason,
            checked: false,
            host: None,
        };
    }
    let host = entry.host.expect("a concrete entry has a host");
    let exists = existing.contains(host.name.as_str());
    ImportRow {
        pattern: host.name.clone(),
        address: host.address.clone(),
        user: host.user.clone().unwrap_or_default(),
        status: if exists {
            "exists".to_string()
        } else {
            "new".to_string()
        },
        checked: !exists,
        host: Some(host),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_hosts_start_checked_and_existing_hosts_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(
            &path,
            "Host web-1\n  HostName 203.0.113.10\nHost db-1\n  HostName 10.0.0.20\nHost *\n  User root\n",
        )
        .unwrap();
        let inventory = toml_edit::de::from_str::<Inventory>(
            "[[hosts]]\nname = \"web-1\"\naddress = \"203.0.113.10\"\n",
        )
        .unwrap();
        let mut view = load(&path, &inventory);
        let web = view.rows.iter().find(|row| row.pattern == "web-1").unwrap();
        assert_eq!(web.status, "exists");
        assert!(!web.checked);
        let db = view.rows.iter().find(|row| row.pattern == "db-1").unwrap();
        assert!(db.checked);
        let hosts = match on_key(&mut view, KeyCode::Enter, KeyModifiers::NONE) {
            ImportEvent::Import(hosts) => hosts,
            other => panic!("expected import, got {other:?}"),
        };
        assert!(hosts.iter().any(|host| host.name == "db-1"));
        assert!(hosts.iter().all(|host| host.name != "web-1"));
        on_key(&mut view, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(view.rows[0].checked);
    }
}
