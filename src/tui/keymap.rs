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
    ///
    /// Caps Lock sends `A` with no Shift. When there is no exact binding for that
    /// uppercase letter, the lowercase binding is tried so `a` still adds a host.
    pub fn lookup(&self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        let modifiers = match code {
            KeyCode::Char(ch) if ch.is_ascii_uppercase() => {
                modifiers.difference(KeyModifiers::SHIFT)
            }
            _ => modifiers,
        };
        let modifiers =
            modifiers.intersection(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        if let Some(action) = self.bindings.get(&Binding { code, modifiers }).cloned() {
            return Some(action);
        }
        if modifiers.is_empty()
            && let KeyCode::Char(ch) = code
            && ch.is_ascii_uppercase()
        {
            return self
                .bindings
                .get(&Binding {
                    code: KeyCode::Char(ch.to_ascii_lowercase()),
                    modifiers: KeyModifiers::NONE,
                })
                .cloned();
        }
        None
    }

    /// Human-readable help lines derived from the current bindings.
    ///
    /// Actions are listed in a stable grouped order. Each line is
    /// `keys` padded then the action description, so `?` stays in sync
    /// with `[keys.hosts]` overrides.
    pub fn help_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (heading, actions) in HELP_GROUPS {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push((*heading).to_string());
            for action in *actions {
                let keys = self.keys_for(action);
                if keys.is_empty() {
                    continue;
                }
                let label = action_help_label(action);
                lines.push(format!("{:<16} {label}", keys.join(" / ")));
            }
        }
        lines
    }

    fn keys_for(&self, action: &Action) -> Vec<String> {
        let mut keys: Vec<String> = self
            .bindings
            .iter()
            .filter(|(_, bound)| *bound == action)
            .map(|(binding, _)| format_binding(*binding))
            .collect();
        keys.sort();
        keys.dedup();
        // Prefer letter chords before arrow aliases when both exist.
        keys.sort_by(|left, right| {
            let left_special = left.len() > 1;
            let right_special = right.len() > 1;
            left_special
                .cmp(&right_special)
                .then_with(|| left.cmp(right))
        });
        keys
    }
}

/// Help sections shown in the `?` modal.
const HELP_GROUPS: &[(&str, &[Action])] = &[
    (
        "Navigation",
        &[
            Action::MoveUp,
            Action::MoveDown,
            Action::MoveTop,
            Action::MoveBottom,
            Action::PageUp,
            Action::PageDown,
            Action::FocusTree,
            Action::FocusTable,
            Action::OpenSearch,
            Action::SwitchScreen(1),
            Action::SwitchScreen(2),
            Action::SwitchScreen(3),
            Action::SwitchScreen(4),
            Action::SwitchScreen(5),
        ],
    ),
    (
        "Selection",
        &[
            Action::ToggleSelect,
            Action::SelectAll,
            Action::ClearSelection,
        ],
    ),
    (
        "Hosts",
        &[
            Action::Connect,
            Action::OpenSession,
            Action::CopySshCommand,
            Action::ToggleFavorite,
            Action::ToggleSort,
            Action::AddHost,
            Action::EditHost,
            Action::DuplicateHost,
            Action::DeleteHost,
            Action::ImportHosts,
            Action::EditInventory,
            Action::OpenFiles,
            Action::OpenSnippets,
            Action::OpenTunnels,
            Action::Exec,
            Action::ShowExec,
        ],
    ),
    (
        "General",
        &[Action::ToggleHelp, Action::ToggleProblems, Action::Quit],
    ),
];

fn action_help_label(action: &Action) -> &'static str {
    match action {
        Action::Quit => "quit",
        Action::ToggleHelp => "this help",
        Action::ToggleProblems => "validation problems",
        Action::SwitchScreen(1) => "Hosts tab",
        Action::SwitchScreen(2) => "Health tab",
        Action::SwitchScreen(3) => "Files tab",
        Action::SwitchScreen(4) => "Sessions tab",
        Action::SwitchScreen(5) => "Log tab",
        Action::SwitchScreen(_) => "switch tab",
        Action::FocusTree => "focus tag tree",
        Action::FocusTable => "focus host table",
        Action::MoveUp => "move up",
        Action::MoveDown => "move down",
        Action::MoveTop => "top",
        Action::MoveBottom => "bottom",
        Action::PageUp => "page up",
        Action::PageDown => "page down",
        Action::OpenSearch => "search; Esc clears, Enter keeps it",
        Action::ToggleSelect => "toggle select",
        Action::SelectAll => "select visible hosts",
        Action::ClearSelection => "clear selection",
        Action::CopySshCommand => "copy the ssh command",
        Action::Connect => "connect",
        Action::ToggleFavorite => "toggle favorite",
        Action::ToggleSort => "sort by frecency",
        Action::EditInventory => "edit hosts.toml",
        Action::AddHost => "add a host",
        Action::EditHost => "edit the cursor host",
        Action::DuplicateHost => "duplicate the cursor host",
        Action::DeleteHost => "delete the cursor host",
        Action::ImportHosts => "import ~/.ssh/config",
        Action::Exec => "run a command on the selection",
        Action::ShowExec => "reopen exec results",
        Action::OpenFiles => "open files for the cursor host",
        Action::OpenSession => "open an embedded session",
        Action::OpenSnippets => "snippet picker",
        Action::OpenTunnels => "tunnels for the cursor host",
        Action::Tick
        | Action::CloseModal
        | Action::SearchInsert(_)
        | Action::SearchBackspace
        | Action::SearchSubmit
        | Action::SearchClear => "",
    }
}

fn format_binding(binding: Binding) -> String {
    // Shift+/ is the usual way to type `?`; show the character users expect.
    if binding.code == KeyCode::Char('/') && binding.modifiers.contains(KeyModifiers::SHIFT) {
        return "?".to_string();
    }
    let key = match binding.code {
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Down".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        KeyCode::PageUp => "PgUp".to_string(),
        KeyCode::PageDown => "PgDn".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(ch) => ch.to_string(),
        other => format!("{other:?}"),
    };
    if binding.modifiers.contains(KeyModifiers::CONTROL) {
        format!("ctrl-{key}")
    } else if binding.modifiers.contains(KeyModifiers::ALT) {
        format!("alt-{key}")
    } else {
        key
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
    bind(KeyCode::Char('x'), KeyModifiers::NONE, Action::Exec);
    bind(KeyCode::Char('X'), KeyModifiers::NONE, Action::ShowExec);
    bind(KeyCode::Char('f'), KeyModifiers::NONE, Action::OpenFiles);
    bind(KeyCode::Char('o'), KeyModifiers::NONE, Action::OpenSession);
    bind(KeyCode::Char('s'), KeyModifiers::NONE, Action::OpenSnippets);
    bind(KeyCode::Char('t'), KeyModifiers::NONE, Action::OpenTunnels);
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
        "exec" => Action::Exec,
        "show_exec" => Action::ShowExec,
        "files" | "open_files" => Action::OpenFiles,
        "open_session" | "session" => Action::OpenSession,
        "snippets" | "open_snippets" => Action::OpenSnippets,
        "tunnels" | "open_tunnels" => Action::OpenTunnels,
        "up" => Action::MoveUp,
        "down" => Action::MoveDown,
        _ => return None,
    })
}

/// Parse a chord such as `ctrl-]` into a key code and modifiers.
pub fn parse_chord(raw: &str) -> Option<(KeyCode, KeyModifiers)> {
    parse_key(raw).map(|binding| (binding.code, binding.modifiers))
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

    #[test]
    fn o_opens_an_embedded_session() {
        let keymap = Keymap::from_config(&KeyConfig::default()).unwrap();
        assert_eq!(
            keymap.lookup(KeyCode::Char('o'), KeyModifiers::NONE),
            Some(Action::OpenSession)
        );
    }

    #[test]
    fn caps_lock_a_still_adds_a_host() {
        let keymap = Keymap::from_config(&KeyConfig::default()).unwrap();
        assert_eq!(
            keymap.lookup(KeyCode::Char('a'), KeyModifiers::NONE),
            Some(Action::AddHost)
        );
        assert_eq!(
            keymap.lookup(KeyCode::Char('A'), KeyModifiers::NONE),
            Some(Action::AddHost)
        );
        // Exact uppercase bindings win over the lowercase fallback.
        assert_eq!(
            keymap.lookup(KeyCode::Char('E'), KeyModifiers::NONE),
            Some(Action::EditInventory)
        );
        assert_eq!(
            keymap.lookup(KeyCode::Char('e'), KeyModifiers::NONE),
            Some(Action::EditHost)
        );
    }

    #[test]
    fn help_lines_include_bound_actions() {
        let keymap = Keymap::from_config(&KeyConfig::default()).unwrap();
        let text = keymap.help_lines().join("\n");
        assert!(text.contains("Navigation"), "{text}");
        assert!(text.contains("connect"), "{text}");
        assert!(text.contains('q'), "{text}");
        assert!(text.contains('?'), "{text}");
    }

    #[test]
    fn help_lines_reflect_overrides() {
        let mut hosts = BTreeMap::new();
        hosts.insert("connect".into(), "c".into());
        let keymap = Keymap::from_config(&KeyConfig { hosts }).unwrap();
        let text = keymap.help_lines().join("\n");
        assert!(text.contains('c'), "{text}");
        assert!(
            !text.lines().any(|line| line.starts_with("Enter ")),
            "{text}"
        );
    }
}
