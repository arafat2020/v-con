# Phase 0 — Foundation and TUI Skeleton

**Goal:** A clean skeleton every later phase plugs into: the ratatui app loop,
safe terminal handling, async runtime, logging to a file, config paths, and a
CLI entry point.

**You can now:** run `vcon` and see an empty TUI with a tab bar (Hosts,
Health, Files, Sessions, Log), switch tabs with `1`–`5`, open a help modal with
`?`, and quit with `q`. Your terminal is always restored — even on a panic.

**Estimated effort:** 2–3 days

Reference: [06-tui-design.md](../06-tui-design.md)

## Step 1 — Project setup

- [ ] Dependencies: `ratatui`, `crossterm` (feature `event-stream`), `tokio` (`full` is fine to start), `tokio-util`, `futures`, `clap` (derive), `anyhow`, `thiserror`, `tracing`, `tracing-subscriber`, `tracing-appender`, `directories` or `etcetera`.
- [ ] Create the module folders from [02-architecture.md](../02-architecture.md#source-layout) with empty `mod.rs` files.
- [ ] Tooling: `cargo fmt`, `cargo clippy -- -D warnings`, `rust-toolchain.toml` pinning stable.

## Step 2 — Entry point

- [ ] clap definition: global flags (`-v`, `--config-dir`, `--json`, `--no-color`, `--yes`) and an **optional** subcommand.
- [ ] No subcommand → start the TUI. Subcommand → CLI handler (placeholders for now).
- [ ] `paths.rs`: config / data / state / cache directories. Order: `--config-dir` → `VCON_CONFIG_DIR` → `XDG_*` → `~/.config/vcon` etc.
- [ ] Logging to `~/.local/state/vcon/vcon.log` via `tracing-appender` (stderr is unusable once the TUI owns the screen). `-v`/`-vv` or `VCON_LOG` set the level.

## Step 3 — Terminal lifecycle (`tui/terminal.rs`)

- [ ] `setup()`: raw mode, alternate screen, bracketed paste, hide cursor.
- [ ] `restore()`: undo everything; safe to call twice.
- [ ] Panic hook that calls `restore()` **before** the default hook prints the panic.
- [ ] `suspend()` / `resume()` helpers (used in phase 2 for Enter-to-connect, but build and test them now by suspending to run `$SHELL` from a debug key).
- [ ] Check stdout is a TTY before starting; if not, print a helpful message ("vcon's TUI needs a terminal; use subcommands for scripting").

## Step 4 — Event loop (`tui/event.rs`, `tui/app.rs`, `tui/action.rs`)

Follow [06-tui-design.md → Internal architecture](../06-tui-design.md#internal-architecture):

- [ ] `Action` enum (start with `Tick`, `Render`, `Resize`, `Quit`, `SwitchScreen(n)`, `ToggleHelp`, `Toast(String)`).
- [ ] An action channel (`tokio::sync::mpsc`) whose sender can be cloned into background tasks.
- [ ] Main loop with `tokio::select!` over: crossterm `EventStream`, tick interval (250 ms), render interval (from `fps`), action channel.
- [ ] `App::update(action) -> Vec<Command>` and `App::draw(frame)`; draw only when state changed ("dirty" flag).
- [ ] Component shape for screens: `handle_key`, `update`, `draw` (see design doc).
- [ ] Placeholder screens for the five tabs.

## Step 5 — Shell UI pieces

- [ ] Tab bar, content area, toast line, key hint bar (layout from the design doc).
- [ ] Modal stack with a generic centered-popup widget; the help modal is the first modal.
- [ ] `Theme` struct with defaults; respect `NO_COLOR` / `--no-color`.
- [ ] Keymap module: map `(context, key) → Action` with defaults in code. Loading overrides from config can wait until phase 1.

## Step 6 — First tests

- [ ] clap definition test (`Command::debug_assert()`).
- [ ] `update` test: `SwitchScreen(2)` changes the active screen; `ToggleHelp` pushes/pops the modal.
- [ ] Render snapshot of the empty layout using ratatui `TestBackend` + `insta`.

## Done when

- `vcon` opens instantly, tabs switch, `?` shows help, `q` quits
- A deliberate `panic!` behind a debug key leaves your terminal fully usable
- Resizing the terminal redraws correctly
- `vcon -vv` writes debug lines to the log file
- `cargo clippy -- -D warnings` passes

## Pitfalls

- Never `println!` while the TUI is running — it corrupts the screen. Log instead.
- Don't block inside the loop (no `std::thread::sleep`, no synchronous network or file-heavy work).
- ratatui's official templates (`cargo generate ratatui/templates`, "component" / "event-driven-async") are a good reference for this exact structure — read them, but build your own so you understand every piece.
