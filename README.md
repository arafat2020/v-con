# vcon

**One terminal dashboard for your whole VPS fleet.**

Browse hosts, jump into SSH, run commands across many servers, watch live
health, move files over SFTP, keep embedded sessions in tabs, open tunnels,
and keep an audit trail — driven by system OpenSSH (agent, jump hosts,
`known_hosts`, ControlMaster).

```text
┌ vcon ──[1 Hosts]──[2 Health]──[3 Files]──[4 Sessions (2)]──[5 Log]── ⇄ 1 ⚠ 0 ┐
│ Tags          │ Hosts ─ 23 ─ /@prod                                      │
│ ▾ all (23)    │   NAME      ADDRESS        TAGS         UP   LAST        │
│   ▸ prod (9)  │ ● web-1     203.0.113.10   web,prod     ✔    2m          │
│   ▸ db (3)    │ > web-2     203.0.113.11   web,prod     ✔    1d          │
│   recent      │ ● db-1      10.0.0.20      db,prod      ⚠    3d          │
├───────────────┴──────────────────────────────────────────────────────────┤
│ web-2  deploy@203.0.113.11 via bastion   tunnels: grafana                │
└ ⏎ connect  o session  s snippet  t tunnel  x exec  ? help  q quit ────────┘
```

| | |
|--|--|
| **Platforms** | macOS · Linux |
| **Needs** | `ssh` on `PATH` (optional `rsync`) |
| **UI** | Full-screen TUI — run `vcon` |
| **Scripting** | Same engine via CLI subcommands |
| **Docs** | [User manual](MANUAL.md) · [Changelog](CHANGELOG.md) · [Design notes](docs/README.md) |

---

## Why vcon

Working a fleet usually means juggling `ssh` one-liners, a bloated
`~/.ssh/config`, too many terminal tabs, and no shared picture of “who is
full / down / slow.” vcon keeps the inventory in editable TOML, the dashboard
in one process, and the transport as real `ssh` — so auth, jumps, and host
keys stay familiar.

**You open it in the morning and leave it open.** Find a host in a second,
shell in with Enter, fan out a command with `x`, glance at Health, copy a
config with Files, and answer “what did I run on db-1?” from the Log tab.

---

## What you get

| Area | Capability |
|------|------------|
| **Inventory** | `hosts.toml` with tags, jumps, keys, notes; import from `~/.ssh/config` |
| **Connect** | Enter → interactive SSH; exit lands you back in the dashboard |
| **Selectors** | `web-1`, `web-*`, `@prod`, `@web+prod`, `!@staging` |
| **Exec** | Parallel remote commands in TUI or `vcon exec` |
| **Health** | Live CPU / mem / disk / load / net; `vcon status` / `ping` |
| **Files** | Dual-pane local ↔ remote (SFTP); `push` / `pull` |
| **Sessions** | Embedded PTY tabs & splits; prefix `Ctrl+]` like tmux |
| **Snippets** | Parameterised runbooks (`{{service}}`) with safe shell quoting |
| **Tunnels** | `L:` / `R:` / `D:` forwards from TUI or CLI |
| **Audit** | JSONL history of exec / transfer / tunnel / session actions |
| **Hardening** | `doctor`, completions, man page, `0600` files, inventory lock |

---

## Quick start

```bash
cargo build --release
cp target/release/vcon ~/.local/bin/   # or anywhere on PATH

vcon doctor                 # ssh, agent, config paths, control dir
vcon init                   # ~/.config/vcon/hosts.toml
$EDITOR ~/.config/vcon/hosts.toml
vcon check
vcon                        # dashboard
```

Inside the TUI: `?` for keys, `1`–`5` for screens, `q` to quit.

### Thirty-second tour

```bash
# Scripting uses the same inventory and selectors
vcon ls @prod --json
vcon x @web -- 'uptime'
vcon status @prod --sort disk
vcon snippet run disk all
vcon tunnel web-1 grafana          # Ctrl+C to stop
vcon log --host web-1 --since 1d
```

---

## Demo walkthrough

### 1. Inventory that survives git

```toml
[defaults]
user = "deploy"
identity_file = "~/.ssh/id_ed25519"

[[hosts]]
name = "web-1"
address = "203.0.113.10"
tags = ["web", "prod"]
jump = "bastion"

[[hosts.tunnels]]
name = "grafana"
spec = "L:3000:localhost:3000"
```

Add / edit / delete from the Hosts screen (`a` `e` `d`) or import with `i`.
Comments in `hosts.toml` are kept on write.

### 2. Act on many hosts

Multi-select with Space, then `x`, or type a selector in search (`/@prod`).
From the shell:

```bash
vcon x '@web,@db,!staging-1' --sudo -- 'systemctl is-active nginx'
```

### 3. Live health at a glance

Tab **2** refreshes on an interval; warn/crit thresholds colour the fleet.
One-shot:

```bash
vcon st @prod
```

### 4. Files without remembering scp flags

Tab **3** — local left, remote right. Mark files, copy across, watch the
transfer queue. Or:

```bash
vcon push @web ./site/ /var/www/
vcon pull web-1 /etc/nginx/nginx.conf ./
```

### 5. Several shells without leaving vcon

`o` opens an embedded session. Prefix **`Ctrl+]`** then `n`/`p`, splits
`|`/`-`, `d` to detach back to the dashboard while sessions keep running.
Enter on Hosts still does a full terminal handoff when you want “just ssh.”

### 6. Snippets and tunnels

```toml
# ~/.config/vcon/snippets.toml
[[snippets]]
name = "restart"
command = "sudo systemctl restart {{service}}"
confirm = true
```

```bash
vcon snippet run restart @web --var service=nginx
vcon tunnel web-1 'L:3000:localhost:3000'
```

In the TUI: `s` for snippets, `t` for tunnels (`⇄ N` in the header while
forwards are up).

### 7. Audit trail

Tab **5** or:

```bash
vcon log --action exec --failed --json
```

---

## Architecture (short)

```text
┌──────────── TUI / CLI ────────────┐
│  screens · keymap · subcommands   │
├───────────────────────────────────┤
│  services (connect, exec, health, │
│  files, sessions, tunnels, …)     │
├───────────────────────────────────┤
│  domain (hosts, selectors, …)     │
├───────────────────────────────────┤
│  infra: system ssh · SFTP · PTY   │
│         inventory · audit · state │
└───────────────────────────────────┘
```

Built in Rust (ratatui + tokio). Phases 0–9 are complete; the step-by-step
design history lives under [`docs/phases/`](docs/phases/).

---

## Project map

| Path | Role |
|------|------|
| [`MANUAL.md`](MANUAL.md) | Full user manual (keys, config, CLI, troubleshooting) |
| [`CHANGELOG.md`](CHANGELOG.md) | Release notes |
| [`docs/`](docs/README.md) | Requirements, architecture, data model, decisions |
| `src/` | Application source |
| `tests/` | CLI + ignored Docker sshd integration scaffold |
| `.github/workflows/ci.yml` | fmt, clippy, tests (macOS + Linux) |

---

## Develop

```bash
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
cargo fmt --check

# Optional Docker sshd fixtures (ignored by default)
cargo test --test integration_ssh -- --ignored
```

Release profile uses LTO, single codegen unit, and stripped binaries
(`Cargo.toml`).

Shell completions and man page:

```bash
vcon completions zsh > ~/.zsh/completions/_vcon
vcon man | man -l -
```

---

## Not in scope

Provisioning (Terraform), config management (Ansible), a web UI, Windows
hosts, storing passwords, or replacing every tmux feature. vcon is for
**interactive, ad-hoc** fleet work on Linux servers.

---

## License

See repository license file if present; otherwise treat as source-available
for your own use until a license is added.
