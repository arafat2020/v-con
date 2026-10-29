# vcon — User Manual

`vcon` is a keyboard-driven terminal dashboard for managing a fleet of Linux
VPS hosts. Browse and search your inventory, open SSH shells, run the same
command on many servers at once, watch live health, browse files over SFTP,
keep embedded sessions in tabs, start port forwards, and keep an audit trail —
all while using the system `ssh` you already trust (agent, `~/.ssh/config`,
ProxyJump, ControlMaster).

Running `vcon` with no arguments opens the TUI. Subcommands exist for scripting
and one-offs (`vcon exec`, `vcon list --json`, …) and share the same core.

| | |
|--|--|
| Platforms | macOS and Linux |
| Requirements | System `ssh` on `PATH` (optional `rsync` for faster transfers) |
| Config | `~/.config/vcon/` (or `VCON_CONFIG_DIR` / `--config-dir`) |

---

## Table of contents

1. [Install and first run](#1-install-and-first-run)
2. [Where files live](#2-where-files-live)
3. [Inventory (`hosts.toml`)](#3-inventory-hoststoml)
4. [Host selectors](#4-host-selectors)
5. [The TUI](#5-the-tui)
6. [Hosts screen](#6-hosts-screen)
7. [Health](#7-health)
8. [Files](#8-files)
9. [Embedded sessions](#9-embedded-sessions)
10. [Snippets](#10-snippets)
11. [Tunnels](#11-tunnels)
12. [Audit log](#12-audit-log)
13. [CLI reference](#13-cli-reference)
14. [Settings (`config.toml`)](#14-settings-configtoml)
15. [Doctor, completions, and man page](#15-doctor-completions-and-man-page)
16. [Security and safety](#16-security-and-safety)
17. [Troubleshooting](#17-troubleshooting)

---

## 1. Install and first run

### Build from source

```bash
git clone <repo-url> v-con
cd v-con
cargo build --release
# binary: target/release/vcon
```

Put `target/release/vcon` somewhere on your `PATH`, or run it by path.

### First run

```bash
vcon doctor          # check ssh, config paths, agent, control dir
vcon init            # create ~/.config/vcon/hosts.toml (example host)
$EDITOR ~/.config/vcon/hosts.toml
vcon check           # validate inventory
vcon                 # open the dashboard
```

Press `?` inside the TUI for keybindings. Press `q` to quit.

---

## 2. Where files live

| Path | Purpose |
|------|---------|
| `~/.config/vcon/hosts.toml` | Inventory (edit by hand or from the TUI) |
| `~/.config/vcon/config.toml` | Settings, theme, key overrides (optional) |
| `~/.config/vcon/snippets.toml` | Saved command templates |
| `~/.local/share/vcon/state.json` | Recents, favorites, UI memory, health cache |
| `~/.local/state/vcon/audit.jsonl` | Audit log (JSON lines) |
| `~/.local/state/vcon/vcon.log` | Debug log (`-v` / `-vv`) |
| `~/.cache/vcon/cm/` | OpenSSH ControlMaster sockets |

Override the config directory with `--config-dir <path>` or `VCON_CONFIG_DIR`.
Data/state/cache still follow XDG (`XDG_DATA_HOME`, `XDG_STATE_HOME`,
`XDG_CACHE_HOME`) unless those variables are set.

On Unix, directories are created mode `0700` and sensitive files `0600`.

---

## 3. Inventory (`hosts.toml`)

Plain TOML you can edit, diff, and keep in git. Comments and key order are
preserved when vcon adds or edits hosts from the TUI.

```toml
[defaults]
user = "root"
port = 22
identity_file = "~/.ssh/id_ed25519"
connect_timeout = 5

[[hosts]]
name = "bastion"
address = "198.51.100.5"
user = "admin"
tags = ["infra"]

[[hosts]]
name = "web-1"
address = "203.0.113.10"
user = "deploy"
tags = ["web", "prod"]
jump = "bastion"
notes = "Main nginx box."

[[hosts.tunnels]]
name = "grafana"
spec = "L:3000:localhost:3000"
```

### Host fields

| Field | Required | Notes |
|-------|----------|-------|
| `name` | yes | Unique. Lowercase letters, digits, `-`, `_`, `.` |
| `address` | yes | IP or DNS name |
| `user` | no | Else `[defaults].user`, else your login name |
| `port` | no | Else defaults / 22 |
| `identity_file` | no | `~` expanded; else ssh decides |
| `jump` | no | Another host `name`, or raw `user@host:port` |
| `tags` | no | Used by `@tag` selectors |
| `notes` | no | Shown in the detail pane |
| `ssh_options` | no | Extra `-o Key=Value` map |
| `tunnels` | no | Saved forwards (`name` + `spec`) |
| `disabled` | no | Skipped by `all` / tags / globs; still reachable by exact name |

Resolution order for each field: **host → `[defaults]` → built-in**.

Validate anytime:

```bash
vcon check
```

---

## 4. Host selectors

Selectors pick hosts for `exec`, `status`, `push`, `list`, snippets, and the
TUI search box (when the query looks like a selector).

A selector is a **comma-separated** list of terms:

| Term | Meaning | Example |
|------|---------|---------|
| `name` | Exact host | `web-1` |
| `glob` | Name with `*` / `?` | `web-*` |
| `@tag` | Has tag | `@prod` |
| `@a+b` | Has **all** tags | `@web+prod` |
| `all` | Every enabled host | `all` |
| `!term` | Exclude | `!web-3`, `!@staging` |

Positive terms are unioned; negatives are subtracted. A selector of only
negatives starts from `all`.

| Selector | Result |
|----------|--------|
| `web-1` | Just that host |
| `@web,@db,!web-3` | Web and db hosts except web-3 |
| `!@staging` | Everything except staging |

Empty matches after evaluation are an **error** (never “ran on zero hosts”).
Unknown bare names suggest close matches.

---

## 5. The TUI

```
┌ vcon ──[1 Hosts]──[2 Health]──[3 Files]──[4 Sessions]──[5 Log]──── ⇄ 1 ⚠ 0 ┐
│                         active screen                                      │
├────────────────────────────────────────────────────────────────────────────┤
│ toast messages                                                             │
└ key hints for the current context ─────────────────────────────────────────┘
```

### Global keys (when not typing into a field / not inside a session)

| Key | Action |
|-----|--------|
| `1`–`5` | Switch screen (Hosts, Health, Files, Sessions, Log) |
| `?` | Help (generated from your keymap) |
| `q` | Quit (confirms if transfers or live sessions are active) |
| `Ctrl+C` | Quit immediately (still cleans up children) |

The bottom hint bar always lists keys that work **right now**. Usable at
**80×24**; the detail pane hides on small terminals.

---

## 6. Hosts screen

- **Tag tree** (left): `all`, each tag, `recent`, `favorites`. `h`/`l` or
  arrows move focus between tree and table.
- **Table**: cursor `>`, multi-select `●`. Move with `j`/`k`, `g`/`G`,
  Page Up/Down.
- **Detail pane**: effective destination, jump chain, identity, tags, notes,
  ssh command line, active tunnels.

| Key | Action |
|-----|--------|
| `/` | Search / selector filter (`Esc` clears, Enter keeps filter) |
| `Enter` | Interactive SSH (full terminal handoff; exit returns to vcon) |
| `o` | Open an **embedded** session (Sessions tab) |
| `Space` | Toggle multi-select |
| `Ctrl+A` | Select all visible |
| `Esc` | Clear selection |
| `a` / `e` / `D` / `d` | Add / edit / duplicate / delete |
| `i` | Import from `~/.ssh/config` |
| `x` | Exec on selection (or cursor host) |
| `X` | Reopen last exec results |
| `f` | Files for cursor host |
| `s` | Snippet picker |
| `t` | Tunnels for cursor host |
| `y` | Copy ssh command to clipboard |
| `*` | Toggle favorite |
| `S` | Sort by frecency vs inventory order |
| `E` | Open `hosts.toml` in `$EDITOR` / `$VISUAL` |
| `v` | Validation problems |

Override Hosts bindings in `config.toml` under `[keys.hosts]` (see
[Settings](#14-settings-configtoml)).

---

## 7. Health

Tab **2**: live fleet metrics (CPU, load, memory, disk, network, uptime).
Auto-refresh uses `[health]` in config. Warning/critical thresholds colour
rows; the tab bar shows warn/crit counts.

One-shot from the shell:

```bash
vcon status @prod
vcon status --json --sort cpu
vcon ping @web -t 5
```

---

## 8. Files

Tab **3**: dual-pane browser — **local** left, **remote** (SFTP over `ssh`)
right. `Tab` switches panes. Transfers continue in the background; the queue
shows progress. `f` on Hosts opens Files for the cursor host.

Typical keys (see on-screen hints): Enter open, Backspace up, Space mark,
`c` copy to other pane, `r` rename, `m` mkdir, `d` delete (confirm),
`H` pick remote host.

CLI:

```bash
vcon push @web ./deploy/ /var/www/app/
vcon pull web-1 /var/log/nginx/access.log ./logs/
```

Uses `rsync` when available, otherwise `scp`.

---

## 9. Embedded sessions

Tab **4**: keep several live SSH sessions inside vcon (PTY + terminal
emulator). Prefer this for multitasking; use **Enter** on Hosts when you want
a perfect-fidelity shell with full terminal handoff.

Open with `o` on a host (or each selected host). While a pane is focused,
**every key goes to the remote** except the prefix key.

### Prefix key (default `Ctrl+]`)

Configurable as `sessions.prefix_key` (e.g. `ctrl-]`).

| After prefix | Action |
|--------------|--------|
| `n` / `p` | Next / previous session tab |
| `1`–`9` | Jump to session |
| `\|` / `-` | Split vertical / horizontal |
| Arrows | Move focus between panes |
| `x` | Close session (confirm) |
| `[` | Scrollback mode (`Esc` to leave) |
| `d` | Detach focus — back to normal vcon navigation; sessions keep running |
| Prefix again | Send the prefix key to the remote |

Header shows `⇄ N` when tunnels are running and session counts on the Sessions
tab when sessions are open.

---

## 10. Snippets

Saved command templates in `~/.config/vcon/snippets.toml`:

```toml
[[snippets]]
name = "disk"
description = "Disk usage of root"
command = "df -h /"

[[snippets]]
name = "restart"
description = "Restart a systemd unit"
command = "sudo systemctl restart {{service}} && systemctl status {{service}} --no-pager"
confirm = true

[[snippets]]
name = "logs"
command = "sudo journalctl -u {{service}} -n {{lines|100}} --no-pager"
```

- `{{var}}` is required; `{{var|default}}` supplies a default.
- Substituted values are **shell-quoted** (safe against spaces and metacharacters).

**TUI:** `s` opens the picker → fill variables → confirm if needed → runs
through the same exec path as `x`. Manage snippets with `a` / `e` / `d` in the
picker.

**CLI:**

```bash
vcon snippet list
vcon snippet run restart @web --var service=nginx
```

---

## 11. Tunnels

Compact specs (same idea as ssh `-L` / `-R` / `-D`):

| Spec | Meaning |
|------|---------|
| `L:3000:localhost:3000` | Local 3000 → remote `localhost:3000` |
| `R:9000:localhost:9000` | Remote 9000 → your `localhost:9000` |
| `D:1080` | SOCKS on local 1080 |

Save under a host in `hosts.toml` as `[[hosts.tunnels]]` with `name` + `spec`.

**TUI:** `t` on a host — start/stop saved tunnels, or type an ad-hoc spec.
Running count appears as `⇄ N` in the header. Tunnels are killed when vcon
exits.

**CLI** (foreground; Ctrl+C stops):

```bash
vcon tunnel web-1 grafana
vcon tunnel web-1 'L:3000:localhost:3000'
```

Local/dynamic forwards fail fast if the local port is already in use.

---

## 12. Audit log

Actions such as exec, snippet runs, transfers, remote deletes, tunnel starts,
and session opens append one JSON line to `audit.jsonl` (no command **output**,
only metadata). Optional redaction regexes live under `[audit]` in config.
The file rotates near the configured size limit (default 10 MB).

**TUI:** tab **5** — filter, search, Enter for detail, `R` to re-run an exec
entry (with confirm).

**CLI:**

```bash
vcon log
vcon log --host web-1 --since 1d --failed
vcon log --action exec --json
```

`--since` accepts durations like `30m`, `12h`, `1d`.

---

## 13. CLI reference

Global flags:

| Flag | Meaning |
|------|---------|
| `-v`, `-vv` | More logging to the log file |
| `--config-dir <path>` | Alternate config directory |
| `--json` | Machine-readable output where supported |
| `--no-color` | Disable colours (also `NO_COLOR`) |
| `-y`, `--yes` | Skip confirmation prompts |

| Command | Description |
|---------|-------------|
| `vcon` | Open the TUI |
| `vcon init` | Create config dir + example `hosts.toml` |
| `vcon list [selector]` (`ls`) | Print hosts; `--json` for scripts |
| `vcon check` | Validate inventory; non-zero if problems |
| `vcon import ssh-config [--file …] [--dry-run] [--overwrite]` | Import OpenSSH config |
| `vcon connect <name>` (`c`) | Replace this process with interactive ssh |
| `vcon exec <selector> -- <cmd>` (`x`) | Parallel remote command |
| `vcon status [selector]` (`st`) | One-shot health table / `--json` |
| `vcon ping [selector]` | Latency via remote `true` |
| `vcon push <selector> <local> <remote>` | Upload |
| `vcon pull <host> <remote> <local>` | Download |
| `vcon tunnel <host> <spec\|name>` | Foreground port forward |
| `vcon snippet list` / `vcon snippet run …` | List or run snippets |
| `vcon log […]` | Print audit log |
| `vcon doctor` | Environment / config health check |
| `vcon completions <shell>` | bash / zsh / fish / powershell / elvish |
| `vcon man` | Print man page (troff) to stdout |

### `exec` flags

| Flag | Meaning |
|------|---------|
| `-p`, `--parallel <n>` | Max concurrent hosts (default from config) |
| `-t`, `--timeout <secs>` | Per-host timeout (`0` = no limit; default 30) |
| `--stream` / `--grouped` | Live lines vs grouped blocks |
| `--only-failed` | Hide successful hosts |
| `--dry-run` | Print ssh commands only |
| `--sudo` | Prefix with `sudo -n` |

Exit code **3** if any host failed.

```bash
vcon x @web --only-failed -- 'systemctl is-active nginx'
```

---

## 14. Settings (`config.toml`)

All fields optional. A missing file uses defaults.

```toml
parallel = 10
confirm_threshold = 5
protected_tags = ["prod"]
multiplexing = true

[tui]
start_screen = "hosts"    # or "last"
mouse = false
fps = 30
show_detail_pane = true

[health]
refresh_seconds = 30
history_samples = 60
warn = { disk = 80, mem = 85, load_per_cpu = 1.0 }
crit = { disk = 90, mem = 95, load_per_cpu = 2.0 }

[sessions]
prefix_key = "ctrl-]"
scrollback_lines = 5000

[audit]
max_bytes = 10485760
redact = ["-p\\S+", "password=\\S+"]

[keys.hosts]
connect = "enter"
exec = "x"
open_session = "o"
# files / open_files, open_session / session, etc.

[theme]
highlight = "cyan"
ok = "green"
warn = "yellow"
crit = "red"
```

- **`confirm_threshold`**: ask before acting on more than this many hosts.
- **`protected_tags`**: always confirm when any targeted host has these tags.
- **`multiplexing`**: ControlMaster for faster repeat connections.
- Theme colours: `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`,
  `gray`, `white`.

---

## 15. Doctor, completions, and man page

```bash
vcon doctor
# OK/WARN/FAIL for ssh, rsync, config, hosts.toml, ssh-agent,
# control socket dir, and terminal capabilities.
# Non-zero exit if any FAIL.

# Shell completions (example: zsh)
vcon completions zsh > ~/.zsh/completions/_vcon

# Man page
vcon man | man -l -
# or: vcon man > /tmp/vcon.1 && man /tmp/vcon.1
```

---

## 16. Security and safety

- Passwords are **not** stored; use keys and `ssh-agent`.
- Host key checking is never disabled by default; treat “host key changed”
  as a hard stop and fix `known_hosts` deliberately.
- Snippet variables are single-quote shell-escaped before substitution.
- Audit redaction can strip patterns (tokens, `-p…`) from logged commands.
- Inventory writes use an exclusive lock so two `vcon` processes do not
  clobber `hosts.toml`.
- Quitting cleans up exec jobs, health polls, SFTP, PTY sessions, and tunnels.
  Panic paths restore the terminal (raw mode / alternate screen).

---

## 17. Troubleshooting

| Symptom | What to try |
|---------|-------------|
| TUI will not start | Run from a real TTY; check `vcon doctor` |
| `ssh` not found | Install OpenSSH client; ensure `PATH` |
| Auth failures | `ssh-add -l`; test `vcon connect <host>` / plain `ssh` |
| Jump / ProxyJump issues | Confirm `jump` names exist and have no cycles (`vcon check`) |
| Slow repeat connects | Leave `multiplexing = true`; ensure control dir is writable |
| Transfers fail | Install `rsync`, or fall back relies on `scp`; check remote path perms |
| Session keys “eaten” by vcon | You are not session-focused — press Enter on Sessions or open with `o`; use prefix `d` only when you want vcon keys again |
| Stuck in a session | Prefix (`Ctrl+]`) then `d` to detach, or `x` to close |
| Corrupt inventory | `vcon check`; restore `hosts.toml.bak` if needed |
| Colours wrong | `NO_COLOR=1` or `--no-color`; adjust `[theme]` |

Debug logging:

```bash
vcon -vv exec web-1 -- true
# inspect ~/.local/state/vcon/vcon.log
```

---

## Related docs

Developer design notes and phase history live under [`docs/`](docs/README.md).
Release notes: [`CHANGELOG.md`](CHANGELOG.md).
