# 06 — TUI Design

The TUI is vcon's main interface. This document describes what it looks like,
how you move around it, and how it is structured internally.

## Overall layout

```
┌ vcon ──[1 Hosts]──[2 Health]──[3 Files]──[4 Sessions (2)]──[5 Log]───── ⚠ 1 ┐
│                                                                             │
│                         active screen content                               │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│ ✔ Saved web-1                                                     (toast)   │
└ ⏎ connect  / search  a add  e edit  x exec  space select  ? help  q quit ───┘
```

- **Tab bar** (top): switch screens with `1`–`5`, or `Tab` / `Shift+Tab`. The Sessions tab shows how many embedded sessions are open. A right-side indicator shows hosts in warning/critical state.
- **Content area**: the active screen.
- **Toast line**: short messages that disappear after a few seconds.
- **Key hint bar** (bottom): context-sensitive — always shows the keys that work *right now*.
- **Modals** (centered popups) for forms, confirmations, command input, help.

Must remain usable at **80×24**: panes collapse (e.g. detail pane hides) on
narrow terminals rather than overflowing.

## Screens

### 1. Hosts (home)

```
┌ Tags ──────────┐┌ Hosts ─ 23 ─ /prod ng ───────────────────────────────────┐
│ ▾ all (23)     ││   NAME       ADDRESS         TAGS          UP  LAST      │
│   ▸ prod (9)   ││ ● web-1      203.0.113.10    web,prod      ✔   2m        │
│   ▸ staging (4)││ > web-2      203.0.113.11    web,prod      ✔   1d        │
│   ▸ db (3)     ││ ● db-1       10.0.0.20       db,prod       ⚠   3d        │
│   ▸ hetzner(12)││   cache-1    10.0.0.30       cache,prod    ✖   —         │
│   recent       ││                                                          │
├────────────────┘└──────────────────────────────────────────────────────────┤
│ web-2  deploy@203.0.113.11:22 via bastion   Ubuntu 24.04   up 41d          │
│ Notes: Secondary nginx. Tunnels: grafana (L:3000)                          │
└────────────────────────────────────────────────────────────────────────────┘
```

- Left: **tag tree** (`tui-tree-widget`) — `all`, each tag, plus virtual groups `recent` and `favorites`. Selecting a node filters the list.
- Center: **host table**. `●` = multi-selected, `>` = cursor. Health column from cache.
- Bottom: **detail pane** for the cursor host.
- `/` opens search: fuzzy over name, address, tags, notes. Input starting with `@`, `!`, or containing `*` is treated as a **selector** instead.

### 2. Health

Fleet-wide live metrics table, auto-refreshing. Each row: host, status,
CPU % with sparkline, load, memory gauge, disk gauge, network rx/tx, uptime.
Enter opens a **host health detail** view with larger charts (CPU, memory,
network over time). `p` pauses refresh, `s` cycles sort column.

### 3. Files

Two panes: **local** (left) and **remote** (right) for a chosen host, plus a
collapsible **transfer queue** at the bottom with per-file progress.

### 4. Sessions

Embedded SSH sessions as sub-tabs, optionally split into panes. While a
session pane has focus, keys go to the remote shell; a **prefix key** returns
control to vcon (see [Session focus](#session-focus-and-the-prefix-key)).

### 5. Log

The audit log (phase 8): filterable table of past exec/transfer actions,
Enter shows full details.

### Modals

| Modal | Opened by |
|-------|-----------|
| Host form (add/edit/duplicate) | `a`, `e`, `D` on Hosts |
| Import from `~/.ssh/config` | `i` on Hosts |
| Confirm (yes/no, optionally "type the host count to confirm" for big prod runs) | destructive or wide actions |
| Command input (with history) | `x` |
| Exec results | after starting an exec job; reopen with `X` |
| Snippet picker | `s` |
| Help (all keybindings for the current context) | `?` |
| Error details | Enter on an error toast |

## Default keymap

Global (when not typing into an input and not inside a session pane):

| Key | Action |
|-----|--------|
| `1`–`5`, `Tab`, `Shift+Tab` | switch screen |
| `?` | help |
| `q` | quit (asks if sessions/transfers are active) |
| `Ctrl+C` | quit immediately (still cleans up children) |
| `:` | command palette (stretch: type actions by name) |

Hosts screen:

| Key | Action |
|-----|--------|
| `↑↓` / `j k`, `g G`, `PgUp PgDn` | move |
| `←→` / `h l` | focus tag tree ↔ host table |
| `/` | search / selector filter; `Esc` clears |
| `Enter` | connect (terminal handoff) |
| `o` | open in embedded session (phase 7) |
| `Space` | toggle multi-select; `Ctrl+A` select all visible; `Esc` clears selection |
| `a` / `e` / `D` / `d` | add / edit / duplicate / delete host |
| `i` | import from ssh config |
| `x` | exec on selected hosts (or cursor host) |
| `f` | open Files screen for cursor host |
| `s` | snippet picker |
| `t` | tunnels for cursor host |
| `y` | copy ssh command to clipboard |
| `*` | toggle favorite |
| `E` | open `hosts.toml` in `$EDITOR` |

Keybindings are configurable in `config.toml` (see [03-data-model.md](03-data-model.md#settings-configvconconfigtoml)).

## Session focus and the prefix key

Inside an embedded session, *every* key must go to the remote shell (vim,
htop, Ctrl+R in bash…). To get back to vcon, use a **prefix key**, like tmux:

- Default prefix: `Ctrl+]` (rarely used by shells; also the classic telnet escape). Configurable.
- `prefix` then:
  - `n` / `p` — next / previous session
  - `1`–`9` — jump to session
  - `|` / `-` — split vertically / horizontally
  - arrow keys — move focus between panes
  - `x` — close session (with confirm)
  - `[` — scrollback mode (arrows/PgUp to scroll, `Esc` to leave)
  - `d` — detach focus: back to normal vcon navigation (session keeps running)
  - `prefix` twice — send the prefix key itself to the remote

The focused pane has a highlighted border; the key hint bar shows
`SESSION  web-1  —  Ctrl+] for vcon`.

## Internal architecture

### The loop

One async task owns all UI state:

```
loop {
    select! {
        input event      => handle key/mouse/resize/paste → maybe Action
        tick (every 250ms) => Action::Tick   (animations, spinners, toast expiry)
        render timer (~30–60 fps, only if dirty) => draw
        background msg   => Action from services (health, exec, sftp, pty)
    }
    while let Some(action) = queue.pop() {
        commands = app.update(action)      // mutate state; no I/O
        for cmd in commands { spawn(cmd) } // I/O happens here, results come back as Actions
    }
}
```

- **Render only when dirty.** Embedded sessions can produce thousands of output chunks per second; coalesce them into one redraw per frame.
- `update` returns **commands** (e.g. `StartExec { hosts, cmd }`, `RefreshHealth(hosts)`, `ListRemoteDir { host, path }`, `Suspend(ConnectHandoff(host))`) instead of performing side effects. Unit tests call `update` directly and assert on state and returned commands.

### Actions

A single `Action` enum carries everything that can change state. Rough groups:

- **Input-derived**: `MoveUp`, `Select`, `OpenSearch`, `SubmitForm`, `SwitchScreen(n)`, `Quit`, …
- **From services**: `InventoryReloaded`, `HealthUpdated { host, report }`, `ExecStarted/Output/Finished`, `RemoteDirListed`, `TransferProgress`, `SessionOutput(id)`, `SessionExited(id)`, `Error(..)`.
- **Housekeeping**: `Tick`, `Resize`, `Toast(msg)`.

### Components

Each screen and reusable widget follows the same shape:

- **handle_key(key) → Option<Action>** — translate input in this context into an action (via the keymap).
- **update(action) → Vec<Command>** — change own state.
- **draw(frame, area)** — render; no logic, no I/O.

The App holds: the active screen, a **modal stack** (top modal gets input first),
a **focus** marker within the screen, and shared state (inventory, health
cache, sessions, selection).

Input routing order: **session pane (if focused, except prefix)** → **top modal**
→ **text input with focus** → **screen** → **global keys**.

### Terminal lifecycle

- **Setup**: enable raw mode, enter alternate screen, enable bracketed paste (and mouse capture if enabled), hide cursor.
- **Teardown**: reverse all of it. Must run on normal exit, on error, **and on panic** (install a panic hook that restores the terminal before printing the panic).
- **Suspend/resume** (for Enter-to-connect and `$EDITOR`): stop reading input events, teardown, run the child process with the real terminal, wait, setup again, force a full redraw, then drain any stray input events.
- **Ctrl+Z** (stretch): suspend vcon to the shell properly (teardown, raise SIGTSTP, setup on SIGCONT).

### Theme

Colors defined in one `Theme` struct (border, highlight, ok/warn/critical,
muted text), loadable from `config.toml`. With `NO_COLOR`, fall back to
bold/reverse/underline only — status must still be readable via symbols
(✔ ⚠ ✖), never color alone.

## UX rules

1. **Never freeze.** Anything that touches the network runs in the background and shows a spinner.
2. **Always show what keys do** in the hint bar.
3. **Destructive and wide actions confirm**, and the confirm dialog says exactly what will happen ("Run `apt upgrade -y` on 9 hosts including prod?").
4. **Preserve context**: returning from ssh/editor/modal puts the cursor, filter, and selection back where they were. Save last screen + filter in the state file so the next launch resumes.
5. **Errors are specific**: "auth failed for deploy@web-1 (key ~/.ssh/id_ed25519)" beats "ssh exited with 255".
