# 04 — CLI Reference

vcon is TUI-first: **running `vcon` with no arguments opens the dashboard.**
The subcommands below exist for scripting, automation, and quick one-offs.
They are thin wrappers over the same services the TUI uses, so each is cheap
to add once the matching TUI feature exists.

## Global flags

| Flag | Meaning |
|------|---------|
| `-v`, `-vv` | More logging (to the log file; debug shows exact ssh commands) |
| `--config-dir <path>` | Use a different config directory (also `VCON_CONFIG_DIR`) |
| `--json` | Machine-readable output where supported |
| `--no-color` | Disable colours (also `NO_COLOR`) |
| `-y`, `--yes` | Skip confirmation prompts |

## Commands

| Command | Phase | Description |
|---------|-------|-------------|
| `vcon` | 0 | Open the TUI |
| `vcon init` | 1 | Create config dir and a commented example `hosts.toml` |
| `vcon list [selector]` (`ls`) | 1 | Print hosts; `--json` for scripts |
| `vcon check` | 1 | Validate the inventory, print all problems, non-zero exit if any |
| `vcon import ssh-config [--dry-run]` | 3 | Import from `~/.ssh/config` without opening the TUI |
| `vcon connect <name>` (`c`) | 2 | Interactive SSH straight from your shell (replaces the process with ssh) |
| `vcon exec <selector> -- <command>` (`x`) | 4 | Parallel command; grouped or `--stream` output; exit code 3 if any host failed |
| `vcon status [selector]` (`st`) | 5 | One-shot health table or `--json` |
| `vcon push <selector> <local> <remote>` | 6 | Upload via rsync/scp |
| `vcon pull <name> <remote> <local>` | 6 | Download via rsync/scp |
| `vcon tunnel <name> <spec or saved name>` | 8 | Foreground port forward, Ctrl+C to stop |
| `vcon snippet run <name> <selector> [--var k=v]` | 8 | Run a saved snippet |
| `vcon log [--host h] [--since 1d]` | 8 | Print the audit log |
| `vcon doctor` | 9 | Check ssh/rsync, config validity, agent keys |
| `vcon completions <shell>` | 9 | Shell completion script |

## `exec` flags

| Flag | Meaning |
|------|---------|
| `-p`, `--parallel <n>` | Max concurrent hosts (default 10) |
| `-t`, `--timeout <secs>` | Per-host timeout |
| `--stream` / `--grouped` | Output mode (grouped is default) |
| `--only-failed` | Only show output of failed hosts |
| `--dry-run` | Show hosts and exact ssh commands, run nothing |
| `--sudo` | Prefix with `sudo -n` (fail instead of hanging on a password) |

## Example

```
$ vcon x @web --only-failed -- 'systemctl is-active nginx'
── staging-1 ─ exit 3 ─ 0.3s
inactive

4 hosts: 3 ok, 1 failed (staging-1)
$ echo $?
3
```
