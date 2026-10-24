# Phase 1 — Inventory and Host List

**Goal:** Load your hosts from `hosts.toml` and browse them in the Hosts
screen: tag tree, searchable table, detail pane.

**You can now:** add hosts by editing `hosts.toml` (forms come in phase 3),
open vcon, filter by tag, fuzzy-search, multi-select, and read host details.
CLI: `vcon init`, `vcon ls`, `vcon check`.

**Estimated effort:** 3–5 days

Reference: [03-data-model.md](../03-data-model.md), [06-tui-design.md → Hosts](../06-tui-design.md#1-hosts-home)

## Step 1 — Domain types (no I/O)

- [ ] `Host`, `Defaults`, `Inventory` with serde; `#[serde(default)]` on optional fields and `#[serde(deny_unknown_fields)]` to catch typos.
- [ ] `EffectiveHost` + a pure `resolve(host, defaults)` function.
- [ ] Validation returning **all** problems: duplicate names, bad characters, bad port, unknown jump host, jump cycles.
- [ ] Unit tests for resolution precedence, validation, cycle detection.

## Step 2 — Selectors (no I/O)

- [ ] Parse selector strings into terms (name, glob, `@tag`, `@a+b`, `all`, `!term`).
- [ ] Match against an inventory following the [evaluation rules](../03-data-model.md#evaluation).
- [ ] "Did you mean?" suggestions (`strsim`).
- [ ] Unit tests for every example in the data model.

## Step 3 — Store

- [ ] `InventoryStore::load()`: missing file → empty inventory; parse error → error with path + line.
- [ ] Write operations come in phase 3, but design the store with `toml_edit` in mind.
- [ ] `config.toml` loading for settings, theme, and keymap overrides (all optional, sensible defaults).
- [ ] **Watch the file** (stretch, `notify` crate): reload when `hosts.toml` changes on disk, so editing in another window updates the TUI.

## Step 4 — The ssh command builder

- [ ] Pure function in `infra/ssh/command.rs`: `EffectiveHost + options → Vec<String>`.
- [ ] Unit tests for port, key, user, jump host, extra `ssh_options`, BatchMode/ControlMaster options.
- [ ] The detail pane shows the resulting command line, and `y` copies it (good for trust and debugging).

## Step 5 — Hosts screen

- [ ] **Host table**: name, address, user, tags, last-connected; cursor movement (`j/k`, arrows, `g/G`, PgUp/PgDn); scrollbar for long lists.
- [ ] **Tag tree** (`tui-tree-widget`): `all`, one node per tag with counts, plus `recent` / `favorites` (empty until phase 2). Selecting a node filters the table. `h/l` moves focus between tree and table.
- [ ] **Search** (`/`): a text input in the table title; filters live as you type.
  - Plain text → fuzzy match (`nucleo`) over name + address + tags + notes, results ranked by score; highlight matched characters.
  - Text starting with `@` or `!`, or containing `*` → parse as a selector; show selector errors inline in red.
  - `Esc` clears search; `Enter` returns focus to the table keeping the filter.
- [ ] **Multi-select**: `Space` toggles, `Ctrl+A` selects all visible, `Esc` clears; selected count shown in the title.
- [ ] **Detail pane**: effective user@address:port, jump chain, identity file, tags, notes, ssh command. Hide automatically when the terminal is too narrow/short.
- [ ] **Empty state**: if there are no hosts, show "No hosts yet — press `i` to import from ~/.ssh/config or `a` to add one (phase 3), or edit ~/.config/vcon/hosts.toml".
- [ ] **Validation problems** from loading show as a warning badge; `?`-style modal lists them.

## Step 6 — CLI

- [ ] `vcon init`: create config dir + a commented example `hosts.toml` (refuse to overwrite).
- [ ] `vcon ls [selector]` with table and `--json` output.
- [ ] `vcon check`: print all validation problems; non-zero exit if any.

## Done when

- Your real servers are in `hosts.toml` and vcon shows them
- Typing `/pr ng` narrows to prod nginx hosts in milliseconds; `/@db,!db-3` works too
- The tag tree filters correctly and counts are right
- Snapshot tests cover: full list, filtered list, empty state, narrow terminal

## Pitfalls

- Keep filtering logic (fuzzy + selector) in a pure function `filter(inventory, query) → Vec<index + match info>` so it can be unit tested without the TUI.
- Keep the **cursor stable** when the filter changes: if the host under the cursor is still visible, keep it selected.
- Store multi-selection by host **name**, not table index — indices change when filtering.
