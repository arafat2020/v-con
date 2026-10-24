# vcon — VPS Control

`vcon` is a **terminal UI (TUI) application** built with
[ratatui](https://ratatui.rs) for managing a fleet of VPS servers from one
place: browse and search your servers, jump into any of them with Enter, keep
several SSH sessions open in tabs, run commands across many at once, browse and
transfer files, and watch live health metrics.

Run `vcon` and you are in the dashboard. A small set of CLI subcommands exists
alongside it for scripting (`vcon exec`, `vcon list --json`, …), sharing the
same core.

This folder is the build plan. There is no code here, only the design and the
steps to build it yourself.

## How to read these docs

| # | Document | What it answers |
|---|----------|-----------------|
| 1 | [01-requirements.md](01-requirements.md) | What problem are we solving and what is in or out of scope? |
| 2 | [02-architecture.md](02-architecture.md) | How is the program structured, and why? |
| 3 | [03-data-model.md](03-data-model.md) | What does a host look like on disk, and how do we select hosts? |
| 4 | [04-cli-reference.md](04-cli-reference.md) | Which CLI commands exist for scripting? |
| 5 | [05-decisions.md](05-decisions.md) | The key design choices and the reasoning behind them |
| 6 | [06-tui-design.md](06-tui-design.md) | Screens, layouts, keybindings, and the TUI's internal architecture |
| 7 | [phases/](phases/) | The step-by-step build plan |

## Build phases

Each phase ends with something you can actually use. Do them in order.

| Phase | Name | You can now... |
|-------|------|----------------|
| 0 | [Foundation and TUI skeleton](phases/phase-0-foundation.md) | Launch an empty ratatui app with tabs, an event loop, and safe terminal handling |
| 1 | [Inventory and host list](phases/phase-1-inventory-and-host-list.md) | Browse, search, and filter your hosts in the TUI |
| 2 | [Connect](phases/phase-2-connect.md) | Press Enter on a host to get a shell, exit, and land back in the TUI |
| 3 | [Host forms](phases/phase-3-host-forms.md) | Add, edit, duplicate, delete, and import hosts from inside the TUI |
| 4 | [Remote exec](phases/phase-4-remote-exec.md) | Select hosts, run a command on all of them, watch results live |
| 5 | [Health dashboard](phases/phase-5-health-dashboard.md) | Live CPU, memory, disk, load, and network with auto-refresh and sparklines |
| 6 | [File browser](phases/phase-6-file-browser.md) | Two-pane local ↔ remote file browser with a transfer queue |
| 7 | [Embedded sessions](phases/phase-7-embedded-sessions.md) | Several live SSH sessions in tabs and split panes inside vcon |
| 8 | [Snippets, tunnels, audit](phases/phase-8-snippets-tunnels-audit.md) | Saved runbooks, port forwards, and an audit log |
| 9 | [Hardening and release](phases/phase-9-hardening-and-release.md) | Tests, CI, completions, installable binaries |

**Milestone 1 = phases 0–3.** At that point vcon replaces typing `ssh` by hand.
Use it daily before going further; real use will tell you what matters most
in phases 4–9.

Phase 7 (embedded terminals) is the hardest one by far. It is deliberately
late: by then the TUI architecture is proven, and Enter-to-connect from phase 2
already covers the everyday need.

## Future ideas (after phase 9)

- Native SSH backend using `russh` instead of driving system `ssh`
- Sync inventory from cloud provider APIs (Hetzner, DigitalOcean, AWS, Vultr)
- Shared team inventory (a git repo of host files)
- Per-host bookmarks: common directories, log paths, service names
- Broadcast mode: type once, send keystrokes to several embedded sessions
- Long-lived streaming health agents instead of polling
