//! QA: the files conflict dialog is drawn and keyed, but nothing ever opens it
//! and the choice is discarded.

use super::action::{FilesCommand, UiCommand};
use super::app::App;
use super::screens::files::{ConflictDialog, PaneFocus};
use crate::config::Config;
use crate::domain::host::Inventory;
use crate::infra::remote_fs::{DirEntry, EntryKind};
use crate::paths::Paths;
use crate::services::inventory::InventoryBundle;
use crossterm::event::{KeyCode, KeyModifiers};
use toml_edit::DocumentMut;

fn entry(name: &str) -> DirEntry {
    DirEntry {
        name: name.to_string(),
        kind: EntryKind::File,
        size: 4,
        mtime: None,
        mode: Some(0o100644),
        target: None,
    }
}

fn app() -> App {
    let root = tempfile::tempdir().expect("tempdir");
    // Keep the directory for the App, which only stores the paths.
    let root = root.keep();
    let paths = Paths {
        config_dir: root.join("config"),
        data_dir: root.join("data"),
        state_dir: root.join("state"),
        cache_dir: root.join("cache"),
    };
    let config = Config {
        multiplexing: false,
        ..Config::default()
    };
    let bundle = InventoryBundle {
        inventory: Inventory::default(),
        hosts: Vec::new(),
        problems: Vec::new(),
        document: DocumentMut::new(),
    };
    let mut app = App::new(bundle, config, paths, true).expect("app");
    app.update(super::action::Action::SwitchScreen(3));
    app.files.host = Some("web-1".to_string());
    app.files.focus = PaneFocus::Local;
    app.files.local.path = "/tmp/local".to_string();
    app.files.local.entries = vec![entry("notes.txt")];
    app.files.remote.path = "/tmp/remote".to_string();
    app.files.remote.entries = vec![entry("notes.txt")];
    app
}

#[test]
fn qa_tui_existing_name_does_not_open_the_conflict_dialog() {
    let mut app = app();
    let commands = app.on_key(KeyCode::Char('c'), KeyModifiers::NONE);
    assert!(
        app.files.conflict.is_some(),
        "copying notes.txt onto an existing notes.txt must ask before overwrite; \
         conflict={:?} commands={commands:?}",
        app.files.conflict
    );
    assert!(
        !commands
            .iter()
            .any(|command| matches!(command, UiCommand::Files(FilesCommand::Copy { .. }))),
        "copy must wait for the conflict choice, got {commands:?}"
    );
}

#[test]
fn qa_tui_conflict_choice_is_not_discarded() {
    let mut app = app();
    app.files.conflict = Some(ConflictDialog {
        id: 7,
        path: "/tmp/remote/notes.txt".to_string(),
    });
    let commands = app.on_key(KeyCode::Char('o'), KeyModifiers::NONE);
    assert!(
        !commands.is_empty(),
        "pressing 'o' on the conflict dialog cleared it and did nothing (commands={commands:?}, \
         conflict now {:?})",
        app.files.conflict
    );
}
