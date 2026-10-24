# Phase 4 — Remote Exec

**Goal:** Select hosts, type a command, and watch it run on all of them in
parallel with live status and per-host output.

**You can now:** `Space` to select hosts (or use a `@tag` filter + `Ctrl+A`),
`x`, type `sudo systemctl reload nginx`, confirm, and watch. CLI:
`vcon exec @web -- uptime`.

**Estimated effort:** 3–5 days

## Step 1 — `SystemSsh::exec`

- [ ] Build args with the shared builder plus `BatchMode=yes`, `ConnectTimeout`, and ControlMaster options.
- [ ] Spawn with piped stdout/stderr and **null stdin**.
- [ ] Streaming: read stdout and stderr **line by line concurrently** and emit each line as it arrives.
- [ ] Timeout with `tokio::time::timeout`; on timeout or cancellation, kill the child.
- [ ] Classify ssh failures (exit 255 + stderr): connection refused, timed out, auth failed, host key changed, DNS failure.
- [ ] Cap captured output per host (e.g. 1 MB, keep the tail) with a "truncated" marker.

## Step 2 — `ExecService` (no ratatui!)

- [ ] `start_job(hosts, command, options) → JobId` plus a stream/channel of events: `HostStarted`, `Line { host, stream, text }`, `HostFinished { host, result }`, `JobFinished { summary }`.
- [ ] Concurrency via the global semaphore; one host failing never cancels others.
- [ ] Cancellation token per job: cancel kills all running children and marks queued hosts as skipped.
- [ ] Confirmation **policy** lives here as a pure function: `needs_confirmation(hosts, settings) → Option<Reason>` (count over threshold, protected tags). The TUI and CLI each decide how to ask.
- [ ] Unit tests with a `FakeBackend`: partial failures, timeouts, cancellation, concurrency limit respected, event ordering.

## Step 3 — TUI: command input

- [ ] `x` opens a command modal targeting the multi-selection, or the cursor host if nothing is selected. Title shows the target: "Run on 4 hosts (web-1, web-2, … )".
- [ ] Input with **history** (↑/↓ cycles previous commands, saved in the state file).
- [ ] Options toggles inside the modal: `sudo -n` prefix, timeout.
- [ ] Confirm modal when the policy requires it, stating the exact command and host count. For big prod runs, require typing the number of hosts to confirm (stretch).

## Step 4 — TUI: exec results view

```
┌ Exec: sudo systemctl reload nginx ─ 4 hosts ─ 3 ✔ 1 ✖ ─ 2.1s ──────────────┐
│ ✔ web-1    0.4s  ││ ── staging-1 ── exit 255 ── auth failed ──               │
│ ✔ web-2    0.5s  ││ deploy@staging.example.com: Permission denied            │
│ ✔ web-4    0.6s  ││ (publickey).                                             │
│ ✖ staging-1 2.1s ││                                                          │
│                  ││                                                          │
└ ↑↓ host  ⏎ full screen  f failed only  R rerun failed  y copy  c cancel  Esc ┘
```

- [ ] Left: host list with live status (spinner while running, ✔ / ✖ / ⏱ timeout / ⊘ skipped) and duration.
- [ ] Right: output of the highlighted host, streaming live, auto-scroll at bottom, scrollable with PgUp/PgDn; stderr in a different colour.
- [ ] `f` show failed only; `R` rerun on failed hosts; `y` copy output (`arboard` or OSC 52); `c` cancel; `Esc` closes the view (job keeps running — reopen with `X`).
- [ ] Status bar indicator while a job runs in the background.
- [ ] Optional "combined" view: all output interleaved with `[host]` prefixes.

## Step 5 — CLI

- [ ] `vcon exec <selector> -- <command>` with grouped (default) or `--stream` output, `--only-failed`, `--dry-run`, `--sudo`, `--json`, `-p`, `-t`.
- [ ] Progress bar (`indicatif`) on stderr in grouped mode; summary line; exit code 3 if any host failed.
- [ ] Confirmation prompt on a TTY; refuse without `--yes` when not a TTY.

## Done when

- Running `uptime` on 10+ hosts finishes in about the time of the slowest host
- An unreachable host shows a clear reason while others succeed
- Closing the results view doesn't stop the job; cancelling does, and leaves no orphan ssh processes (check with `ps`)
- The same `ExecService` powers both the TUI and the CLI

## Pitfalls

- **Quoting**: the command is sent as one string and parsed by the *remote* shell. Test quotes, `$VARS`, pipes, `&&`.
- **ControlPath length** on macOS (~104 bytes): use `%C` and a short cache path.
- **stdin**: forgetting `Stdio::null()` causes hangs and swallowed keystrokes — fatal inside a TUI.
- Flooding the UI: a command printing 100k lines sends 100k actions. Batch lines per host per frame before sending them to the UI loop.
