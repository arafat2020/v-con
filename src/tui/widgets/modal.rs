//! Centered popup shared by the help text and the validation-problem list.

use crate::tui::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

/// Draw a bordered popup in the middle of `area`.
///
/// The popup is cleared first so the host table does not show through it.
/// Very small terminals skip the popup rather than drawing a broken box.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: Vec<Line<'static>>,
    theme: &Theme,
) {
    if area.width < 8 || area.height < 4 {
        return;
    }
    let width = area.width.saturating_sub(4).clamp(20, 72);
    let height = area.height.saturating_sub(4).clamp(6, 18);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    if !theme.no_color {
        let shadow = Rect {
            x: popup.x.saturating_add(1),
            y: popup.y.saturating_add(1),
            width: popup
                .width
                .min(area.right().saturating_sub(popup.x.saturating_add(1))),
            height: popup
                .height
                .min(area.bottom().saturating_sub(popup.y.saturating_add(1))),
        };
        if shadow.width > 0 && shadow.height > 0 {
            frame.render_widget(
                Block::default().style(Style::new().bg(Color::Rgb(8, 10, 14))),
                shadow,
            );
        }
    }
    frame.render_widget(Clear, popup);
    let block = theme
        .block()
        .title(Span::styled(format!(" {title} "), theme.title()))
        .border_style(theme.border_focus())
        .style(theme.bar());
    let paragraph = Paragraph::new(lines)
        .style(theme.text())
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}
