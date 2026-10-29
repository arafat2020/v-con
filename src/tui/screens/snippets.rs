//! Snippet picker, variable form, and simple add/edit/delete management.

use crate::domain::snippet::{self, Snippet, list_variables};
use crate::tui::theme::Theme;
use crate::tui::widgets::form::{Field, Form, FormEffect};
use crossterm::event::{KeyCode, KeyModifiers};
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};
use std::collections::BTreeMap;

/// What the app should do after a key in the snippets overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnippetEvent {
    /// Keep the overlay open.
    Continue,
    /// Close without running.
    Cancel,
    /// Run this rendered command on the given optional selector override.
    Run {
        /// Snippet that was chosen.
        snippet: Snippet,
        /// Fully rendered remote command.
        command: String,
    },
    /// Persist a create/update.
    Upsert(Snippet),
    /// Delete by name.
    Delete(String),
}

/// Overlay mode.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Picker,
    Vars {
        snippet: Snippet,
        form: Form,
    },
    Edit {
        original: Option<String>,
        form: Form,
    },
    ConfirmDelete {
        name: String,
    },
}

/// Snippet picker / editor state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetsView {
    snippets: Vec<Snippet>,
    filter: String,
    cursor: usize,
    mode: Mode,
}

impl SnippetsView {
    /// Open the picker with the current store contents.
    pub fn new(snippets: Vec<Snippet>) -> Self {
        Self {
            snippets,
            filter: String::new(),
            cursor: 0,
            mode: Mode::Picker,
        }
    }

    /// Replace the list after a save/delete.
    pub fn set_snippets(&mut self, snippets: Vec<Snippet>) {
        self.snippets = snippets;
        let len = self.filtered().len();
        if self.cursor >= len && len > 0 {
            self.cursor = len - 1;
        }
    }

    /// Return to the fuzzy picker after an edit/delete.
    pub fn mode_picker(&mut self) {
        self.mode = Mode::Picker;
    }
}

/// Handle a key. Returns the event for the app.
pub fn on_key(view: &mut SnippetsView, code: KeyCode, modifiers: KeyModifiers) -> SnippetEvent {
    match &mut view.mode {
        Mode::Picker => picker_key(view, code, modifiers),
        Mode::Vars { snippet, form } => match form.on_key(code, modifiers) {
            FormEffect::Continue => SnippetEvent::Continue,
            FormEffect::Cancel => {
                view.mode = Mode::Picker;
                SnippetEvent::Continue
            }
            FormEffect::Submit => {
                let snippet = snippet.clone();
                let vars = form_vars(form, &snippet);
                match snippet::render(&snippet.command, &vars) {
                    Ok(command) => SnippetEvent::Run { snippet, command },
                    Err(missing) => {
                        if let Some(field) = form
                            .fields
                            .iter_mut()
                            .find(|field| missing.names.iter().any(|name| name == &field.id))
                        {
                            field.error = Some("required".into());
                        }
                        SnippetEvent::Continue
                    }
                }
            }
        },
        Mode::Edit { original, form } => match form.on_key(code, modifiers) {
            FormEffect::Continue => SnippetEvent::Continue,
            FormEffect::Cancel => {
                view.mode = Mode::Picker;
                SnippetEvent::Continue
            }
            FormEffect::Submit => match form_to_snippet(form) {
                Ok(snippet) => {
                    let _ = original;
                    SnippetEvent::Upsert(snippet)
                }
                Err(message) => {
                    if let Some(field) = form.fields.first_mut() {
                        field.error = Some(message);
                    }
                    SnippetEvent::Continue
                }
            },
        },
        Mode::ConfirmDelete { name } => match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => SnippetEvent::Delete(name.clone()),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                view.mode = Mode::Picker;
                SnippetEvent::Continue
            }
            _ => SnippetEvent::Continue,
        },
    }
}

fn picker_key(view: &mut SnippetsView, code: KeyCode, modifiers: KeyModifiers) -> SnippetEvent {
    match code {
        KeyCode::Esc => SnippetEvent::Cancel,
        KeyCode::Enter => {
            let Some(snippet) = view.filtered().get(view.cursor).cloned() else {
                return SnippetEvent::Continue;
            };
            let vars = list_variables(&snippet.command);
            if vars.is_empty() {
                match snippet::render(&snippet.command, &BTreeMap::new()) {
                    Ok(command) => SnippetEvent::Run { snippet, command },
                    Err(_) => SnippetEvent::Continue,
                }
            } else {
                view.mode = Mode::Vars {
                    form: vars_form(&snippet),
                    snippet,
                };
                SnippetEvent::Continue
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            view.cursor = view.cursor.saturating_sub(1);
            SnippetEvent::Continue
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let len = view.filtered().len();
            if len > 0 {
                view.cursor = (view.cursor + 1).min(len - 1);
            }
            SnippetEvent::Continue
        }
        KeyCode::Char('a') if modifiers.is_empty() => {
            view.mode = Mode::Edit {
                original: None,
                form: edit_form(None),
            };
            SnippetEvent::Continue
        }
        KeyCode::Char('e') if modifiers.is_empty() => {
            if let Some(snippet) = view.filtered().get(view.cursor).cloned() {
                view.mode = Mode::Edit {
                    original: Some(snippet.name.clone()),
                    form: edit_form(Some(&snippet)),
                };
            }
            SnippetEvent::Continue
        }
        KeyCode::Char('d') if modifiers.is_empty() => {
            if let Some(snippet) = view.filtered().get(view.cursor) {
                view.mode = Mode::ConfirmDelete {
                    name: snippet.name.clone(),
                };
            }
            SnippetEvent::Continue
        }
        KeyCode::Backspace => {
            view.filter.pop();
            view.cursor = 0;
            SnippetEvent::Continue
        }
        KeyCode::Char(ch) if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT => {
            view.filter.push(ch);
            view.cursor = 0;
            SnippetEvent::Continue
        }
        _ => SnippetEvent::Continue,
    }
}

impl SnippetsView {
    fn filtered(&self) -> Vec<Snippet> {
        if self.filter.trim().is_empty() {
            return self.snippets.clone();
        }
        let pattern = Pattern::parse(&self.filter, CaseMatching::Ignore, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buf = Vec::new();
        let mut scored = Vec::new();
        for snippet in &self.snippets {
            let hay = format!(
                "{} {} {}",
                snippet.name,
                snippet.description.as_deref().unwrap_or(""),
                snippet.command
            );
            if let Some(score) = pattern.score(Utf32Str::new(&hay, &mut buf), &mut matcher) {
                scored.push((score, snippet.clone()));
            }
        }
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        scored.into_iter().map(|(_, s)| s).collect()
    }
}

fn vars_form(snippet: &Snippet) -> Form {
    let mut fields = Vec::new();
    for var in list_variables(&snippet.command) {
        let mut field = Field::text(&var.name, &var.name);
        if let Some(default) = &var.default {
            field.value = default.clone();
            field.cursor = field.value.len();
        }
        fields.push(field);
    }
    fields.push(Field::button("run", "Run"));
    Form::new(fields)
}

fn edit_form(snippet: Option<&Snippet>) -> Form {
    let mut name = Field::text("name", "Name");
    let mut description = Field::text("description", "Description");
    let mut command = Field::multiline("command", "Command");
    let mut confirm = Field::select("confirm", "Confirm", vec!["no".into(), "yes".into()], "no");
    let mut selector = Field::text("selector", "Selector");
    if let Some(snippet) = snippet {
        name.value = snippet.name.clone();
        name.cursor = name.value.len();
        if let Some(desc) = &snippet.description {
            description.value = desc.clone();
            description.cursor = description.value.len();
        }
        command.value = snippet.command.clone();
        command.cursor = command.value.len();
        if snippet.confirm {
            confirm.value = "yes".into();
        }
        if let Some(sel) = &snippet.selector {
            selector.value = sel.clone();
            selector.cursor = selector.value.len();
        }
    }
    Form::new(vec![
        name,
        description,
        command,
        confirm,
        selector,
        Field::button("save", "Save"),
    ])
}

fn form_vars(form: &Form, snippet: &Snippet) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    for var in list_variables(&snippet.command) {
        if let Some(field) = form.fields.iter().find(|field| field.id == var.name) {
            vars.insert(var.name, field.value.clone());
        }
    }
    vars
}

fn form_to_snippet(form: &Form) -> Result<Snippet, String> {
    let name = field_value(form, "name").trim().to_string();
    if name.is_empty() {
        return Err("name is required".into());
    }
    let command = field_value(form, "command").trim().to_string();
    if command.is_empty() {
        return Err("command is required".into());
    }
    let description = blank(field_value(form, "description"));
    let confirm = field_value(form, "confirm") == "yes";
    let selector = blank(field_value(form, "selector"));
    Ok(Snippet {
        name,
        description,
        command,
        confirm,
        selector,
    })
}

fn field_value<'a>(form: &'a Form, id: &str) -> &'a str {
    form.fields
        .iter()
        .find(|field| field.id == id)
        .map(|field| field.value.as_str())
        .unwrap_or("")
}

fn blank(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Draw the snippets overlay.
pub fn draw(frame: &mut Frame, area: Rect, view: &SnippetsView, theme: &Theme) {
    if area.width < 10 || area.height < 6 {
        return;
    }
    let width = area.width.saturating_sub(4).clamp(40, 80);
    let height = area.height.saturating_sub(4).clamp(12, 24);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    match &view.mode {
        Mode::Picker => draw_picker(frame, popup, view, theme),
        Mode::Vars { snippet, form } => {
            let title = format!("Snippet: {}", snippet.name);
            let lines: Vec<Line<'static>> = form.lines().into_iter().map(Line::from).collect();
            crate::tui::widgets::modal::render(frame, area, &title, lines, theme);
        }
        Mode::Edit { form, .. } => {
            let lines: Vec<Line<'static>> = form.lines().into_iter().map(Line::from).collect();
            crate::tui::widgets::modal::render(frame, area, "Edit snippet", lines, theme);
        }
        Mode::ConfirmDelete { name } => {
            let block = Block::bordered()
                .title(Span::styled(" Delete snippet ", theme.title()))
                .border_style(theme.border_focus())
                .style(theme.bar());
            frame.render_widget(
                Paragraph::new(format!("Delete `{name}`? [y/N]"))
                    .style(theme.text())
                    .block(block)
                    .wrap(Wrap { trim: true }),
                popup,
            );
        }
    }
}

fn draw_picker(frame: &mut Frame, area: Rect, view: &SnippetsView, theme: &Theme) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(area);
    let filter = if view.filter.is_empty() {
        " / filter   ⏎ run   a add   e edit   d del   Esc close".to_string()
    } else {
        format!(" /{} ", view.filter)
    };
    frame.render_widget(Paragraph::new(filter).style(theme.muted()), chunks[0]);
    let items: Vec<ListItem> = view
        .filtered()
        .into_iter()
        .map(|snippet| {
            let desc = snippet.description.as_deref().unwrap_or("");
            let line = if desc.is_empty() {
                Line::from(vec![
                    Span::styled(format!("{:<12}", snippet.name), theme.title()),
                    Span::styled(format!(" {}", snippet.command), theme.text()),
                ])
            } else {
                Line::from(vec![
                    Span::styled(format!("{:<12}", snippet.name), theme.title()),
                    Span::styled(
                        format!(" {desc}  ·  {}", snippet.command),
                        theme.muted(),
                    ),
                ])
            };
            ListItem::new(line)
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(view.cursor));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::bordered()
                    .title(Span::styled(" Snippets ", theme.title()))
                    .border_style(theme.border()),
            )
            .highlight_style(theme.selected()),
        chunks[1],
        &mut state,
    );
    frame.render_widget(
        Paragraph::new("Enter runs · variables prompted when needed").style(theme.muted()),
        chunks[2],
    );
}
