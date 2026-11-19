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

fn marked(names: &[&str]) -> App {
    let mut app = app();
    app.files.local.entries = names.iter().map(|name| entry(name)).collect();
    app.files.remote.entries = names.iter().map(|name| entry(name)).collect();
    for name in names {
        app.files.local.marks.insert((*name).to_string());
    }
    app
}

fn marked_pair() -> App {
    let mut app = app();
    app.files.local.entries = vec![entry("a.txt"), entry("b.txt")];
    app.files.local.marks.insert("a.txt".to_string());
    app.files.local.marks.insert("b.txt".to_string());
    app.files.remote.entries = vec![entry("a.txt"), entry("b.txt")];
    app
}

fn copy_sources(commands: &[UiCommand]) -> Vec<String> {
    commands
        .iter()
        .find_map(|command| match command {
            UiCommand::Files(FilesCommand::Copy { sources, .. }) => Some(sources.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn qa_retest_overwrite_one_does_not_replace_every_marked_name() {
    let mut app = marked_pair();
    let commands = app.on_key(KeyCode::Char('c'), KeyModifiers::NONE);
    assert!(
        commands.is_empty() && app.files.conflict.is_some(),
        "expected a conflict prompt before copying two existing names, commands={commands:?}"
    );
    let commands = app.on_key(KeyCode::Char('o'), KeyModifiers::NONE);
    let sources = copy_sources(&commands);
    assert!(
        app.files.conflict.is_some() || sources.len() == 1,
        "overwrite-one copied every conflicting name: sources={sources:?} conflict={:?}",
        app.files.conflict
    );
}

#[test]
fn qa_retest_skip_leaves_the_other_conflict_and_cancel_copies_nothing() {
    let mut app = marked_pair();
    app.on_key(KeyCode::Char('c'), KeyModifiers::NONE);
    let commands = app.on_key(KeyCode::Char('s'), KeyModifiers::NONE);
    let sources = copy_sources(&commands);
    assert!(
        sources.iter().all(|source| !source.ends_with("a.txt")),
        "skip still queued the prompted file: {sources:?}"
    );
    assert!(
        app.files
            .conflict
            .as_ref()
            .is_some_and(|dialog| dialog.path.ends_with("b.txt"))
            || sources == vec!["/tmp/local/b.txt".to_string()],
        "skip did not keep b.txt for a later decision: sources={sources:?} conflict={:?}",
        app.files.conflict
    );

    let mut app = marked_pair();
    app.on_key(KeyCode::Char('c'), KeyModifiers::NONE);
    let commands = app.on_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(
        commands.is_empty() && app.files.conflict.is_none(),
        "cancel should copy nothing, commands={commands:?} conflict={:?}",
        app.files.conflict
    );
}

#[test]
fn qa_retest_three_conflicts_do_not_drop_the_third_name() {
    let mut app = marked(&["a.txt", "b.txt", "c.txt"]);
    assert!(
        app.on_key(KeyCode::Char('c'), KeyModifiers::NONE)
            .is_empty()
    );
    assert!(
        app.files
            .conflict
            .as_ref()
            .is_some_and(|dialog| dialog.path.ends_with("a.txt"))
    );
    assert_eq!(
        copy_sources(&app.on_key(KeyCode::Char('o'), KeyModifiers::NONE)),
        vec!["/tmp/local/a.txt".to_string()]
    );
    let commands = app.on_key(KeyCode::Char('o'), KeyModifiers::NONE);
    assert!(
        copy_sources(&commands)
            .iter()
            .any(|source| source.ends_with("b.txt")),
        "{commands:?}"
    );
    assert!(
        app.files
            .conflict
            .as_ref()
            .is_some_and(|dialog| dialog.path.ends_with("c.txt")),
        "third overwrite should prompt c.txt, got {:?}",
        app.files.conflict
    );

    let mut app = marked(&["a.txt", "b.txt", "c.txt"]);
    app.on_key(KeyCode::Char('c'), KeyModifiers::NONE);
    app.on_key(KeyCode::Char('s'), KeyModifiers::NONE);
    let commands = app.on_key(KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(
        app.files
            .conflict
            .as_ref()
            .is_some_and(|dialog| dialog.path.ends_with("c.txt")),
        "after skipping a and b, c should be prompted, got {:?} commands={commands:?}",
        app.files.conflict
    );
}
