//! State changes for the TUI.
//!
//! [`Action`] is what a key or a timer means. [`UiCommand`] is the side effect
//! `App::update` asks the event loop to perform. Update itself does not touch
//! the clipboard or the terminal.

/// Something that can change the UI. Search editing produces the `Search*` variants
/// directly; other keys are translated through the keymap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Leave the TUI and restore the terminal.
    Quit,
    /// 250 ms heartbeat. Expires the toast.
    Tick,
    /// Open or close the help modal.
    ToggleHelp,
    /// Open or close the validation-problem modal.
    ToggleProblems,
    /// Close whichever modal is open.
    CloseModal,
    /// Switch the numbered tab. `1` is Hosts.
    SwitchScreen(u8),
    /// Move focus to the tag tree.
    FocusTree,
    /// Move focus to the host table.
    FocusTable,
    /// Previous row, or previous tag when the tree is focused.
    MoveUp,
    /// Next row, or next tag when the tree is focused.
    MoveDown,
    /// First row or first tag.
    MoveTop,
    /// Last row or last tag.
    MoveBottom,
    /// Move by one table page.
    PageUp,
    /// Move by one table page.
    PageDown,
    /// Start typing a filter. The query so far is kept.
    OpenSearch,
    /// Append one character to the search query and filter immediately.
    SearchInsert(char),
    /// Delete the last search character.
    SearchBackspace,
    /// Leave the search box and keep the current filter.
    SearchSubmit,
    /// Clear the filter and leave the search box.
    SearchClear,
    /// Toggle multi-select on the cursor host. Selection is stored by name.
    ToggleSelect,
    /// Select every host currently visible.
    SelectAll,
    /// Drop the multi-selection. Does not clear the search filter.
    ClearSelection,
    /// Ask the event loop to copy the cursor host's ssh command.
    CopySshCommand,
    /// Hand the terminal to ssh for the cursor host.
    Connect,
    /// Toggle the favorite flag on the cursor host.
    ToggleFavorite,
    /// Switch the table between inventory order and frecency.
    ToggleSort,
    /// Open `hosts.toml` in `$EDITOR`, then reload it.
    EditInventory,
    /// Open a blank host form.
    AddHost,
    /// Open the host form for the cursor host.
    EditHost,
    /// Open the host form with a copied name.
    DuplicateHost,
    /// Ask to delete the cursor host.
    DeleteHost,
    /// Open the ssh config import screen.
    ImportHosts,
    /// Open the exec prompt for the selection, or the cursor host.
    Exec,
    /// Reopen the exec results view.
    ShowExec,
    /// Open the Files screen for the cursor host.
    OpenFiles,
    /// Open an embedded PTY session for the cursor host (or each selection).
    OpenSession,
}

/// Work the event loop performs after `update` returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    /// Copy this text to the clipboard.
    Copy(String),
    /// Write the in-memory state file.
    Persist,
    /// Leave the TUI, run a child with the real terminal, then come back.
    Suspend(Handoff),
    /// Start a parallel exec. The loop owns the child processes.
    StartExec(crate::services::exec::ExecLaunch),
    /// Kill the running exec and mark hosts that have not started as skipped.
    CancelExec,
    /// Send a command to the background health scheduler.
    Health(crate::services::health::HealthControl),
    /// File-browser work for the event loop.
    Files(FilesCommand),
    /// Embedded-session work for the event loop.
    Sessions(SessionsCommand),
}

/// Side effects for embedded PTY sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionsCommand {
    /// Spawn `ssh -t` for `host` and place it in the UI.
    Open {
        /// Inventory host name.
        host: String,
        /// Initial rows.
        rows: u16,
        /// Initial columns.
        cols: u16,
        /// Where the new session goes.
        place: SessionPlace,
    },
    /// Write bytes to a live session.
    Write {
        /// Target session.
        id: crate::services::sessions::SessionId,
        /// Encoded key or paste bytes.
        bytes: Vec<u8>,
    },
    /// Resize one session's PTY.
    Resize {
        /// Target session.
        id: crate::services::sessions::SessionId,
        /// Rows.
        rows: u16,
        /// Columns.
        cols: u16,
    },
    /// Kill and drop a session.
    Close(crate::services::sessions::SessionId),
    /// Kill every session (on quit).
    CloseAll,
    /// Move scrollback for a session (positive looks at older lines).
    Scroll {
        /// Target session.
        id: crate::services::sessions::SessionId,
        /// Row delta.
        delta: isize,
    },
    /// Replace an ended session with a fresh connection.
    Reconnect {
        /// Pane to reuse.
        id: crate::services::sessions::SessionId,
        /// Host name.
        host: String,
        /// Rows.
        rows: u16,
        /// Columns.
        cols: u16,
    },
}

/// Where a newly opened session is attached in the Sessions UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionPlace {
    /// New sub-tab.
    NewTab,
    /// Split the focused pane.
    Split(crate::tui::screens::sessions::SplitDir),
}

/// Side effects for the Files screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesCommand {
    /// Reload local and/or remote listings.
    Refresh {
        /// Refresh the local pane.
        local: bool,
        /// Refresh the remote pane.
        remote: bool,
    },
    /// Open an SFTP session to `host`.
    Connect(String),
    /// Close the remote SFTP session.
    Disconnect,
    /// Start copying the given paths toward the other pane.
    Copy {
        /// Upload or download.
        direction: crate::services::files::TransferDirection,
        /// Source paths.
        sources: Vec<String>,
        /// Destination directory.
        dest_dir: String,
        /// Remote host name for labels.
        host: String,
    },
    /// Cancel active transfers.
    CancelTransfers,
    /// Delete paths on the local or remote side.
    Delete {
        /// Paths to remove.
        paths: Vec<String>,
        /// True deletes on the remote filesystem.
        remote: bool,
    },
    /// Create a directory.
    Mkdir {
        /// Absolute path.
        path: String,
        /// True creates on the remote filesystem.
        remote: bool,
    },
    /// Rename within one filesystem.
    Rename {
        /// Source path.
        from: String,
        /// Destination path.
        to: String,
        /// True operates on the remote filesystem.
        remote: bool,
    },
    /// Load a text file into the viewer (cap 1 MB).
    View {
        /// Absolute path.
        path: String,
        /// True reads from the remote filesystem.
        remote: bool,
    },
}

/// A child process that needs the real terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handoff {
    /// Interactive ssh. The banner is printed before the process starts.
    Connect(ConnectHandoff),
    /// `$EDITOR` opened on `hosts.toml`.
    Editor(EditorHandoff),
}

/// ssh invocation produced by the connect service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectHandoff {
    /// Host that was recorded in the state file.
    pub host_name: String,
    /// Line printed on the normal screen before ssh starts.
    pub banner: String,
    /// `ssh` binary.
    pub program: std::path::PathBuf,
    /// Arguments after the binary.
    pub args: Vec<String>,
}

/// Editor invocation. `args` already includes the file path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorHandoff {
    /// `$EDITOR` program.
    pub program: std::path::PathBuf,
    /// Arguments, including the inventory path.
    pub args: Vec<String>,
}
