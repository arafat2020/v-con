# 01 — Requirements

## The problem

Working with many VPS servers means:

- Remembering IPs, users, ports, and which SSH key goes where
- Typing long `ssh -i ... -p ... user@ip` commands, or maintaining a huge `~/.ssh/config` by hand
- Juggling many terminal windows/tabs, one per server
- Repeating the same command (update packages, check disk, restart a service) on server after server
- No quick overview of which servers are up, full, or overloaded
- Moving files around with `scp` commands you have to look up every time
- No record of what was run where

## Goal

A single fast, keyboard-driven **terminal dashboard** where you can **find any
server in a second, get a shell on it, act on many servers at once, and see
their health** — without giving up the standard OpenSSH tooling you already
trust.

## Users

One primary user: a DevOps/software engineer on macOS or Linux, managing
roughly 5–200 Linux VPS servers across one or more providers.

## Functional requirements

### Must have (milestone 1: phases 0–3)

- **F1 TUI home** — running `vcon` opens a full-screen ratatui dashboard.
- **F2 Inventory** — store hosts with name, address, user, port, identity file, jump host, tags, and notes.
- **F3 Host navigation** — host list with live fuzzy search, a tag/group tree, a detail pane, and sorting.
- **F4 Connect** — Enter on a host opens an interactive SSH session; exiting returns to the TUI where you left off.
- **F5 Host forms** — add, edit, duplicate, and delete hosts through validated forms inside the TUI.
- **F6 Import** — import existing hosts from `~/.ssh/config` via a preview screen.
- **F7 Selection** — target many hosts using multi-select in the TUI, or selectors (`@prod`, `web-*`, `!web-3`; see [03-data-model.md](03-data-model.md#host-selectors)).

### Should have (phases 4–6)

- **F8 Parallel exec** — run one command on many hosts concurrently; live per-host status and output in an exec panel.
- **F9 Health dashboard** — live CPU, memory, disk, load, network, uptime, OS per host; auto-refresh; history sparklines; warning/critical highlighting.
- **F10 File browser** — two-pane local ↔ remote browser: navigate, copy both ways, rename, delete, mkdir, with a transfer queue showing progress.

### Should have (phase 7)

- **F11 Embedded sessions** — multiple live SSH sessions inside vcon as tabs and split panes, with a prefix key to switch between them and the rest of the TUI.

### Nice to have (phase 8+)

- **F12 Snippets** — saved commands/runbooks with variables, runnable from the TUI.
- **F13 Tunnels** — saved port forwards per host, started/stopped from the TUI.
- **F14 Audit log** — record every exec/transfer: when, which hosts, what command, result; viewable in a Log tab.
- **F15 CLI for scripting** — non-interactive subcommands with `--json` output.

## Non-functional requirements

| ID | Requirement |
|----|-------------|
| N1 | **Fast**: TUI visible in under 200 ms; search stays instant with 200+ hosts; UI never freezes waiting on the network. |
| N2 | **Safe by default**: never store passwords in plain text; never disable host key checking silently; confirm before acting on many hosts or `prod`-tagged hosts. |
| N3 | **Never break the terminal**: on crash, panic, or Ctrl+C, the user's terminal is always restored. |
| N4 | **Plays well with OpenSSH**: respect `~/.ssh/config`, ssh-agent, `known_hosts`, ProxyJump. |
| N5 | **Human-editable data**: the inventory is a plain text file you can edit, diff, and keep in git. |
| N6 | **Keyboard first**: every action reachable from the keyboard; mouse support is a bonus. Keybindings configurable. |
| N7 | **Works in a normal terminal**: usable at 80×24, works in iTerm2, Terminal.app, Alacritty, WezTerm, Kitty, and inside tmux. Respects `NO_COLOR`. |
| N8 | **Single binary**, no runtime dependencies beyond system `ssh` (and optionally `rsync`). |
| N9 | **Portable**: macOS and Linux. Windows is out of scope. |

## Out of scope

- Provisioning servers (Terraform/Pulumi's job)
- Configuration management (Ansible's job — vcon is for interactive, ad-hoc work)
- A web UI or a server/daemon component
- Managing Windows servers
- Storing passwords (use keys + ssh-agent)
- Being a full tmux replacement (embedded sessions cover the common case, not every tmux feature)

## Success criteria

You open vcon in the morning and keep it open: you stop typing raw `ssh`
commands, "which servers are low on disk?" is one glance at the Health tab,
and copying a config to five servers is a few keystrokes.
