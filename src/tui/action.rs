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
    /// Interactive connect. Phase 2. Phase 1 shows a toast.
    Connect,
}

/// Work the event loop performs after `update` returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    /// Copy this shell command to the clipboard.
    Copy(String),
}
