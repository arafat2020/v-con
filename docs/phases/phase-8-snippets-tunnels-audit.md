# Phase 8 — Snippets, Tunnels, and Audit Log

**Goal:** Save common commands as parameterised snippets, manage port
forwards from the TUI, and keep a record of everything run across the fleet.

**You can now:** `s` to pick and run a snippet, `t` to start a saved tunnel,
`5` to browse the audit log. CLI: `vcon snippet run`, `vcon tunnel`, `vcon log`.

**Estimated effort:** 4–6 days

## Part A — Snippets

Reference: [snippets file format](../03-data-model.md#snippets-file-configvconsnippetstoml-phase-8)

- [ ] `Snippet` domain type: name, description, command template, `confirm` flag, optional default selector.
- [ ] Pure template engine: `{{var}}` and `{{var|default}}`; report missing variables.
- [ ] **Shell-quote substituted values** (single quotes, escape embedded quotes). Unit tests with spaces, quotes, `;`, `$()`.
- [ ] Store: read/write `snippets.toml` with `toml_edit`.
- [ ] TUI snippet picker (`s`): fuzzy list with descriptions and command preview → variable form (reuse phase 3 form widgets, fields generated from the template's variables, defaults prefilled) → confirm → run through `ExecService` → results in the exec view.
- [ ] Snippet management screen or modal: add / edit / delete snippets (reuse form widgets).
- [ ] `confirm = true` snippets always ask.
- [ ] CLI: `vcon snippet list`, `vcon snippet run <name> <selector> --var k=v`.

## Part B — Tunnels

Reference: [tunnel spec](../03-data-model.md#tunnel-spec-phase-8)

- [ ] Pure tunnel spec parser (`L:`, `R:`, `D:`) with tests.
- [ ] Saved tunnels per host in `hosts.toml`; edit them in the host form (a small list editor).
- [ ] `TunnelService`: start `ssh -N` with forward flags + `ExitOnForwardFailure=yes`, track running tunnels, stop on request or on vcon exit.
- [ ] Before a local forward, check the local port is free; clear error if not.
- [ ] TUI: `t` on a host shows its saved tunnels with start/stop toggles and a "new ad-hoc tunnel" input; a tunnels indicator in the tab bar shows how many are running (`⇄ 2`); the host detail pane lists active tunnels.
- [ ] Detect tunnel death (child exit) and show a toast; offer restart.
- [ ] CLI: `vcon tunnel <host> <spec or name>` in the foreground, Ctrl+C to stop.

## Part C — Audit log

Reference: [audit log format](../03-data-model.md#audit-log-localstatevconauditjsonl-phase-8)

- [ ] `AuditLog`: append one JSON line per action (exec, snippet run, transfer, delete remote file, tunnel start, session open). One write per entry.
- [ ] Record: timestamp, action, hosts, command (post-substitution), per-host exit code, duration. **Never** command output.
- [ ] Optional redaction regexes in config (e.g. `-p\S+`, `password=\S+`).
- [ ] Rotation at a size limit (e.g. 10 MB → `audit.jsonl.1`).
- [ ] **Log screen** (`5`): table (time, action, hosts, command, result), filter by host/action/failed, `/` search, Enter for full detail, `R` to re-run an exec entry (with confirm).
- [ ] CLI: `vcon log [--host] [--action] [--since 1d] [--failed] [--json]`.

## Done when

- Your five most common commands are snippets run from the TUI in a few keystrokes
- A Grafana/Postgres tunnel starts with two keys and shows as active in the tab bar
- "What did I run on db-1 last week?" is answered by the Log screen

## Pitfalls

- Template injection: test a variable value of `; rm -rf /` against a throwaway Docker container, never a real server.
- Tunnels outliving vcon: make sure they're children that die with vcon (or explicitly killed on quit, including on panic).
