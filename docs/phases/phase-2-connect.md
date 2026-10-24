# Phase 2 — Connect

**Goal:** Press Enter on a host and get a real SSH session; exit and land back
in vcon exactly where you were.

**You can now:** connect from the TUI, connect through bastion hosts, see
recents and favorites in the tag tree. CLI: `vcon connect web-1`.

**Estimated effort:** 2–3 days

## Step 1 — `SshBackend` trait and `SystemSsh`

- [ ] Define the trait (see [architecture](../02-architecture.md#sshbackend-commands-and-interactive-handoff)). Implement `interactive` now; stub the rest.
- [ ] Locate `ssh` on PATH once at startup; show a clear error modal if missing.
- [ ] Log every ssh command line at debug level.

## Step 2 — Jump hosts

- [ ] A `jump` that names another host → resolve it and render `user@address:port` for `-J`.
- [ ] Chains (`db-1 → bastion-2 → bastion-1`) → `-J a,b`.
- [ ] `-J` does not pass the jump host's identity file. Start by relying on ssh-agent (document it); later, optionally generate a `ProxyCommand` using `ssh -W` with the right key.

## Step 3 — Enter-to-connect (terminal handoff)

Flow (see [connect sequence](../02-architecture.md#connect-terminal-handoff)):

- [ ] `update` handles `Connect(host)` by returning a `Suspend(ConnectHandoff(host))` command.
- [ ] The loop executes it: stop the input reader → `suspend()` the terminal → spawn ssh with **inherited** stdin/stdout/stderr → wait → `resume()` → full redraw → drain stray input events → restart the input reader.
- [ ] Print a one-line banner before ssh starts (`Connecting to web-1 (deploy@203.0.113.10 via bastion)…`) so the user sees something during the handshake.
- [ ] If ssh exits with 255 (connection/auth failure), show an **error modal** with ssh's stderr summary when you return. Capture it by teeing stderr — or simpler: rely on the user having seen it, and show "ssh exited with code 255" as a toast.
- [ ] Update `last_connected` / `connect_count` in the state store.

## Step 4 — State store

- [ ] `StateStore` for `state.json` with a `version` field. Missing/corrupt → start fresh, log a warning, never block connecting.
- [ ] Recents and connect counts; favorites (`*` toggles).
- [ ] Frecency score for an optional "sort by frecency" mode in the host table.
- [ ] Fill the `recent` and `favorites` nodes in the tag tree.
- [ ] Save UI state (last screen, last filter) on quit; restore on start if `tui.start_screen = "last"`.

## Step 5 — `$EDITOR` handoff

- [ ] `E` on the Hosts screen opens `hosts.toml` in `$EDITOR` using the same suspend/resume mechanism, then reloads and re-validates. This gives you full host editing before the forms in phase 3.

## Step 6 — CLI

- [ ] `vcon connect <name>`: resolve → record state → replace the process with ssh (Unix `CommandExt::exec`, see [D6](../05-decisions.md#d6-replace-the-process-for-cli-connect)).
- [ ] Unknown name → "did you mean?" suggestions.

## Done when

- Enter → shell on the server → `exit` → back in vcon with the same cursor, filter, and selection
- vim/htop work perfectly inside the session (because it's real ssh with the real terminal)
- A host behind a bastion connects with a single Enter
- `recent` shows what you just used

## Pitfalls

- Forgetting to stop the crossterm event reader before handing over the terminal: it will steal keystrokes from ssh. Make the reader task pausable or cancel and recreate it.
- Forgetting to drain input after resuming leaks keys typed during the session into vcon.
- Don't pipe stdio for interactive sessions — always inherit.
