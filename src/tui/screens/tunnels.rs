//! Per-host tunnel panel: saved forwards, start/stop, ad-hoc input.

use crate::domain::host::{EffectiveHost, HostTunnel};
use crate::domain::tunnel::TunnelSpec;
use crate::services::tunnels::{RunningTunnel, TunnelId};
use crate::tui::theme::Theme;
use crate::tui::widgets::input::{backspace, insert};
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

/// Result of a key in the tunnels panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelUiEvent {
    /// Keep open.
    Continue,
    /// Close the panel.
    Close,
    /// Start a saved or ad-hoc tunnel.
    Start {
        /// Display label.
        label: String,
        /// Parsed spec.
        spec: TunnelSpec,
    },
    /// Stop a running tunnel.
    Stop(TunnelId),
}

/// Tunnels modal for one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelsView {
    /// Host this panel is for.
    pub host: String,
    /// Saved tunnels from inventory.
    pub saved: Vec<HostTunnel>,
    /// Cursor into the combined list (saved rows, then ad-hoc input).
    pub cursor: usize,
    /// Ad-hoc spec being typed.
    pub adhoc: String,
    /// Cursor inside `adhoc`.
    pub adhoc_cursor: usize,
    /// Last parse / start error.
    pub error: Option<String>,
}

impl TunnelsView {
    /// Open for `host`.
    pub fn new(host: &EffectiveHost) -> Self {
        Self {
            host: host.name.clone(),
            saved: host.tunnels.clone(),
            cursor: 0,
            adhoc: String::new(),
            adhoc_cursor: 0,
            error: None,
        }
    }

    fn row_count(&self) -> usize {
        self.saved.len() + 1
    }
}

/// Handle a key given the currently running tunnels for this host.
pub fn on_key(view: &mut TunnelsView, code: KeyCode, running: &[RunningTunnel]) -> TunnelUiEvent {
    match code {
        KeyCode::Esc => TunnelUiEvent::Close,
        KeyCode::Up | KeyCode::Char('k') => {
            view.cursor = view.cursor.saturating_sub(1);
            TunnelUiEvent::Continue
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let max = view.row_count().saturating_sub(1);
            view.cursor = (view.cursor + 1).min(max);
            TunnelUiEvent::Continue
        }
        KeyCode::Enter | KeyCode::Char(' ') => toggle_row(view, running),
        KeyCode::Backspace if view.cursor == view.saved.len() => {
            backspace(&mut view.adhoc, &mut view.adhoc_cursor);
            view.error = None;
            TunnelUiEvent::Continue
        }
        KeyCode::Char(ch) if view.cursor == view.saved.len() && !ch.is_control() => {
            insert(&mut view.adhoc, &mut view.adhoc_cursor, ch);
            view.error = None;
            TunnelUiEvent::Continue
        }
        _ => TunnelUiEvent::Continue,
    }
}

fn toggle_row(view: &mut TunnelsView, running: &[RunningTunnel]) -> TunnelUiEvent {
    if view.cursor < view.saved.len() {
        let saved = &view.saved[view.cursor];
        if let Some(active) = running.iter().find(|tunnel| tunnel.label == saved.name) {
            return TunnelUiEvent::Stop(active.id);
        }
        match TunnelSpec::parse(&saved.spec) {
            Ok(spec) => TunnelUiEvent::Start {
                label: saved.name.clone(),
                spec,
            },
            Err(error) => {
                view.error = Some(error.message);
                TunnelUiEvent::Continue
            }
        }
    } else {
        let raw = view.adhoc.trim();
        if raw.is_empty() {
            view.error = Some("type a tunnel spec, e.g. L:3000:localhost:3000".into());
            return TunnelUiEvent::Continue;
        }
        match TunnelSpec::parse(raw) {
            Ok(spec) => TunnelUiEvent::Start {
                label: spec.to_compact(),
                spec,
            },
            Err(error) => {
                view.error = Some(error.message);
                TunnelUiEvent::Continue
            }
        }
    }
}

/// Draw the tunnels panel.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    view: &TunnelsView,
    running: &[RunningTunnel],
    theme: &Theme,
) {
    if area.width < 10 || area.height < 6 {
        return;
    }
    let width = area.width.saturating_sub(4).clamp(40, 72);
    let height = area.height.saturating_sub(4).clamp(10, 20);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).split(popup);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (index, saved) in view.saved.iter().enumerate() {
        let active = running.iter().any(|tunnel| tunnel.label == saved.name);
        let marker = if index == view.cursor { ">" } else { " " };
        let state = if active { "ON " } else { "off" };
        lines.push(Line::from(format!(
            "{marker} [{state}] {:<12} {}",
            saved.name, saved.spec
        )));
    }
    let adhoc_index = view.saved.len();
    let marker = if view.cursor == adhoc_index { ">" } else { " " };
    lines.push(Line::from(format!(
        "{marker} ad-hoc: {}_",
        &view.adhoc[..view.adhoc_cursor.min(view.adhoc.len())]
    )));
    if view.saved.is_empty() {
        lines.insert(
            0,
            Line::from(Span::styled(
                "(no saved tunnels — type an ad-hoc spec)",
                theme.muted(),
            )),
        );
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.text())
            .block(
                Block::bordered()
                    .title(Span::styled(
                        format!(" Tunnels — {} ", view.host),
                        theme.title(),
                    ))
                    .border_style(theme.border_focus())
                    .style(theme.bar()),
            )
            .wrap(Wrap { trim: false }),
        chunks[0],
    );
    let footer = view
        .error
        .clone()
        .unwrap_or_else(|| "⏎/space start·stop   Esc close".into());
    let style = if view.error.is_some() {
        theme.error()
    } else {
        theme.muted()
    };
    frame.render_widget(Paragraph::new(footer).style(style), chunks[1]);
}
