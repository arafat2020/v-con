//! Add, edit, and duplicate a host.
//!
//! The form emits one host when the user saves. Validation uses
//! [`crate::domain::host_form`] so the widget itself does not decide whether a
//! name or a jump is legal.

use crate::domain::host::{Host, Inventory};
use crate::domain::host_form::{self, Draft, DraftField, DraftReport};
use crate::tui::theme::Theme;
use crate::tui::widgets::form::{Field, Form, FormEffect};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use std::path::Path;

/// What the host form wants the shell to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostFormEvent {
    /// Keep the form open.
    Continue,
    /// Close without writing.
    Cancel,
    /// Write this host. `original_name` is set while editing.
    Submit {
        /// Name before the edit. `None` when adding or duplicating.
        original_name: Option<String>,
        /// Record to write.
        host: Box<Host>,
    },
}

/// The modal state for one host form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostForm {
    /// Window title.
    pub title: String,
    /// Host being edited. `None` for add and duplicate.
    pub original_name: Option<String>,
    /// Source host for a duplicate. Its options, tunnels, and disabled flag are copied on save.
    pub preserved: Option<Host>,
    /// Fields and focus.
    pub form: Form,
    /// A problem that is not attached to one field.
    pub status: Option<String>,
}

/// Blank form for a new host.
pub fn add_form(inventory: &Inventory, local_user: &str) -> HostForm {
    let mut form = HostForm {
        title: "Add host".to_string(),
        original_name: None,
        preserved: None,
        form: form_with(Draft::blank(), inventory, local_user, None),
        status: None,
    };
    refresh(&mut form, inventory);
    form
}

/// Form filled from `host`.
pub fn edit_form(host: &Host, inventory: &Inventory, local_user: &str) -> HostForm {
    let mut form = HostForm {
        title: format!("Edit {}", host.name),
        original_name: Some(host.name.clone()),
        preserved: None,
        form: form_with(
            host_form::draft_from_host(host),
            inventory,
            local_user,
            Some(host),
        ),
        status: None,
    };
    refresh(&mut form, inventory);
    form
}

/// A copy of `host` with a free name.
pub fn duplicate_form(host: &Host, inventory: &Inventory, local_user: &str) -> HostForm {
    let mut draft = host_form::draft_from_host(host);
    draft.name = host_form::duplicate_name(&host.name, inventory);
    let mut form = HostForm {
        title: format!("Duplicate {}", host.name),
        original_name: None,
        preserved: Some(host.clone()),
        form: form_with(draft, inventory, local_user, Some(host)),
        status: None,
    };
    form.form.dirty = true;
    refresh(&mut form, inventory);
    form
}

/// Apply a key, then refresh errors and tag suggestions.
pub fn on_key(
    form: &mut HostForm,
    inventory: &Inventory,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> HostFormEvent {
    let effect = form.form.on_key(code, modifiers);
    refresh(form, inventory);
    match effect {
        FormEffect::Continue => HostFormEvent::Continue,
        FormEffect::Cancel => HostFormEvent::Cancel,
        FormEffect::Submit => submit(form, inventory),
    }
}

/// Recompute field errors from the current values.
pub fn refresh(form: &mut HostForm, inventory: &Inventory) {
    let draft = draft_from_form(&form.form);
    suggest_tags(&mut form.form, inventory, &draft.tags);
    let report = host_form::check_draft(&draft, inventory, form.original_name.as_deref());
    apply_report(&mut form.form, &report);
    if let Some(field) = form.form.field_mut("identity") {
        field.warning = identity_warning(&field.value);
    }
    form.status = report.other.first().cloned();
    if form.form.has_errors()
        && let Some(field) = form.form.field_mut("save")
    {
        field.error = Some("fix the errors above before saving".to_string());
    }
}

fn submit(form: &HostForm, inventory: &Inventory) -> HostFormEvent {
    let draft = draft_from_form(&form.form);
    let report = host_form::check_draft(&draft, inventory, form.original_name.as_deref());
    if report.blocked() {
        return HostFormEvent::Continue;
    }
    let previous = form.preserved.as_ref().or_else(|| {
        form.original_name
            .as_deref()
            .and_then(|name| inventory.hosts.iter().find(|host| host.name == name))
    });
    match host_form::draft_to_host(&draft, previous) {
        Ok(host) => HostFormEvent::Submit {
            original_name: form.original_name.clone(),
            host: Box::new(host),
        },
        Err(_) => HostFormEvent::Continue,
    }
}

/// Draw the form in the middle of `area`.
pub fn draw(frame: &mut Frame, area: Rect, form: &HostForm, theme: &Theme) {
    if area.width < 8 || area.height < 4 {
        return;
    }
    let width = area.width.saturating_sub(4).clamp(24, 72);
    let height = area.height.saturating_sub(2).clamp(8, 22);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let mut lines = form.form.lines();
    if let Some(status) = &form.status {
        lines.push(status.clone());
    }
    lines.push("Tab moves  Ctrl+S saves  Esc cancels".to_string());
    let paragraph = Paragraph::new(lines.join("\n"))
        .block(
            Block::bordered()
                .title(format!(" {} ", form.title))
                .border_style(theme.selected()),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

fn form_with(
    draft: Draft,
    inventory: &Inventory,
    local_user: &str,
    previous: Option<&Host>,
) -> Form {
    let user_placeholder = inventory
        .defaults
        .user
        .clone()
        .unwrap_or_else(|| local_user.to_string());
    let port_placeholder = inventory.defaults.port.unwrap_or(22).to_string();
    let mut options = vec!["none".to_string()];
    if let Some(jump) = previous.and_then(|host| host.jump.clone())
        && jump != "none"
        && !options.iter().any(|option| option == &jump)
        && !inventory.hosts.iter().any(|host| host.name == jump)
    {
        options.push(jump);
    }
    for host in &inventory.hosts {
        options.push(host.name.clone());
    }
    let jump = if draft.jump.is_empty() {
        "none".to_string()
    } else {
        draft.jump.clone()
    };
    let mut fields = vec![
        Field::text("name", "Name"),
        Field::text("address", "Address"),
        Field::text("user", "User"),
        Field::number("port", "Port"),
        Field::text("identity", "Identity"),
        Field::select("jump", "Jump", options, &jump),
        Field::tags("tags", "Tags", draft.tags.clone()),
        Field::multiline("notes", "Notes"),
        Field::button("save", "Save"),
    ];
    set_text(&mut fields, "name", &draft.name);
    set_text(&mut fields, "address", &draft.address);
    set_text(&mut fields, "user", &draft.user);
    set_text(&mut fields, "port", &draft.port);
    set_text(&mut fields, "identity", &draft.identity_file);
    set_text(&mut fields, "notes", &draft.notes);
    if let Some(field) = fields.iter_mut().find(|field| field.id == "user") {
        field.placeholder = format!("{user_placeholder} (default)");
    }
    if let Some(field) = fields.iter_mut().find(|field| field.id == "port") {
        field.placeholder = format!("{port_placeholder} (default)");
    }
    if let Some(field) = fields.iter_mut().find(|field| field.id == "identity") {
        field.placeholder = "ssh default".to_string();
    }
    Form::new(fields)
}

fn set_text(fields: &mut [Field], id: &str, value: &str) {
    if let Some(field) = fields.iter_mut().find(|field| field.id == id) {
        field.cursor = value.chars().count();
        field.value = value.to_string();
    }
}

fn draft_from_form(form: &Form) -> Draft {
    Draft {
        name: value(form, "name"),
        address: value(form, "address"),
        user: value(form, "user"),
        port: value(form, "port"),
        identity_file: value(form, "identity"),
        jump: value(form, "jump"),
        tags: form
            .field("tags")
            .map(|field| field.chips.clone())
            .unwrap_or_default(),
        notes: value(form, "notes"),
    }
}

fn value(form: &Form, id: &str) -> String {
    form.field(id)
        .map(|field| field.value.clone())
        .unwrap_or_default()
}

fn suggest_tags(form: &mut Form, inventory: &Inventory, chips: &[String]) {
    let prefix = form
        .field("tags")
        .map(|field| field.value.trim().to_string())
        .unwrap_or_default();
    let mut suggestions = Vec::new();
    if !prefix.is_empty() {
        for host in &inventory.hosts {
            for tag in &host.tags {
                if tag.starts_with(&prefix)
                    && tag != &prefix
                    && !chips.iter().any(|chip| chip == tag)
                    && !suggestions.iter().any(|existing: &String| existing == tag)
                {
                    suggestions.push(tag.clone());
                }
            }
        }
    }
    if let Some(field) = form.field_mut("tags")
        && let crate::tui::widgets::form::FieldKind::Tags { suggestions: slot } = &mut field.kind
    {
        *slot = suggestions;
    }
}

fn apply_report(form: &mut Form, report: &DraftReport) {
    for field in &mut form.fields {
        field.error = None;
    }
    for (field_id, id) in [
        (DraftField::Name, "name"),
        (DraftField::Address, "address"),
        (DraftField::User, "user"),
        (DraftField::Port, "port"),
        (DraftField::Identity, "identity"),
        (DraftField::Jump, "jump"),
        (DraftField::Tags, "tags"),
        (DraftField::Notes, "notes"),
    ] {
        let Some(message) = report.message(field_id) else {
            continue;
        };
        if let Some(field) = form.field_mut(id) {
            field.error = Some(message.to_string());
        }
    }
}

fn identity_warning(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let expanded = shellexpand::tilde(path);
    if Path::new(expanded.as_ref()).is_file() {
        None
    } else {
        Some("identity file was not found on this machine".to_string())
    }
}

impl Draft {
    fn blank() -> Self {
        Self {
            name: String::new(),
            address: String::new(),
            user: String::new(),
            port: String::new(),
            identity_file: String::new(),
            jump: "none".to_string(),
            tags: Vec::new(),
            notes: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn inventory() -> Inventory {
        toml_edit::de::from_str(
            r#"
            [[hosts]]
            name = "web-1"
            address = "203.0.113.10"
            tags = ["web"]
            "#,
        )
        .unwrap()
    }

    #[test]
    fn duplicate_name_is_shown_and_save_does_not_submit() {
        let inventory = inventory();
        let mut form = add_form(&inventory, "ada");
        for ch in "web-1".chars() {
            on_key(&mut form, &inventory, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        assert!(
            form.form
                .field("name")
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("duplicated")
        );
        let event = on_key(
            &mut form,
            &inventory,
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        );
        assert_eq!(event, HostFormEvent::Continue);
    }

    #[test]
    fn a_valid_host_submits_and_q_stays_in_the_name() {
        let inventory = inventory();
        let mut form = add_form(&inventory, "ada");
        on_key(
            &mut form,
            &inventory,
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        );
        assert_eq!(form.form.field("name").unwrap().value, "q");
        let mut form = add_form(&inventory, "ada");
        for ch in "edge".chars() {
            on_key(&mut form, &inventory, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_key(&mut form, &inventory, KeyCode::Down, KeyModifiers::NONE);
        for ch in "10.0.0.1".chars() {
            on_key(&mut form, &inventory, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        match on_key(
            &mut form,
            &inventory,
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        ) {
            HostFormEvent::Submit {
                host,
                original_name,
            } => {
                assert!(original_name.is_none());
                assert_eq!(host.name, "edge");
                assert_eq!(host.address, "10.0.0.1");
            }
            other => panic!("expected submit, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_keeps_options_the_form_does_not_edit() {
        let mut host = inventory().hosts.remove(0);
        host.disabled = true;
        host.ssh_options
            .insert("ForwardAgent".to_string(), "yes".to_string());
        let inventory = Inventory {
            hosts: vec![host.clone()],
            ..Inventory::default()
        };
        let mut form = duplicate_form(&host, &inventory, "ada");
        match on_key(
            &mut form,
            &inventory,
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        ) {
            HostFormEvent::Submit {
                host,
                original_name,
            } => {
                assert!(original_name.is_none());
                assert_eq!(host.name, "web-1-copy");
                assert!(host.disabled);
                assert_eq!(
                    host.ssh_options.get("ForwardAgent").map(String::as_str),
                    Some("yes")
                );
            }
            other => panic!("expected submit, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_identity_file_is_a_warning() {
        let inventory = Inventory::default();
        let mut form = add_form(&inventory, "ada");
        form.form.field_mut("identity").unwrap().value = "/no/such/identity".to_string();
        refresh(&mut form, &inventory);
        let warning = form
            .form
            .field("identity")
            .unwrap()
            .warning
            .clone()
            .unwrap();
        assert!(warning.contains("not found"), "{warning}");

        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("id_ed25519");
        std::fs::write(&key, "key").unwrap();
        form.form.field_mut("identity").unwrap().value = key.display().to_string();
        refresh(&mut form, &inventory);
        assert!(form.form.field("identity").unwrap().warning.is_none());
    }
}
