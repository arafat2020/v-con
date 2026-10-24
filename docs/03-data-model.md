# 03 — Data Model

## Inventory file: `~/.config/vcon/hosts.toml`

The inventory is a plain TOML file. You can edit it by hand, keep it in git,
and copy it between machines. `vcon add`/`edit`/`rm` modify the same file.

### Example

```toml
# Values here apply to every host unless the host overrides them.
[defaults]
user = "root"
port = 22
identity_file = "~/.ssh/id_ed25519"
connect_timeout = 5        # seconds

[[hosts]]
name = "bastion"
address = "198.51.100.5"
user = "admin"
tags = ["infra"]

[[hosts]]
name = "web-1"
address = "203.0.113.10"
user = "deploy"
tags = ["web", "prod", "hetzner"]
jump = "bastion"           # name of another host in this file
notes = "Main nginx box. Certs renew via certbot timer."

[[hosts.tunnels]]
name = "grafana"
spec = "L:3000:localhost:3000"

[[hosts]]
name = "db-1"
address = "10.0.0.20"
port = 2222
tags = ["db", "prod"]
jump = "bastion"
identity_file = "~/.ssh/db_key"

[[hosts]]
name = "staging-1"
address = "staging.example.com"
tags = ["web", "staging"]
```

### Host fields

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| `name` | string | yes | — | Unique. Lowercase letters, digits, `-`, `_`, `.`. This is what you type. |
| `address` | string | yes | — | IP or DNS name. |
| `user` | string | no | `defaults.user`, else your local user | |
| `port` | integer | no | `defaults.port`, else 22 | 1–65535. |
| `identity_file` | path | no | `defaults.identity_file`, else let ssh decide | `~` is expanded. |
| `jump` | string | no | none | Another host's `name` (resolved by vcon) **or** a raw `user@host:port`. |
| `tags` | list of strings | no | `[]` | Used by selectors (`@prod`). |
| `notes` | string | no | — | Free text shown in `show` and the TUI. |
| `ssh_options` | map string→string | no | `{}` | Extra `-o Key=Value` passed to ssh, e.g. `{ ServerAliveInterval = "30" }`. |
| `tunnels` | list of tunnel | no | `[]` | Saved port forwards (phase 8). |
| `disabled` | bool | no | `false` | Excluded from selectors like `all`/`@tag`, still connectable by name. |

### Defaults section

Same optional fields as a host (`user`, `port`, `identity_file`, `jump`,
`ssh_options`) plus `connect_timeout`. **Resolution order** for any field:

1. Value on the host
2. Value in `[defaults]`
3. Built-in default (or "don't pass it, let ssh decide")

Implement this as one pure function: `(host, defaults) → effective host`. Every
other part of the program works only with the effective host.

### Validation rules

Validate on load and on every write. Report **all** problems at once with the
host name (and line number if you can get it from the TOML parser):

- Names are unique and match the allowed characters
- `address` is non-empty
- `port` is in range
- `jump` refers to an existing host (if it is a plain name) and **does not create a cycle** (`a → b → a`)
- Tunnel specs parse correctly (phase 8)

### Writing the file safely

- Use `toml_edit` for modifications so **your comments and formatting survive** when `vcon` adds or removes a host.
- Write atomically: write to `hosts.toml.tmp` in the same directory, then rename over the original. A crash mid-write must never leave a half-written inventory.
- Optionally keep one backup: `hosts.toml.bak` before each write.

## Host selectors

Selectors decide which hosts a multi-host command acts on. One small grammar
is used everywhere (`exec`, `push`, `status`, `list --filter`, TUI filter).

### Grammar

A selector is a comma-separated list of **terms**. Each term is one of:

| Term | Meaning | Example |
|------|---------|---------|
| `name` | exact host name | `web-1` |
| `glob` | name glob with `*` and `?` | `web-*` |
| `@tag` | every host with that tag | `@prod` |
| `@tag1+tag2` | hosts having **all** listed tags | `@web+prod` |
| `all` | every enabled host | `all` |
| `!term` | exclude hosts matching the term | `!web-3`, `!@staging` |

### Evaluation

1. Start with an empty set.
2. Add every host matched by each **positive** term (union).
3. If the selector has **only** negative terms, start from `all` instead.
4. Remove every host matched by any **negative** term.
5. Return hosts in inventory order (stable, predictable output).

### Examples

| Selector | Result |
|----------|--------|
| `web-1` | just web-1 |
| `web-*` | web-1, web-2, … |
| `@prod` | every prod host |
| `@web+prod` | prod web servers only |
| `@web,@db,!web-3` | all web and db hosts except web-3 |
| `!@staging` | everything except staging |

### Error behaviour

- A term that matches nothing is a **warning** (likely a typo); an entirely empty result is an **error** — never "successfully run on zero hosts".
- If a bare word is not a host name and not a glob, suggest close matches ("no host `wbe-1`, did you mean `web-1`?").

## Tunnel spec (phase 8)

Compact string form, mirroring ssh flags:

| Spec | Meaning | ssh flag |
|------|---------|----------|
| `L:3000:localhost:3000` | local port 3000 → remote's localhost:3000 | `-L 3000:localhost:3000` |
| `R:9000:localhost:9000` | remote port 9000 → your localhost:9000 | `-R 9000:localhost:9000` |
| `D:1080` | SOCKS proxy on local port 1080 | `-D 1080` |

## Snippets file: `~/.config/vcon/snippets.toml` (phase 8)

```toml
[[snippets]]
name = "disk"
description = "Disk usage of root filesystem"
command = "df -h /"

[[snippets]]
name = "restart"
description = "Restart a systemd service"
command = "sudo systemctl restart {{service}} && systemctl status {{service}} --no-pager"
confirm = true                 # always ask before running

[[snippets]]
name = "logs"
command = "sudo journalctl -u {{service}} -n {{lines|100}} --no-pager"
```

`{{var}}` is required; `{{var|default}}` has a default. Missing variables are
prompted for interactively or passed as `--var service=nginx`.

## State file: `~/.local/share/vcon/state.json`

Owned by vcon, not meant for hand editing. Safe to delete.

```json
{
  "version": 1,
  "hosts": {
    "web-1": {
      "last_connected": "2026-10-24T10:12:00Z",
      "connect_count": 42,
      "favorite": true,
      "last_health": { "checked_at": "...", "reachable": true, "load1": 0.42, "...": "..." }
    }
  },
  "ui": { "last_screen": "hosts", "last_filter": "@prod", "exec_history": ["df -h /", "uptime"] }
}
```

Include a `version` field from day one so you can migrate the format later.

## Audit log: `~/.local/state/vcon/audit.jsonl` (phase 8)

One JSON object per line, append only:

```json
{"ts":"2026-10-24T10:15:02Z","action":"exec","selector":"@web","hosts":["web-1","web-2"],"command":"sudo apt update","results":{"web-1":0,"web-2":0},"duration_ms":3120}
```

## Settings: `~/.config/vcon/config.toml`

```toml
parallel = 10                  # max concurrent ssh processes (exec, health, sftp)
confirm_threshold = 5          # ask for confirmation above this many hosts
protected_tags = ["prod"]      # always confirm when any of these is targeted
multiplexing = true            # use ssh ControlMaster

[tui]
start_screen = "hosts"         # or "last" to resume where you left off
mouse = false
fps = 30                       # max redraw rate
show_detail_pane = true

[health]
refresh_seconds = 30
history_samples = 60           # points kept for sparklines
warn = { disk = 80, mem = 85, load_per_cpu = 1.0 }
crit = { disk = 90, mem = 95, load_per_cpu = 2.0 }

[sessions]
prefix_key = "ctrl-]"
scrollback_lines = 5000

[keys.hosts]                   # override any default binding
connect = "enter"
exec = "x"
open_session = "o"

[theme]
highlight = "cyan"
ok = "green"
warn = "yellow"
crit = "red"
```
