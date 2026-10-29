//! Validate a host form before it is written.
//!
//! The form edits plain strings. [`check_draft`] applies them to a copy of the
//! inventory and runs the same checks as `vcon check`, so a duplicate name or
//! a jump cycle is rejected before the file changes.

use crate::domain::host::{Host, Inventory, Problem, ProblemSite, validate_inventory};

/// Which form field a message belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftField {
    /// Inventory name.
    Name,
    /// Address or DNS name.
    Address,
    /// Login name. Empty means "use the default".
    User,
    /// Port text. Empty means "use the default".
    Port,
    /// Identity file path. A missing file is a warning, not an error from `check_draft`.
    Identity,
    /// Jump host, or `none`.
    Jump,
    /// Tag chips.
    Tags,
    /// Free-form notes.
    Notes,
}

/// The strings the host form is editing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    /// Proposed inventory name.
    pub name: String,
    /// Proposed address.
    pub address: String,
    /// Empty keeps the default user.
    pub user: String,
    /// Empty keeps the default port.
    pub port: String,
    /// Empty omits `-i`.
    pub identity_file: String,
    /// `none` or empty omits the jump.
    pub jump: String,
    /// Tag chips, not including the text still being typed.
    pub tags: Vec<String>,
    /// Notes. Empty omits the field.
    pub notes: String,
}

/// Field errors plus any problem that does not belong to one field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftReport {
    /// Messages that disable Save.
    pub errors: Vec<(DraftField, String)>,
    /// Problems on other hosts caused by this edit, such as a broken jump.
    pub other: Vec<String>,
}

impl DraftReport {
    /// True when Save must stay disabled.
    pub fn blocked(&self) -> bool {
        !self.errors.is_empty() || !self.other.is_empty()
    }

    /// The message for one field, if it has an error.
    pub fn message(&self, field: DraftField) -> Option<&str> {
        self.errors
            .iter()
            .find(|(candidate, _)| *candidate == field)
            .map(|(_, message)| message.as_str())
    }
}

/// Check `draft` against `inventory`.
///
/// `original_name` is the host being edited. `None` means this is a new host.
/// `identity_missing` is computed by the caller because this module does not
/// touch the filesystem. A missing identity file is a warning, not an error,
/// and is not included here.
pub fn check_draft(
    draft: &Draft,
    inventory: &Inventory,
    original_name: Option<&str>,
) -> DraftReport {
    let host = match draft_to_host(draft, original_host(inventory, original_name)) {
        Ok(host) => host,
        Err(message) => {
            return DraftReport {
                errors: vec![(DraftField::Port, message)],
                other: Vec::new(),
            };
        }
    };
    let preview = preview_inventory(inventory, original_name, &host);
    let index = preview
        .hosts
        .iter()
        .rposition(|candidate| candidate.name == host.name)
        .unwrap_or(0);
    let mut errors = Vec::new();
    let mut other = Vec::new();
    for problem in validate_inventory(&preview) {
        match classify(&problem, index) {
            Some(field) => errors.push((field, problem.message)),
            None => other.push(problem.message),
        }
    }
    DraftReport { errors, other }
}

/// Build the host record the store would write.
///
/// Optional fields that are blank stay unset so `[defaults]` still apply.
/// `ssh_options`, tunnels, and `disabled` are copied from the host being edited.
pub fn draft_to_host(draft: &Draft, previous: Option<&Host>) -> Result<Host, String> {
    let port = if draft.port.trim().is_empty() {
        None
    } else {
        match draft.port.trim().parse::<u16>() {
            Ok(port) => Some(port),
            Err(_) => return Err(format!("port '{}' is not a number", draft.port.trim())),
        }
    };
    Ok(Host {
        name: draft.name.trim().to_string(),
        address: draft.address.trim().to_string(),
        user: blank(draft.user.trim()),
        port,
        identity_file: blank(draft.identity_file.trim()),
        jump: normalize_jump(draft.jump.trim()),
        tags: draft
            .tags
            .iter()
            .map(|tag| tag.trim().to_string())
            .filter(|tag| !tag.is_empty())
            .collect(),
        notes: blank(draft.notes.trim()),
        ssh_options: previous
            .map(|host| host.ssh_options.clone())
            .unwrap_or_default(),
        tunnels: previous
            .map(|host| host.tunnels.clone())
            .unwrap_or_default(),
        disabled: previous.is_some_and(|host| host.disabled),
    })
}

/// Hosts that jump directly to `name`, plus `defaults` when the default jump is `name`.
pub fn deletion_blockers(inventory: &Inventory, name: &str) -> Vec<String> {
    let mut blockers = Vec::new();
    if inventory.defaults.jump.as_deref() == Some(name) {
        blockers.push("defaults".to_string());
    }
    for host in &inventory.hosts {
        if host.jump.as_deref() == Some(name) {
            blockers.push(host.name.clone());
        }
    }
    blockers
}

/// `web-1-copy`, then `web-1-copy-2`, until the name is free.
pub fn duplicate_name(name: &str, inventory: &Inventory) -> String {
    let mut candidate = format!("{name}-copy");
    let mut suffix = 2u32;
    while inventory.hosts.iter().any(|host| host.name == candidate) {
        candidate = format!("{name}-copy-{suffix}");
        suffix += 1;
    }
    candidate
}

/// Fill a draft from an existing host. Blank optionals stay blank.
pub fn draft_from_host(host: &Host) -> Draft {
    Draft {
        name: host.name.clone(),
        address: host.address.clone(),
        user: host.user.clone().unwrap_or_default(),
        port: host.port.map(|port| port.to_string()).unwrap_or_default(),
        identity_file: host.identity_file.clone().unwrap_or_default(),
        jump: host.jump.clone().unwrap_or_else(|| "none".to_string()),
        tags: host.tags.clone(),
        notes: host.notes.clone().unwrap_or_default(),
    }
}

fn original_host<'a>(inventory: &'a Inventory, original_name: Option<&str>) -> Option<&'a Host> {
    let name = original_name?;
    inventory.hosts.iter().find(|host| host.name == name)
}

fn preview_inventory(inventory: &Inventory, original_name: Option<&str>, host: &Host) -> Inventory {
    let mut next = inventory.clone();
    match original_name {
        None => next.hosts.push(host.clone()),
        Some(old) => {
            if let Some(existing) = next
                .hosts
                .iter_mut()
                .find(|candidate| candidate.name == old)
            {
                *existing = host.clone();
            } else {
                next.hosts.push(host.clone());
            }
            if old != host.name {
                retarget(&mut next, old, &host.name);
            }
        }
    }
    next
}

fn retarget(inventory: &mut Inventory, old: &str, new: &str) {
    for host in &mut inventory.hosts {
        if host.jump.as_deref() == Some(old) {
            host.jump = Some(new.to_string());
        }
    }
    if inventory.defaults.jump.as_deref() == Some(old) {
        inventory.defaults.jump = Some(new.to_string());
    }
}

fn classify(problem: &Problem, index: usize) -> Option<DraftField> {
    let message = problem.message.to_ascii_lowercase();
    if message.contains("duplicat") {
        return Some(DraftField::Name);
    }
    if problem.site != ProblemSite::Host(index) {
        return None;
    }
    if message.contains("jump") || message.contains("cycle") {
        Some(DraftField::Jump)
    } else if message.contains("port") {
        Some(DraftField::Port)
    } else if message.contains("address") {
        Some(DraftField::Address)
    } else if message.contains("name") || message.contains("duplicat") {
        Some(DraftField::Name)
    } else {
        None
    }
}

fn blank(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn normalize_jump(jump: &str) -> Option<String> {
    if jump.is_empty() || jump.eq_ignore_ascii_case("none") {
        None
    } else {
        Some(jump.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inventory() -> Inventory {
        toml_edit::de::from_str(
            r#"
            [[hosts]]
            name = "bastion"
            address = "198.51.100.5"

            [[hosts]]
            name = "web-1"
            address = "203.0.113.10"
            jump = "bastion"
            "#,
        )
        .unwrap()
    }

    fn draft(name: &str, address: &str) -> Draft {
        Draft {
            name: name.into(),
            address: address.into(),
            user: String::new(),
            port: String::new(),
            identity_file: String::new(),
            jump: "none".into(),
            tags: Vec::new(),
            notes: String::new(),
        }
    }

    #[test]
    fn duplicate_name_and_bad_characters_block_save() {
        let inventory = inventory();
        let mut draft = draft("Web-1", "203.0.113.10");
        let report = check_draft(&draft, &inventory, None);
        assert!(
            report
                .message(DraftField::Name)
                .unwrap()
                .contains("invalid")
        );
        draft.name = "web-1".into();
        let report = check_draft(&draft, &inventory, None);
        assert!(
            report
                .message(DraftField::Name)
                .unwrap()
                .contains("duplicated")
        );
        let report = check_draft(&draft, &inventory, Some("web-1"));
        assert!(!report.blocked());
    }

    #[test]
    fn jump_cycle_and_bad_port_are_field_errors() {
        let inventory = inventory();
        let mut draft = draft_from_host(
            inventory
                .hosts
                .iter()
                .find(|host| host.name == "bastion")
                .unwrap(),
        );
        draft.jump = "web-1".into();
        let report = check_draft(&draft, &inventory, Some("bastion"));
        assert!(report.message(DraftField::Jump).unwrap().contains("cycle"));
        draft.jump = "none".into();
        draft.port = "0".into();
        let report = check_draft(&draft, &inventory, Some("bastion"));
        assert!(report.message(DraftField::Port).is_some());
        draft.port = "nope".into();
        let report = check_draft(&draft, &inventory, Some("bastion"));
        assert!(
            report
                .message(DraftField::Port)
                .unwrap()
                .contains("not a number")
        );
    }

    #[test]
    fn rename_updates_jump_references_in_the_preview() {
        let inventory = inventory();
        let mut draft = draft_from_host(
            inventory
                .hosts
                .iter()
                .find(|host| host.name == "bastion")
                .unwrap(),
        );
        draft.name = "edge".into();
        let host = draft_to_host(&draft, Some(&inventory.hosts[0])).unwrap();
        let preview = preview_inventory(&inventory, Some("bastion"), &host);
        let web = preview
            .hosts
            .iter()
            .find(|host| host.name == "web-1")
            .unwrap();
        assert_eq!(web.jump.as_deref(), Some("edge"));
        assert!(!check_draft(&draft, &inventory, Some("bastion")).blocked());
    }

    #[test]
    fn deletion_lists_dependents_and_copy_names_are_unique() {
        let inventory = inventory();
        assert_eq!(
            deletion_blockers(&inventory, "bastion"),
            vec!["web-1".to_string()]
        );
        assert!(deletion_blockers(&inventory, "web-1").is_empty());
        assert_eq!(duplicate_name("web-1", &inventory), "web-1-copy");
    }
}
