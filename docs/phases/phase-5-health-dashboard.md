# Phase 5 — Health Dashboard

**Goal:** A live Health tab showing which servers are up, busy, out of memory,
or running out of disk — refreshing automatically, with history sparklines.

**You can now:** press `2` and watch your fleet. Hosts screen shows a health
column too. CLI: `vcon status`, `vcon ping`.

**Estimated effort:** 3–5 days

Reference: [D14](../05-decisions.md#d14-health-by-polling-a-script-not-an-agent)

## Step 1 — The health script

One POSIX `sh` script (no bash-isms), embedded in the binary
(`include_str!`), printing `key=value` lines. Missing tools → missing keys,
never a failure.

| Metric | Source |
|--------|--------|
| Hostname, kernel | `hostname`, `uname -r` |
| OS | `/etc/os-release` `PRETTY_NAME` |
| Uptime | `/proc/uptime` |
| Load average | `/proc/loadavg` |
| CPU count | `nproc` |
| **CPU counters** | first line of `/proc/stat` (raw counters; % computed locally) |
| Memory | `/proc/meminfo` `MemTotal`, `MemAvailable` |
| Swap | `/proc/meminfo` `SwapTotal`, `SwapFree` |
| Disk | `df -P` for `/` (and optionally all real filesystems) |
| **Network counters** | `/proc/net/dev` bytes rx/tx, excluding `lo` (rates computed locally) |
| Failed units | `systemctl --failed --no-legend` line count |
| Reboot required | existence of `/var/run/reboot-required` |
| Updates pending (stretch) | `apt list --upgradable` count — slow, so run less often |

## Step 2 — Domain: parsing and math

- [ ] `RawSample` parser from `key=value` text; unknown keys ignored.
- [ ] `HealthReport` with derived values: mem %, swap %, disk %, load per CPU, human uptime.
- [ ] **Delta math**: CPU % = busy delta / total delta between two `/proc/stat` samples; network bytes/sec = counter delta / time delta. Handle counter resets (reboot) by discarding negative deltas.
- [ ] Status classification (ok / warn / crit / unreachable) from configurable thresholds.
- [ ] Unit tests with captured outputs from Ubuntu, Debian, Rocky/Alma, and Alpine (busybox).

## Step 3 — `HealthService`

- [ ] Scheduler: refresh all (or visible) hosts every `refresh_seconds`, staggered so polls don't all fire at the same instant.
- [ ] Uses the exec backend + global semaphore + ControlMaster.
- [ ] Per host: latest report, previous raw sample (for deltas), and a **ring buffer** history (`history_samples`) of CPU %, mem %, load, net rx/tx.
- [ ] Sends `HealthUpdated { host, report }` actions; caches the latest report in the state file so the next launch shows data immediately (marked stale).
- [ ] Pause/resume, refresh-now for one host or all.
- [ ] Back off on unreachable hosts (e.g. retry every 2 minutes instead of every 30 s).

## Step 4 — Health screen

```
┌ Health ─ 23 hosts ─ 20 ✔ 2 ⚠ 1 ✖ ─ refresh 30s ─ sort: disk ────────────────┐
│ HOST     ST  CPU            LOAD  MEM         DISK        NET ↓/↑     UP    │
│ db-1     ⚠   37% ▂▃▅▆▅▃▂▃  1.80  ████████░ 81%  █████████ 92%  1.2M/300K  102d │
│ web-1    ✔   12% ▁▁▂▁▁▂▁▁  0.12  ████░░░░░ 38%  ██████░░░ 61%  40K/12K    41d  │
│ cache-1  ✖   — connection timed out (2m ago)                                │
└ ⏎ details  s sort  p pause  r refresh  R refresh all  / filter  ⏎⏎ connect ─┘
```

- [ ] Table with sparklines (ratatui `Sparkline`) and inline gauges; colour by status, plus symbols so it works without colour.
- [ ] Sort by any column (`s` cycles; `S` reverses); filter with `/` (same search/selector as Hosts).
- [ ] Stale indicator (data older than 2× refresh interval).
- [ ] **Host detail view** (Enter): bigger `Chart`s for CPU, memory, network over the history window, plus all filesystems, failed units, reboot-required flag.
- [ ] From the health screen, `c` connects (handoff) and `x` execs on the highlighted/selected hosts.
- [ ] Hosts screen gets a compact `UP` column and the tab bar shows the warn/crit count.

## Step 5 — CLI

- [ ] `vcon status [selector]`: single poll (two samples one second apart for CPU %, or omit CPU in one-shot mode), table or `--json`, `--sort`.
- [ ] `vcon ping [selector]`: connect + `true`, prints latency.

## Done when

- The Health tab updates live without ever stuttering the UI
- A host filling its disk turns yellow then red without you doing anything
- Restarting vcon shows the last known metrics instantly, then refreshes

## Pitfalls

- Use `df -P` (POSIX format) or long device names wrap onto two lines.
- Use `MemAvailable`, not `MemFree`.
- Don't run the script with sudo — everything above is readable by a normal user.
- 200 hosts × every 30 s = ~7 polls/second. Fine with ControlMaster, but respect the semaphore and stagger.
