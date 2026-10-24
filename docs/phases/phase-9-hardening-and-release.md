# Phase 9 — Hardening and Release

**Goal:** Make vcon reliable, well tested, and easy to install on any machine.

**Estimated effort:** 3–5 days

## Testing

- [ ] Fill domain unit-test gaps: selectors, resolver, command builder, health parser + delta math, templating, key encoding.
- [ ] TUI `update` tests for the main flows: search → connect command; select → exec → confirm; add host form validation; prefix key routing.
- [ ] Render snapshots (`TestBackend` + `insta`) for every screen and modal, at 80×24 and a large size.
- [ ] CLI tests (`assert_cmd`) for every subcommand: output, `--json` shape, exit codes, with `VCON_CONFIG_DIR` in a temp dir.
- [ ] **Integration tests with real sshd** in Docker: 2–3 `openssh-server` containers, one as bastion, a generated test key. Cover exec (success, failure, timeout, one container stopped), SFTP list/upload/download, health parsing on a real system, a PTY session that runs `echo` and checks the parsed screen, a tunnel.
- [ ] Mark integration tests `#[ignore]` locally (`cargo test -- --ignored`), always run in CI.
- [ ] Property tests (`proptest`): selector parser, template engine, key encoder never panic.

## Robustness

- [ ] Corrupt `hosts.toml` / `state.json`, missing `ssh`, read-only config dir, tiny terminal (40×10), terminal resized mid-modal — clear messages, never a panic, terminal always restored.
- [ ] No `unwrap()`/`expect()` on runtime-fallible paths (`clippy::unwrap_used`).
- [ ] File lock (`fs4`) around inventory writes.
- [ ] No orphan processes after quit, crash, or kill: check `ps` after each scenario (exec jobs, health polls, sftp, PTY sessions, tunnels).
- [ ] Performance: startup under 200 ms with 200 hosts; CPU near idle when nothing changes (no busy redraw loop); smooth while a session streams heavy output.

## Security review

- [ ] No passwords stored; no secrets in logs.
- [ ] Host key checking never disabled by default; "host key changed" is a loud, specific error.
- [ ] Snippet quoting has injection tests.
- [ ] Config/state/log files created `0600`, directories `0700`.
- [ ] Audit log redaction works.

## Developer experience

- [ ] `vcon doctor`: ssh/rsync present, config valid, ssh-agent has keys, control socket dir writable, terminal capabilities (colours, size).
- [ ] Shell completions (`clap_complete`), bonus: dynamic host-name completion.
- [ ] Man page (`clap_mangen`).
- [ ] In-app help (`?`) generated from the keymap so it's never out of date.
- [ ] A short demo GIF (e.g. `vhs` by Charm) for the README.

## CI

- [ ] `fmt --check`, `clippy -D warnings`, tests on macOS + Linux, Docker integration tests on Linux.
- [ ] `cargo deny` / `cargo audit`.

## Release

- [ ] `CHANGELOG.md`, semantic versioning.
- [ ] Release profile: `lto = true`, `codegen-units = 1`, `strip = true`.
- [ ] Binaries for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` via `cargo-dist` (GitHub Releases + shell installer + optional Homebrew tap).

## Done when

- CI is green on macOS and Linux, including real-ssh integration tests
- A fresh machine goes from zero to a working vcon (with your copied `~/.config/vcon`) in one install command
