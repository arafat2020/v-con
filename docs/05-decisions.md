# 05 — Design Decisions

Short records of the important choices. When you change your mind later, add a
new entry rather than rewriting history — future you will want the "why".

---

## D1: Drive system OpenSSH instead of a native SSH library

**Decision:** v1 spawns `ssh`, `scp`, and `rsync` processes. A native backend
(`russh`) is a future option behind the same `SshBackend` trait.

**Why:**
- You get everything OpenSSH already supports for free: `~/.ssh/config`, ssh-agent, hardware keys, `known_hosts`, ProxyJump, ControlMaster, all ciphers.
- Interactive sessions (full PTY, resize, vim, htop) work perfectly because it *is* ssh.
- Much less code and much less security-sensitive code.

**Trade-offs:** Spawning a process per host per command costs some speed —
largely solved by ControlMaster multiplexing. Error messages come from ssh's
stderr and need light parsing to classify (auth failed, timeout, host key
changed).

---

## D2: TOML for the inventory, not SQLite

**Why:** Human-editable, diff-able, git-friendly, supports comments. 200 hosts
is tiny; loading the whole file is instant.

**Trade-off:** Concurrent writes from two `vcon` processes could clash.
Mitigation: atomic rename on write; optionally a lock file. Machine-only data
(recents, health cache) lives in a separate state file so vcon rarely rewrites
`hosts.toml`.

---

## D3: Use `toml_edit` for writes

**Why:** Plain `serde` + `toml` serialization throws away comments and
reorders things. Since users edit the file by hand, `vcon add` must not wreck
their formatting. Read with `serde` for convenience; modify with `toml_edit`.

---

## D4: File locations

**Decision:** Use `~/.config/vcon` (XDG style) on both macOS and Linux, not
`~/Library/Application Support`.

**Why:** Developer tools conventionally live in `~/.config` on macOS too; it is
easier to find, back up, and put in a dotfiles repo. Respect `XDG_CONFIG_HOME`
if set, and `VCON_CONFIG_DIR` overrides everything.

---

## D5: CLI first, TUI later — SUPERSEDED by D11

**Why (original reasoning):** The CLI forces clean services with no UI logic in them. The TUI then
becomes a second consumer of the same services. Building the TUI first tends
to tangle state, rendering, and SSH logic together.

---

## D6: Replace the process for CLI `connect`

**Decision:** `vcon connect` (the CLI subcommand) replaces itself with `ssh`
(Unix `exec`). Enter-to-connect inside the TUI cannot do this, because it has
to come back — it suspends the TUI and spawns ssh as a child instead (see
[06-tui-design.md](06-tui-design.md#terminal-lifecycle)).

**Why:** Exact `ssh` behaviour: signals, window resizing, exit codes, no extra
process sitting in between. The downside is vcon cannot do anything *after*
the session — so record "last connected" **before** exec'ing.

---

## D7: Never store passwords

**Why:** Passwords in a config file are a liability. Key-based auth +
ssh-agent is the standard. All non-interactive operations use
`BatchMode=yes`, so a host that wants a password fails fast instead of
hanging a parallel run. Optional OS keychain support (`keyring`) for key
passphrases is a phase 8 stretch goal.

---

## D8: Safe defaults for multi-host actions

- Confirm before running on more than `confirm_threshold` hosts or on any `protected_tags`.
- `--dry-run` is available on every multi-host command.
- Never pass `StrictHostKeyChecking=no` by default. A changed host key is shown as a clear, specific error.
- An empty selector result is an error.

---

## D9: Tokio for concurrency

**Why:** Running and waiting on many child processes with timeouts, streaming
their output, and limiting concurrency with a semaphore is exactly what
`tokio::process` + `tokio::sync::Semaphore` + `tokio::time::timeout` are for.
Threads would work too, but async makes timeouts and streaming simpler.

Keep tokio out of the domain layer. Only services and infrastructure use it.

---

## D10: Single crate to start

**Why:** Fewer moving parts while the design settles. Module boundaries
(domain / services / infra / interface) give most of the benefit of separate
crates. Split into a workspace (e.g. `vcon-core` + `vcon-tui`) only if compile
times or reuse demand it.

---

## D11: TUI-first (supersedes D5)

**Decision:** The ratatui TUI is the primary interface and is built from phase
0. CLI subcommands are secondary, added per phase for scripting.

**Why:** The main job is *navigating* a fleet: browsing, searching, jumping
between hosts, watching health. That is a visual, stateful workflow a TUI does
far better than one-shot commands.

**How we keep D5's benefit:** the rule "services know nothing about ratatui"
stays. Services expose async functions + channels; the TUI is just a consumer.
Writing the matching CLI subcommand for each feature is a cheap, useful check
that the logic really lives in services and not in screens.

---

## D12: Embedded sessions: PTY + vt100 parser

**Decision:** Embedded SSH sessions spawn **system `ssh -t` inside a
pseudo-terminal** (`portable-pty`), parse the output with the `vt100` crate
(an in-memory terminal emulator), and render the resulting screen with
`tui-term`.

**Why:**
- Same auth path as everything else (system ssh, agent, config, jump hosts).
- `vt100` handles escape sequences, colours, cursor movement, alternate screen (vim/htop), scrollback — we don't write a terminal emulator.

**Trade-offs:**
- It is an emulator inside an emulator: rare escape sequences, some mouse modes, images (sixel/kitty graphics), and exotic Unicode may render imperfectly. That's acceptable; Enter-to-connect (full handoff) remains the "perfect fidelity" option.
- We must translate crossterm key events back into terminal byte sequences ourselves.
- Alternatives considered: `alacritty_terminal` / `wezterm-term` (more complete, heavier APIs). Revisit if `vt100` falls short.

---

## D13: File browser uses SFTP over system ssh

**Decision:** The `RemoteFs` trait is implemented with an **SFTP client that
talks the SFTP protocol over a `ssh -s sftp` subprocess** (the
`openssh-sftp-client` crate supports this model).

**Why:**
- Structured directory listings (names, sizes, permissions, mtimes) — no fragile parsing of `ls` output.
- Byte-level reads/writes give accurate progress bars and cancellation.
- Still uses system ssh for authentication, keeping D1's benefits.

**Fallbacks / alternatives:**
- If the SFTP crate is a problem, list directories by running a `find ... -printf` script over exec (GNU systems; Alpine/busybox needs a fallback) and transfer with rsync/scp.
- A native `russh` + `russh-sftp` backend is a future option behind the same trait.
- rsync stays available for "sync this directory" actions where delta transfer matters.

---

## D14: Health by polling a script, not an agent

**Decision:** Health metrics come from running a small POSIX shell script over
ssh every N seconds (default 30) per host, reusing ControlMaster connections.
CPU % and network rates are computed **locally** as deltas between two polls.

**Why:** Nothing to install on servers. ControlMaster makes each poll cheap.
Delta-between-polls avoids `sleep 1` inside the script, so each poll returns
immediately.

**Trade-off:** Not real-time. A future option: one long-lived ssh per host
running a loop that prints a sample every few seconds (streaming).
