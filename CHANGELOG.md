# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — 2026-10-29

First usable release spanning foundation through hardening.

### Added

- **Phase 0 — Foundation:** crate layout, paths, errors, logging, empty TUI shell.
- **Phase 1 — Inventory:** `hosts.toml` / `config.toml`, selectors, Hosts screen, `init` / `list` / `check`.
- **Phase 2 — Connect:** interactive SSH, multiplexing control sockets, state/recents/favorites.
- **Phase 3 — Host forms:** add/edit/delete/import from `~/.ssh/config`, comment-preserving writes.
- **Phase 4 — Remote exec:** parallel `exec` in TUI and CLI with streaming/grouped output.
- **Phase 5 — Health:** dashboard, one-shot `status` / `ping`, thresholds and history.
- **Phase 6 — Files:** dual-pane browser, SFTP, `push` / `pull`.
- **Phase 7 — Sessions:** embedded PTY sessions with prefix key and scrollback.
- **Phase 8 — Snippets, tunnels, audit:** templates, port forwards, `vcon log` / snippet / tunnel CLI.
- **Phase 9 — Hardening:** `vcon doctor`, shell completions, man page, secure file modes, inventory write lock, keymap-driven help, CI, CLI and property tests.

### Security

- Config/state/audit/snippet files created `0600`; directories `0700` on unix.
- Inventory writes take an exclusive file lock (`fs4`) to avoid clobbering `hosts.toml`.
