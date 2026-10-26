//! Default key bindings and `[keys.hosts]` overrides.
//!
//! An override names an action (`connect = "c"`) and replaces that action's
//! key. A key string the parser does not understand is a config error.
//! Action names that belong to a later phase are ignored.

use crate::config::KeyConfig;
use crate::error::Error;
use crate::tui::action::Action;
use crossterm::event::{KeyCode, KeyModifiers};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Binding {
    code: KeyCode,
    modifiers: KeyModifiers,
}

/// Map from a key chord to an [`Action`].
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: HashMap<Binding, Action>,
}

impl Keymap {
    /// Build the default Hosts bindings, then apply `keys.hosts`.
    pub fn from_config(keys: &KeyConfig) -> Result<Self, Error> {
        let mut bindings = defaults();
        for (name, raw) in &keys.hosts {
            let Some(action) = action_by_name(name) else {
                continue;
            };
            let binding = parse_key(raw).ok_or_else(|| {
                Error::Message(format!("unknown key binding '{raw}' for '{name}'"))
            })?;
            bindings.retain(|_, existing| existing != &action);
            bindings.insert(binding, action);
        }
        Ok(Self { bindings })
    }

    /// Find the action for this key. Uppercase letters ignore the Shift modifier
    /// because the shifted character is already in `code` (`G` rather than Shift+`g`).
    pub fn lookup(&self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        let modifiers = match code {
            KeyCode::Char(ch) if ch.is_ascii_uppercase() => {
                modifiers.difference(KeyModifiers::SHIFT)
            }
            _ => modifiers,
        };
        let modifiers =
            modifiers.intersection(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        self.bindings.get(&Binding { code, modifiers }).cloned()
    }
}

fn defaults() -> HashMap<Binding, Action> {
    let mut map = HashMap::new();
    let mut bind = |code, modifiers, action| {
        map.insert(Binding { code, modifiers }, action);
    };

    bind(KeyCode::Char('q'), KeyModifiers::NONE, Action::Quit);
    bind(KeyCode::Char('?'), KeyModifiers::NONE, Action::ToggleHelp);
    bind(KeyCode::Char('/'), KeyModifiers::SHIFT, Action::ToggleHelp);
    bind(
        KeyCode::Char('v'),
        KeyModifiers::NONE,
        Action::ToggleProblems,
    );
    for (index, key) in ['1', '2', '3', '4', '5'].into_iter().enumerate() {
        bind(
            KeyCode::Char(key),
            KeyModifiers::NONE,
            Action::SwitchScreen(index as u8 + 1),
        );
    }
    bind(KeyCode::Up, KeyModifiers::NONE, Action::MoveUp);
    bind(KeyCode::Down, KeyModifiers::NONE, Action::MoveDown);
    bind(KeyCode::Char('k'), KeyModifiers::NONE, Action::MoveUp);
    bind(KeyCode::Char('j'), KeyModifiers::NONE, Action::MoveDown);
    bind(KeyCode::Char('g'), KeyModifiers::NONE, Action::MoveTop);
    bind(KeyCode::Char('G'), KeyModifiers::NONE, Action::MoveBottom);
    bind(KeyCode::PageUp, KeyModifiers::NONE, Action::PageUp);
    bind(KeyCode::PageDown, KeyModifiers::NONE, Action::PageDown);
    bind(KeyCode::Left, KeyModifiers::NONE, Action::FocusTree);
    bind(KeyCode::Right, KeyModifiers::NONE, Action::FocusTable);
    bind(KeyCode::Char('h'), KeyModifiers::NONE, Action::FocusTree);
    bind(KeyCode::Char('l'), KeyModifiers::NONE, Action::FocusTable);
    bind(KeyCode::Char('/'), KeyModifiers::NONE, Action::OpenSearch);
    bind(KeyCode::Char(' '), KeyModifiers::NONE, Action::ToggleSelect);
    bind(KeyCode::Char('a'), KeyModifiers::CONTROL, Action::SelectAll);
    bind(KeyCode::Esc, KeyModifiers::NONE, Action::ClearSelection);
    bind(
        KeyCode::Char('y'),
        KeyModifiers::NONE,
        Action::CopySshCommand,
    );
    bind(KeyCode::Enter, KeyModifiers::NONE, Action::Connect);
    bind(
        KeyCode::Char('*'),
        KeyModifiers::SHIFT,
        Action::ToggleFavorite,
    );
    bind(
        KeyCode::Char('*'),
        KeyModifiers::NONE,
        Action::ToggleFavorite,
    );
    bind(
        KeyCode::Char('E'),
        KeyModifiers::NONE,
        Action::EditInventory,
    );
    bind(KeyCode::Char('S'), KeyModifiers::NONE, Action::ToggleSort);
    bind(KeyCode::Char('a'), KeyModifiers::NONE, Action::AddHost);
    bind(KeyCode::Char('e'), KeyModifiers::NONE, Action::EditHost);
    bind(
        KeyCode::Char('D'),
        KeyModifiers::NONE,
        Action::DuplicateHost,
    );
    bind(KeyCode::Char('d'), KeyModifiers::NONE, Action::DeleteHost);
    bind(KeyCode::Char('i'), KeyModifiers::NONE, Action::ImportHosts);
    map
}

fn action_by_name(name: &str) -> Option<Action> {
    Some(match name {
        "quit" => Action::Quit,
        "help" => Action::ToggleHelp,
        "problems" => Action::ToggleProblems,
        "search" => Action::OpenSearch,
        "select" => Action::ToggleSelect,
        "select_all" => Action::SelectAll,
        "copy" => Action::CopySshCommand,
        "connect" => Action::Connect,
        "favorite" => Action::ToggleFavorite,
        "sort" => Action::ToggleSort,
        "edit" => Action::EditInventory,
        "add" => Action::AddHost,
        "edit_host" => Action::EditHost,
        "duplicate" => Action::DuplicateHost,
        "delete" => Action::DeleteHost,
        "import" => Action::ImportHosts,
        "up" => Action::MoveUp,
        "down" => Action::MoveDown,
        _ => return None,
    })
}

fn parse_key(raw: &str) -> Option<Binding> {
    let raw = raw.trim();
    if let Some(letter) = raw.chars().next()
        && raw.chars().count() == 1
        && letter.is_ascii_uppercase()
    {
        return Some(Binding {
            code: KeyCode::Char(letter),
            modifiers: KeyModifiers::NONE,
        });
    }
    let raw = raw.to_ascii_lowercase();
    let (modifiers, key) = if let Some(rest) = raw.strip_prefix("ctrl-") {
        (KeyModifiers::CONTROL, rest)
    } else if let Some(rest) = raw.strip_prefix("alt-") {
        (KeyModifiers::ALT, rest)
    } else {
        (KeyModifiers::NONE, raw.as_str())
    };
    let code = match key {
        "enter" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "tab" => KeyCode::Tab,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        other if other.chars().count() == 1 => KeyCode::Char(other.chars().next()?),
        _ => return None,
    };
    Some(Binding { code, modifiers })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KeyConfig;
    use std::collections::BTreeMap;

    #[test]
    fn override_rebinds_connect() {
        let mut hosts = BTreeMap::new();
        hosts.insert("connect".into(), "c".into());
        let keymap = Keymap::from_config(&KeyConfig { hosts }).unwrap();
        assert_eq!(
            keymap.lookup(KeyCode::Char('c'), KeyModifiers::NONE),
            Some(Action::Connect)
        );
        assert_eq!(keymap.lookup(KeyCode::Enter, KeyModifiers::NONE), None);
    }
}
