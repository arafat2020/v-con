# Phase 3 — Host Forms and Import

**Goal:** Manage the whole inventory from inside the TUI: add, edit,
duplicate, delete, and import hosts through validated forms.

**You can now:** press `a` to add a host, `e` to edit, `D` to duplicate, `d`
to delete, `i` to import from `~/.ssh/config`. CLI: `vcon import ssh-config`.

**Estimated effort:** 3–5 days

**This completes milestone 1.** Start using vcon daily before moving on.

## Step 1 — Store writes

- [ ] `InventoryStore::add_host`, `update_host(old_name, host)`, `remove_host` using **`toml_edit`** so comments and formatting survive ([D3](../05-decisions.md#d3-use-toml_edit-for-writes)).
- [ ] Validate the **whole** inventory after the change, before writing; reject with errors if invalid.
- [ ] Atomic write: temp file in the same directory + rename; keep `hosts.toml.bak`.
- [ ] Renaming a host must also update `jump` references to it in other hosts, and its entry in the state file.
- [ ] Tests: round-trip, comments preserved after add/edit/remove, rename updates references.

## Step 2 — Form widgets (`tui/widgets/`)

Build reusable pieces; the snippet and tunnel forms in phase 8 reuse them.

- [ ] **Text field** (`tui-input` or `tui-textarea`): label, value, cursor, placeholder, inline error message.
- [ ] **Number field** (port) — text field with numeric validation.
- [ ] **Select field**: choose from a list (e.g. jump host from existing hosts, plus "none").
- [ ] **Tag field**: type a tag + Enter to add a chip; Backspace on empty removes the last chip; suggest existing tags as you type.
- [ ] **Multiline field** for notes.
- [ ] **Form container**: vertical list of fields, `Tab`/`Shift+Tab` (and `↑↓`) move focus, `Ctrl+S` or Enter on a "Save" button submits, `Esc` cancels (confirm if there are unsaved changes).

## Step 3 — Host form modal

- [ ] Fields: name, address, user, port, identity file, jump host, tags, notes. Show defaults as placeholders ("root (default)").
- [ ] **Live validation** as you type: invalid name characters, duplicate name, port range, jump cycle. Save is disabled while errors exist.
- [ ] Identity file: validate the file exists locally (warning, not error — it might exist on another machine).
- [ ] Add (`a`), edit (`e`, prefilled), duplicate (`D`, prefilled with name `web-1-copy`).
- [ ] **"Test connection" button** (stretch): runs `ssh -o BatchMode=yes … true` in the background with a spinner and shows ✔ or the classified error. Very useful when adding hosts.
- [ ] Delete (`d`): confirm modal naming the host; if other hosts use it as a jump host, say so and refuse (or offer to clear those references).

## Step 4 — Import screen

- [ ] `~/.ssh/config` parser in `infra/ssh_config.rs`: `Host` blocks with `HostName`, `User`, `Port`, `IdentityFile`, `ProxyJump`; ignore other keywords without failing; skip wildcard patterns; follow or warn on `Include`.
- [ ] Import modal: a checklist of discovered hosts with columns (name, address, user, status: *new* / *exists* / *skipped (wildcard)*). `Space` toggles, `a` toggles all, Enter imports the checked ones.
- [ ] Existing names: unchecked by default; option to overwrite.
- [ ] Imported hosts get the tag `imported`.
- [ ] CLI: `vcon import ssh-config [--file path] [--dry-run] [--overwrite]` using the same parser and store.
- [ ] Fixture-based tests for the parser edge cases.

## Done when

- You never need to open `hosts.toml` by hand (but you still can, and your comments survive vcon's edits)
- Adding a host with a duplicate name or a jump cycle is impossible — the form tells you why
- Importing your `~/.ssh/config` takes one screen and a keypress

## Pitfalls

- Form state (focused field, field values, errors) gets complicated fast. Keep the form as a self-contained component that emits one `SubmitHost(HostDraft)` action; validation logic lives in the domain layer, not in the widget.
- Don't save on every keystroke. Save once on submit.
- Text input inside a modal must swallow keys — `q` typed into a field must not quit vcon. This is why input routing order matters (see [design doc](../06-tui-design.md#components)).
