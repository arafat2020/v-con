//! A small form: text, number, select, tags, notes, and a button.
//!
//! The form owns focus and field values. It does not know what a host is.
//! `q` typed into a field is just the letter q. Esc cancels, and a second Esc
//! discards unsaved edits.

use crate::tui::widgets::input::{backspace, insert, move_left, move_right};
use crossterm::event::{KeyCode, KeyModifiers};

/// What kind of value a row edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    /// One line of text.
    Text,
    /// Digits only. Empty is allowed.
    Number,
    /// One of `options`. Left and right change the choice.
    Select { options: Vec<String> },
    /// Chips plus the word currently being typed.
    Tags { suggestions: Vec<String> },
    /// Several lines. Enter inserts a newline.
    Multiline,
    /// The row that submits the form.
    Button,
}

/// One labelled row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Stable id used by the screen that owns the form.
    pub id: String,
    /// Label drawn beside the value.
    pub label: String,
    /// Current text. For tags this is the unfinished chip.
    pub value: String,
    /// Tag chips already added.
    pub chips: Vec<String>,
    /// Character cursor inside `value`.
    pub cursor: usize,
    /// Shown when `value` and `chips` are empty.
    pub placeholder: String,
    /// Blocks submit when set.
    pub error: Option<String>,
    /// Shown, but does not block submit.
    pub warning: Option<String>,
    /// How this row handles keys.
    pub kind: FieldKind,
}

impl Field {
    /// A text row.
    pub fn text(id: &str, label: &str) -> Self {
        Self::new(id, label, FieldKind::Text)
    }

    /// A digits-only row.
    pub fn number(id: &str, label: &str) -> Self {
        Self::new(id, label, FieldKind::Number)
    }

    /// A choice row. `selected` is stored in `value`.
    pub fn select(id: &str, label: &str, options: Vec<String>, selected: &str) -> Self {
        let mut field = Self::new(id, label, FieldKind::Select { options });
        field.value = selected.to_string();
        field
    }

    /// A tag row.
    pub fn tags(id: &str, label: &str, chips: Vec<String>) -> Self {
        let mut field = Self::new(
            id,
            label,
            FieldKind::Tags {
                suggestions: Vec::new(),
            },
        );
        field.chips = chips;
        field
    }

    /// A notes row.
    pub fn multiline(id: &str, label: &str) -> Self {
        Self::new(id, label, FieldKind::Multiline)
    }

    /// The save row.
    pub fn button(id: &str, label: &str) -> Self {
        Self::new(id, label, FieldKind::Button)
    }

    fn new(id: &str, label: &str, kind: FieldKind) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            value: String::new(),
            chips: Vec::new(),
            cursor: 0,
            placeholder: String::new(),
            error: None,
            warning: None,
            kind,
        }
    }
}

/// What the screen should do after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormEffect {
    /// Stay on the form.
    Continue,
    /// The user confirmed they want to leave.
    Cancel,
    /// Ctrl+S, or Enter on the button.
    Submit,
}

/// Focus, values, and the discard confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    /// Rows from top to bottom.
    pub fields: Vec<Field>,
    /// Index of the focused row.
    pub focus: usize,
    /// True after the user changes a value.
    pub dirty: bool,
    /// True after Esc on a dirty form. A second Esc cancels.
    pub confirm_discard: bool,
}

impl Form {
    /// Start on the first row.
    pub fn new(fields: Vec<Field>) -> Self {
        Self {
            fields,
            focus: 0,
            dirty: false,
            confirm_discard: false,
        }
    }

    /// Mutable access to a row by id.
    pub fn field_mut(&mut self, id: &str) -> Option<&mut Field> {
        self.fields.iter_mut().find(|field| field.id == id)
    }

    /// Borrow a row by id.
    pub fn field(&self, id: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.id == id)
    }

    /// True when any row has an error.
    pub fn has_errors(&self) -> bool {
        self.fields.iter().any(|field| field.error.is_some())
    }

    /// Handle one key. Printable characters never leave the form.
    pub fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> FormEffect {
        if self.confirm_discard {
            self.confirm_discard = false;
            if code == KeyCode::Esc {
                return FormEffect::Cancel;
            }
            return FormEffect::Continue;
        }
        if code == KeyCode::Esc {
            if self.dirty {
                self.confirm_discard = true;
                return FormEffect::Continue;
            }
            return FormEffect::Cancel;
        }
        // Caps Lock can deliver `S` with CONTROL; accept both.
        if matches!(code, KeyCode::Char('s' | 'S')) && modifiers.contains(KeyModifiers::CONTROL) {
            return FormEffect::Submit;
        }
        match code {
            KeyCode::Tab => self.move_focus(1),
            KeyCode::BackTab => self.move_focus(-1),
            KeyCode::Up => self.move_focus(-1),
            KeyCode::Down => self.move_focus(1),
            KeyCode::Enter => return self.on_enter(),
            other => self.edit(other, modifiers),
        }
        FormEffect::Continue
    }

    /// Lines the popup draws, including the discard prompt.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (index, field) in self.fields.iter().enumerate() {
            let marker = if index == self.focus { ">" } else { " " };
            lines.push(format!("{marker} {}", render_field(field)));
            if let Some(error) = &field.error {
                lines.push(format!("  {error}"));
            } else if let Some(warning) = &field.warning {
                lines.push(format!("  {warning}"));
            }
        }
        if self.confirm_discard {
            lines.push("Discard unsaved changes? Esc again to discard.".to_string());
        }
        lines
    }

    fn on_enter(&mut self) -> FormEffect {
        let Some(kind) = self.fields.get(self.focus).map(|field| field.kind.clone()) else {
            return FormEffect::Continue;
        };
        match kind {
            FieldKind::Button => FormEffect::Submit,
            FieldKind::Tags { .. } => {
                self.add_chip();
                FormEffect::Continue
            }
            FieldKind::Multiline => {
                self.insert_char('\n');
                FormEffect::Continue
            }
            _ => {
                self.move_focus(1);
                FormEffect::Continue
            }
        }
    }

    fn edit(&mut self, code: KeyCode, _modifiers: KeyModifiers) {
        let Some(kind) = self.fields.get(self.focus).map(|field| field.kind.clone()) else {
            return;
        };
        match kind {
            FieldKind::Button => {}
            FieldKind::Select { options } => self.cycle_select(&options, code),
            FieldKind::Number => match code {
                KeyCode::Char(ch) if ch.is_ascii_digit() => self.insert_char(ch),
                KeyCode::Backspace => self.backspace(),
                KeyCode::Left => self.nudge(-1),
                KeyCode::Right => self.nudge(1),
                _ => {}
            },
            FieldKind::Tags { suggestions } => match code {
                KeyCode::Char(ch) if !ch.is_control() => self.insert_char(ch),
                KeyCode::Backspace => self.backspace_tag(),
                KeyCode::Left => self.nudge(-1),
                KeyCode::Right if !self.accept_suggestion(&suggestions) => self.nudge(1),
                _ => {}
            },
            FieldKind::Text | FieldKind::Multiline => match code {
                KeyCode::Char(ch) if !ch.is_control() => self.insert_char(ch),
                KeyCode::Backspace => self.backspace(),
                KeyCode::Left => self.nudge(-1),
                KeyCode::Right => self.nudge(1),
                _ => {}
            },
        }
    }

    fn cycle_select(&mut self, options: &[String], code: KeyCode) {
        if options.is_empty() {
            return;
        }
        let Some(field) = self.fields.get_mut(self.focus) else {
            return;
        };
        let current = options
            .iter()
            .position(|option| option == &field.value)
            .unwrap_or(0);
        let next = match code {
            KeyCode::Left | KeyCode::Char('h') => current.saturating_sub(1),
            KeyCode::Right | KeyCode::Char('l') => (current + 1).min(options.len() - 1),
            _ => return,
        };
        field.value = options[next].clone();
        field.cursor = field.value.chars().count();
        self.dirty = true;
    }

    fn accept_suggestion(&mut self, suggestions: &[String]) -> bool {
        let Some(field) = self.fields.get_mut(self.focus) else {
            return false;
        };
        let Some(suggestion) = suggestions
            .iter()
            .find(|suggestion| suggestion.starts_with(&field.value) && *suggestion != &field.value)
        else {
            return false;
        };
        field.value = suggestion.clone();
        field.cursor = field.value.chars().count();
        self.dirty = true;
        true
    }

    fn add_chip(&mut self) {
        let Some(field) = self.fields.get_mut(self.focus) else {
            return;
        };
        let chip = field.value.trim().to_string();
        if chip.is_empty() {
            return;
        }
        if !field.chips.iter().any(|existing| existing == &chip) {
            field.chips.push(chip);
        }
        field.value.clear();
        field.cursor = 0;
        self.dirty = true;
    }

    fn backspace_tag(&mut self) {
        let empty = self
            .fields
            .get(self.focus)
            .is_some_and(|field| field.value.is_empty());
        if empty {
            let removed = self
                .fields
                .get_mut(self.focus)
                .and_then(|field| field.chips.pop())
                .is_some();
            if removed {
                self.dirty = true;
            }
            return;
        }
        self.backspace();
    }

    fn insert_char(&mut self, ch: char) {
        let Some(field) = self.fields.get_mut(self.focus) else {
            return;
        };
        insert(&mut field.value, &mut field.cursor, ch);
        self.dirty = true;
    }

    fn backspace(&mut self) {
        let Some(field) = self.fields.get_mut(self.focus) else {
            return;
        };
        let before = field.value.clone();
        backspace(&mut field.value, &mut field.cursor);
        if field.value != before {
            self.dirty = true;
        }
    }

    fn nudge(&mut self, delta: isize) {
        let Some(field) = self.fields.get_mut(self.focus) else {
            return;
        };
        if delta < 0 {
            move_left(&mut field.cursor);
        } else {
            move_right(&field.value, &mut field.cursor);
        }
    }

    fn move_focus(&mut self, delta: isize) {
        if self.fields.is_empty() {
            return;
        }
        let last = self.fields.len() as isize - 1;
        self.focus = (self.focus as isize + delta).clamp(0, last) as usize;
    }
}

fn render_field(field: &Field) -> String {
    match &field.kind {
        FieldKind::Button => format!("[{}]", field.label),
        FieldKind::Tags { suggestions } => {
            let chips = if field.chips.is_empty() {
                String::new()
            } else {
                format!("{} ", field.chips.join(", "))
            };
            let ghost = if field.value.is_empty() {
                field.placeholder.clone()
            } else {
                field.value.clone()
            };
            let hint = suggestions
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            if hint.is_empty() {
                format!("{}: {chips}{ghost}", field.label)
            } else {
                format!("{}: {chips}{ghost}  ({hint})", field.label)
            }
        }
        _ => {
            let shown = if field.value.is_empty() {
                field.placeholder.clone()
            } else {
                field.value.replace('\n', " ")
            };
            format!("{}: {shown}", field.label)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Form {
        Form::new(vec![
            Field::text("name", "Name"),
            Field::tags("tags", "Tags", vec!["web".into()]),
            Field::button("save", "Save"),
        ])
    }

    #[test]
    fn typing_q_does_not_leave_the_form_and_ctrl_s_submits() {
        let mut form = sample();
        assert_eq!(
            form.on_key(KeyCode::Char('q'), KeyModifiers::NONE),
            FormEffect::Continue
        );
        assert_eq!(form.field("name").unwrap().value, "q");
        assert!(form.dirty);
        assert_eq!(
            form.on_key(KeyCode::Esc, KeyModifiers::NONE),
            FormEffect::Continue
        );
        assert!(form.confirm_discard);
        assert_eq!(
            form.on_key(KeyCode::Esc, KeyModifiers::NONE),
            FormEffect::Cancel
        );
    }

    #[test]
    fn enter_adds_a_chip_and_backspace_removes_it() {
        let mut form = sample();
        form.focus = 1;
        form.on_key(KeyCode::Char('d'), KeyModifiers::NONE);
        form.on_key(KeyCode::Char('b'), KeyModifiers::NONE);
        assert_eq!(
            form.on_key(KeyCode::Enter, KeyModifiers::NONE),
            FormEffect::Continue
        );
        assert_eq!(
            form.field("tags").unwrap().chips,
            vec!["web".to_string(), "db".to_string()]
        );
        form.on_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(form.field("tags").unwrap().chips, vec!["web".to_string()]);
    }

    #[test]
    fn ctrl_s_and_the_save_row_submit() {
        let mut form = sample();
        assert_eq!(
            form.on_key(KeyCode::Char('s'), KeyModifiers::CONTROL),
            FormEffect::Submit
        );
        form.focus = 2;
        assert_eq!(
            form.on_key(KeyCode::Enter, KeyModifiers::NONE),
            FormEffect::Submit
        );
    }

    #[test]
    fn caps_lock_ctrl_s_still_submits() {
        let mut form = sample();
        assert_eq!(
            form.on_key(KeyCode::Char('S'), KeyModifiers::CONTROL),
            FormEffect::Submit
        );
    }
}
