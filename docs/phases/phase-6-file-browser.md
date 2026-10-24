# Phase 6 — File Browser

**Goal:** A two-pane, Midnight-Commander-style file browser: local on the left,
remote on the right, copy files either way with a transfer queue showing
progress.

**You can now:** press `f` on a host (or `3`), navigate both file systems, and
copy with `F5`/`c`. CLI: `vcon push`, `vcon pull`.

**Estimated effort:** 5–8 days

Reference: [D13](../05-decisions.md#d13-file-browser-uses-sftp-over-system-ssh)

## Layout

```
┌ Local: ~/projects/site ──────────────┐┌ web-1: /etc/nginx ───────────────────┐
│ ..                                    ││ ..                                    │
│ ▸ dist/                     -   2m    ││ ▸ conf.d/              -   drwxr-xr-x │
│ ● nginx.conf             2.1K  1h     ││ ▸ sites-enabled/       -   drwxr-xr-x │
│   README.md              4.0K  3d     ││   nginx.conf        2.4K   -rw-r--r-- │
├ Transfers ─ 1 active ─ 2 queued ──────┴┴──────────────────────────────────────┤
│ ↑ nginx.conf → web-1:/etc/nginx/       ██████████░░░░  68%  1.4K/2.1K  12K/s  │
└ Tab pane  ⏎ open  ⌫ up  space mark  c copy  r rename  m mkdir  d delete  . hidden ┘
```

## Step 1 — `RemoteFs` trait + SFTP implementation

- [ ] Trait: `list(dir)`, `stat`, `mkdir`, `rename`, `remove_file`, `remove_dir`, `open_read`, `open_write`, `realpath` (to resolve `~` / home directory).
- [ ] Implementation over SFTP via a system `ssh -s sftp` subprocess (`openssh-sftp-client`), with ssh args from the shared builder (jump hosts, keys, ControlMaster all apply).
- [ ] One SFTP session per host, opened lazily, reused, closed when the Files screen has been idle for a while.
- [ ] `LocalFs` implementing the same trait over `tokio::fs`, so both panes share all browsing code.
- [ ] `FakeRemoteFs` for tests.
- [ ] Spike first: before building the UI, write a tiny throwaway program that lists a remote directory with the chosen SFTP crate through a jump host. If it doesn't work well, switch to the fallback in D13 early.

## Step 2 — `FileService`

- [ ] Directory listing with sorting (dirs first, then name / size / mtime) and hidden-file toggle.
- [ ] **Transfer queue**: jobs (upload/download, file or recursive directory), each with bytes done/total, rate, ETA, state (queued, running, done, failed, cancelled).
- [ ] Limited parallel transfers (e.g. 2), chunked read/write streaming with progress events.
- [ ] Conflict policy when the destination exists: ask (overwrite / skip / overwrite all / skip all / rename).
- [ ] Cancellation removes partial files (or keeps them with a `.part` suffix and renames on success).
- [ ] Preserve permissions and mtime where possible.
- [ ] Optional: "Sync directory" action that uses `rsync` via `SshBackend::copy` for delta transfer of large trees.

## Step 3 — Files screen

- [ ] Two panes, `Tab` switches focus; each pane has its own path, cursor, marks, and filter.
- [ ] Navigation: `Enter` opens dir, `Backspace`/`h` goes up, `~` goes home, `g` "go to path" input with tab-completion (stretch).
- [ ] `Space` marks files; `c`/`F5` copies marked (or cursor) files to the other pane's directory; `r` rename; `m` mkdir; `d` delete with confirm listing what will be deleted.
- [ ] `/` filter within the current directory; `.` toggle hidden files; `s` cycle sort.
- [ ] Host switcher (`H`): choose which host the remote pane shows (fuzzy picker over the inventory).
- [ ] **View** (`v`): open a remote text file read-only in a scrollable popup (cap size, e.g. 1 MB; refuse binary).
- [ ] **Edit** (`e`, stretch): download to a temp file, suspend TUI, open `$EDITOR`, on exit upload if changed (confirm), clean up temp.
- [ ] Transfer queue panel: collapsible, shows progress bars; transfers continue when you switch screens; tab bar shows an indicator while active.
- [ ] Errors (permission denied, disk full) appear per transfer and as toasts.

## Step 4 — CLI

- [ ] `vcon push <selector> <local> <remote>`: multi-host, parallel, with confirm policy and summary (rsync if available, else scp — or reuse SFTP).
- [ ] `vcon pull <name> <remote> <local>`: single host.

## Done when

- Browsing `/var/log` on a host behind a bastion feels instant after the first open
- Copying a directory up and a log file down both show accurate progress
- Cancelling a transfer leaves no corrupt half-file at the destination
- Quitting vcon with active transfers asks first

## Pitfalls

- Remote paths are always `/`-separated strings; don't use `std::path::Path` for them (it uses local OS rules).
- Symlinks: show them distinctly; decide whether Enter follows directory symlinks (yes, but avoid infinite loops in recursive copy).
- File names can contain spaces, newlines, and non-UTF-8 bytes. Use lossless types where possible and display lossy.
- Large directories (10k+ entries): list in the background and render only visible rows.
