//! Palette used by the Hosts screen.
//!
//! `NO_COLOR` and `--no-color` set [`Theme::no_color`]. Status styles then use
//! bold, reverse, or underline so the screen stays readable without colour.

use crate::config::ThemeConfig;
use crate::error::Error;
use ratatui::style::{Color, Modifier, Style};

/// Concrete colours for one run of the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// Cursor and selection.
    pub highlight: Color,
    /// Healthy status.
    pub ok: Color,
    /// Warnings and the validation badge.
    pub warn: Color,
    /// Errors.
    pub crit: Color,
    /// Secondary text, such as the hint line.
    pub muted: Color,
    /// When true, status styles use bold and reverse instead of these colours.
    pub no_color: bool,
}

impl Theme {
    /// Resolve colour names. An unknown name is an error so a typo is not silently gray.
    pub fn from_config(config: &ThemeConfig, no_color: bool) -> Result<Self, Error> {
        Ok(Self {
            highlight: parse_color(&config.highlight)?,
            ok: parse_color(&config.ok)?,
            warn: parse_color(&config.warn)?,
            crit: parse_color(&config.crit)?,
            muted: Color::Gray,
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
            no_color: true,
        }
    }

    pub fn selected(&self) -> Style {
        if self.no_color {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().fg(Color::Black).bg(self.highlight)
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
            Style::new().add_modifier(fallback)
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
        "white" => Color::White,
        _ => return Err(Error::Message(format!("unknown theme color '{name}'"))),
    };
    Ok(color)
}
