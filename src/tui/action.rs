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
