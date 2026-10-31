//! Palette used across the TUI.
//!
//! Defaults favour a soft dark slate with teal accents — easy on the eyes in
//! long sessions, without loud neon. `NO_COLOR` / `--no-color` keeps the same
//! layout using bold, reverse, and underline instead of colour.

use crate::config::ThemeConfig;
use crate::error::Error;
use ratatui::style::{Color, Modifier, Style};

/// Concrete colours for one run of the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// Cursor and selection accent.
    pub highlight: Color,
    /// Healthy status.
    pub ok: Color,
    /// Warnings and the validation badge.
    pub warn: Color,
    /// Errors.
    pub crit: Color,
    /// Secondary text (hints, metadata).
    pub muted: Color,
    /// Primary body text.
    pub text: Color,
    /// Titles and brand-ish labels.
    pub title: Color,
    /// Idle borders.
    pub border: Color,
    /// Focused pane / active control borders.
    pub border_focus: Color,
    /// Header and footer bar background.
    pub panel: Color,
    /// Directory names in file lists.
    pub directory: Color,
    /// Symlink names in file lists.
    pub symlink: Color,
    /// When true, styles use bold/reverse/underline instead of colours.
    pub no_color: bool,
}

impl Theme {
    /// Resolve colour names. An unknown name is an error so a typo is not silently gray.
    pub fn from_config(config: &ThemeConfig, no_color: bool) -> Result<Self, Error> {
        let highlight = parse_color(&config.highlight)?;
        let ok = parse_color(&config.ok)?;
        let warn = parse_color(&config.warn)?;
        let crit = parse_color(&config.crit)?;
        Ok(Self {
            highlight,
            ok,
            warn,
            crit,
            // Soft slate chrome — independent of the four semantic accents so
            // a custom highlight does not make the whole UI harsh.
            muted: Color::Rgb(130, 140, 155),
            text: Color::Rgb(220, 226, 234),
            title: Color::Rgb(140, 210, 220),
            border: Color::Rgb(55, 62, 75),
            border_focus: highlight,
            panel: Color::Rgb(24, 28, 36),
            directory: Color::Rgb(120, 170, 230),
            symlink: Color::Rgb(180, 150, 220),
            no_color,
        })
    }

    /// Monochrome theme used by snapshot tests.
    #[cfg(test)]
    pub fn plain() -> Self {
        Self {
            highlight: Color::White,
            ok: Color::White,
            warn: Color::White,
            crit: Color::White,
            muted: Color::White,
            text: Color::White,
            title: Color::White,
            border: Color::White,
            border_focus: Color::White,
            panel: Color::Reset,
            directory: Color::White,
            symlink: Color::White,
            no_color: true,
        }
    }

    /// Cursor / selected row.
    pub fn selected(&self) -> Style {
        if self.no_color {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
                .fg(Color::Rgb(18, 22, 28))
                .bg(self.highlight)
                .add_modifier(Modifier::BOLD)
        }
    }

    /// Top tab bar and bottom hint bar background.
    pub fn bar(&self) -> Style {
        if self.no_color {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().fg(self.text).bg(self.panel)
        }
    }

    /// Active tab label on the header bar.
    pub fn tab_active(&self) -> Style {
        if self.no_color {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::new()
                .fg(Color::Rgb(18, 22, 28))
                .bg(self.highlight)
                .add_modifier(Modifier::BOLD)
        }
    }

    /// Inactive tab label on the header bar.
    pub fn tab_inactive(&self) -> Style {
        if self.no_color {
            Style::new()
        } else {
            Style::new().fg(self.muted).bg(self.panel)
        }
    }

    /// Panel / section title text.
    pub fn title(&self) -> Style {
        self.paint(self.title, Modifier::BOLD)
    }

    /// Idle border.
    pub fn border(&self) -> Style {
        self.paint(self.border, Modifier::DIM)
    }

    /// Focused pane border.
    pub fn border_focus(&self) -> Style {
        if self.no_color {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(self.border_focus)
        }
    }

    /// Border style for a pane that may or may not be focused.
    pub fn pane_border(&self, focused: bool) -> Style {
        if focused {
            self.border_focus()
        } else {
            self.border()
        }
    }

    /// Primary readable text.
    pub fn text(&self) -> Style {
        self.paint(self.text, Modifier::empty())
    }

    /// Directory entry in a file list.
    pub fn directory(&self) -> Style {
        if self.no_color {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(self.directory).add_modifier(Modifier::BOLD)
        }
    }

    /// Symlink entry in a file list.
    pub fn symlink(&self) -> Style {
        self.paint(self.symlink, Modifier::ITALIC)
    }

    /// Table / list header row.
    pub fn header_row(&self) -> Style {
        if self.no_color {
            Style::new().add_modifier(Modifier::UNDERLINED | Modifier::BOLD)
        } else {
            Style::new()
                .fg(self.muted)
                .bg(self.panel)
                .add_modifier(Modifier::BOLD)
        }
    }

    pub fn warning(&self) -> Style {
        self.paint(self.warn, Modifier::BOLD)
    }

    /// Healthy status, such as a finished exec row.
    pub fn ok(&self) -> Style {
        self.paint(self.ok, Modifier::BOLD)
    }

    pub fn error(&self) -> Style {
        self.paint(self.crit, Modifier::BOLD)
    }

    pub fn muted(&self) -> Style {
        self.paint(self.muted, Modifier::DIM)
    }

    pub fn match_style(&self) -> Style {
        if self.no_color {
            Style::new().add_modifier(Modifier::UNDERLINED | Modifier::BOLD)
        } else {
            Style::new().fg(self.highlight).add_modifier(Modifier::BOLD)
        }
    }

    fn paint(&self, color: Color, fallback: Modifier) -> Style {
        if self.no_color {
            if fallback == Modifier::empty() {
                Style::new()
            } else {
                Style::new().add_modifier(fallback)
            }
        } else {
            Style::new().fg(color)
        }
    }
}

/// Parse a theme colour name. `gray` and `grey` are the same.
pub fn parse_color(name: &str) -> Result<Color, Error> {
    let color = match name.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" | "dark_gray" => Color::DarkGray,
        "white" => Color::White,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        _ => return Err(Error::Message(format!("unknown theme color '{name}'"))),
    };
    Ok(color)
}
