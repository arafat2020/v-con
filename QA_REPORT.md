# vcon QA report — file transfer

Verdict: **NO-GO** for production.

File transfer is a client pull over SFTP (`ssh -s sftp`, `openssh-sftp-client`), 256 KiB chunks (`CHUNK` in `src/services/files.rs`), two transfers at a time, written to `destination.part` and renamed into place. SSH is the only integrity layer. There is no application checksum, no resume, and no cap on a peer that never returns EOF.

The release binary builds, the existing suite passes, and the happy path (including a 2 GiB local copy and a real localhost OpenSSH SFTP server) works. The same path accepts truncated files, follows symlinks, writes outside the destination directory when a listing name contains `..`, and can fill the disk from `/dev/zero`. Those are confirmed failures of the transfer path, so this is not a ship candidate.

No application source was changed except two `#[cfg(test)]` module hooks (`src/main.rs`, `src/tui/mod.rs`) so the new tests compile. They are absent from the release binary. Nothing here was pointed at a production host.

## What was tested and how

| Command | Result |
|---|---|
| `cargo test --bin vcon -- --nocapture qa_` | 20 passed, 14 failed (the failures are the bugs below) |
| `cargo test --test qa_cli_transfer -- --nocapture` | 3 passed, 3 failed |
| `cargo test -- --skip qa_` | 210 + 6 + 2 passed, 0 failed. 3 Docker tests in `tests/integration_ssh.rs` stayed ignored |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --all-targets -- -D warnings` | pass |
| `cargo build --release` | pass (`Finished release profile`, LTO, ~2m12s) |
| `cargo audit` 0.23.0 (1533 advisories) | exit 0, 6 warnings (see dependency notes) |
| `VCON_QA_LARGE=1 cargo test --bin vcon -- --nocapture qa_large_file_streams` | pass |

Local fixture only: user-level `sshd` on `127.0.0.1` (pubkey, `IdentitiesOnly`, empty vs seeded `known_hosts`) and `/usr/lib/openssh/sftp-server`. Debug test binary unless noted.

Happy path that passed:

- Local and SFTP round trips at 0, 1, `CHUNK-1`, `CHUNK`, `CHUNK+1`, `2*CHUNK-1`, `2*CHUNK`, `2*CHUNK+3` bytes, text and binary (including NUL and `0xFF`).
- Names with spaces, `文件`, a newline, and a 200-byte name. A 256-byte name and a NUL path return errors and do not panic.
- 40-file tree, empty directory, same file twice, copy onto itself, CLI directory push, CLI pull of the uploaded bytes.
- Cancel mid-copy deletes `.part` and leaves the previous destination bytes (`qa_cancel_keeps_the_original_and_deletes_the_part`, same on SFTP).
- Permission denied and a 1 MiB tmpfs (`ENOSPC`) surface an error and remove `.part` on the local filesystem.
- A read error after the first chunk keeps the old destination and deletes `.part`.
- Mode `0700` is preserved when `set_meta` succeeds. 30 local copies do not leak fds (`before` to `after` stayed under +20).
- 32 MiB copy matches and grows RSS by less than 64 MiB.
- **2 GiB** local copy: 2147483648 bytes in 3.22 s (**636 MiB/s**), RSS 9176 KiB → 16460 KiB (**+7284 KiB**), HWM 16460 KiB. It streams; it does not hold the file.
- Wrong key and an unknown host key do not create the remote file. The matching key push and pull succeed (`qa_cli_wrong_key_cannot_upload_and_right_key_can`). Unauthenticated argv includes `BatchMode=yes` and does not contain a password or `StrictHostKeyChecking=no`.
- Malformed SFTP bytes fail the handshake and do not panic.
- `hosts.toml` writes keep a one-generation `.bak` (`qa_inventory_write_keeps_a_one_generation_backup`). There is no migration framework and no automatic rollback command.
- A declared size of `u64::MAX` with an immediate EOF does not allocate that size (RSS growth under 64 MiB, returned in under 2 s).
- Property tests: random payloads up to 8 KiB round-trip; random strings do not panic `normalize` / `join`.

## What was not tested

- No production URL, credential, or user data.
- `tests/integration_ssh.rs` (`DOCKER=1`, linuxserver image, password profile) was not run. Pubkey SFTP was tested with a local `sshd` instead.
- Password-authenticated SFTP was not executed. `sshpass` is not installed, and the keychain/askpass prompt was not driven. Argv inspection only.
- Multi-GB **over SFTP** was not measured. The largest SFTP payload in the matrix is `2*CHUNK+3`; the CLI binary case is 300000 bytes. `copy_file` is shared with the 2 GiB local run, and `SftpFs::read_at` / `write_at` allocate one chunk, not the declared size. Throughput over SSH was not measured.
- The numbers above are from the **debug** test binary. `cargo build --release` succeeded; release throughput was not re-measured.
- No libFuzzer. `proptest` covered random bytes and paths.
- Linux only. The TUI conflict tests call `App::on_key`; they do not open a real terminal.
- Resume from a crash was not tested beyond “partials are deleted on a local error, and a dead SFTP session leaves `.part`”.

## Confirmed bugs

### 1. Directory listing names with `..` write outside the destination

- Area: backend
- Severity: **critical**
- Test: `qa_hostile_directory_entry_cannot_escape_the_destination`
- Steps: `plan_copies` a directory whose `list` returns a file named `../../escaped.bin`, then `copy_file` into `{tmp}/dest`.
- Expected: the name is rejected, or the bytes stay under `{tmp}/dest`.
- Actual: `Ok(())`, and the bytes land at `{tmp}/escaped.bin` (`escape exists=true`, destination path `.../dest/inbox/../../escaped.bin`).
- Evidence: `src/services/files.rs:257` joins `entry.name` with no check. `src/infra/remote_fs/path.rs:4-18` only special-cases a name that is exactly `..`. `src/infra/remote_fs/sftp.rs:232` uses `filename().to_string_lossy()` unchanged. A normal OpenSSH directory cannot contain a slash, so this needs a hostile SFTP listing (or any `RemoteFs` that returns one). The client does not stop it.

### 2. A size-0 reader that never returns EOF is copied without bound

- Area: backend
- Severity: **critical**
- Tests: `qa_dev_zero_copy_must_stop`, `qa_zero_size_infinite_reader_must_stop`
- Steps: `stat` size is 0 (`/dev/zero` is 0) and `read_at` keeps returning bytes. `copy_file` is given 400 ms.
- Expected: refuse non-regular or non-terminating input.
- Actual: the call was still running at the timeout. `/dev/zero` had already handed the writer **1045168128** bytes. A scripted size-0 reader had handed it **16303259648** bytes. Both go through `dst.write_at`.
- Evidence: `src/services/files.rs:410-424`. When `total == 0` the loop does not stop on offset; it stops only on an empty read. `/dev/zero` never returns one. A download of a remote `/dev/zero`, or a peer that advertises size 0 and keeps sending, fills the destination disk. The `u64::MAX` case does **not** preallocate (`qa_untrusted_size_does_not_allocate_the_declared_length` passed); the leak is the unbounded loop, not a single allocation.

### 3. Early EOF is a successful, truncated file

- Area: backend
- Severity: **high**
- Test: `qa_short_read_must_not_report_success`
- Steps: `stat` says 1000 bytes; the first read returns `short` (5 bytes) and the next read is empty.
- Expected: error, destination not renamed into place.
- Actual: `Ok(())` and the destination is the 5 bytes `[115, 104, 111, 114, 116]`.
- Evidence: `src/services/files.rs:419-424` treats an empty read as success even when `offset < total`. Nothing hashes the file. SSH’s MAC covers the bytes that arrived; it does not notice that the file ended early. There is no application checksum anywhere in the transfer path.

### 4. Symlinks are followed and stored as regular files

- Area: backend
- Severity: **high**
- Tests: `qa_symlink_outside_the_tree_is_not_copied_as_bytes`, `qa_sftp_symlink_is_not_followed`
- Steps: a tree contains `link` → a file outside the tree holding `TOP-SECRET`. Copy the tree locally, and download the symlink over the local `sshd` SFTP server.
- Expected: keep the symlink, or refuse it.
- Actual: both copies return success and the destination is a regular file whose contents are `TOP-SECRET`.
- Evidence: `LocalFs::read_at` uses `File::open` (`src/infra/remote_fs/local.rs:126`), which follows symlinks. `SftpFs::read_at` uses `sftp.open` (`src/infra/remote_fs/sftp.rs:305`). `collect_tree` (`src/services/files.rs:258`) does not recurse into a symlink, but it still copies it as a file. Selecting a directory that contains a symlink to `~/.ssh/id_rsa` uploads the key bytes. A directory symlink does **not** loop (`qa_directory_symlink_does_not_recurse_forever` passed).

### 5. `vcon push ../secret` uploads `./secret`

- Area: backend
- Severity: **high**
- Tests: `qa_normalize_keeps_relative_parent_segments`, `qa_cli_push_relative_dotdot_uploads_that_file`
- Steps: from `work/`, with `work/secret` = `WRONG` and `../secret` = `RIGHT`, `vcon push fixture ../secret <remote>`.
- Expected: the remote file is `RIGHT`.
- Actual: `normalize("../secret")` is `"secret"` (`left: "secret"`, `right: "../secret"`). The CLI upload was the bytes `WRONG`.
- Evidence: `src/infra/remote_fs/path.rs:54-56` pops `..` and, on a relative path, discards it when the stack is empty. `src/cli/commands/push.rs:124` normalizes the local path before `plan_push`. Absolute `/a/../../secret` correctly becomes `/secret`. The bug is relative paths that climb above the start of the string.

### 6. A failed rename deletes the file that was already there

- Area: backend
- Severity: **high**
- Test: `qa_rename_failure_must_not_delete_the_existing_destination`
- Steps: destination holds `ORIGINAL`. The writer’s `rename` returns an error after `remove_file` of the destination.
- Expected: `ORIGINAL` remains and the copy fails.
- Actual: failed copy, snapshot of the destination is `None`.
- Evidence: `src/services/files.rs:430-431` deletes the destination and then renames `.part` over it. On failure, `src/services/files.rs:377-381` also deletes `.part`. Both the old file and the new bytes are gone. `rename(2)` on the same directory would replace atomically; the explicit delete makes a network error between the two calls destroy the previous file.

### 7. Two copies onto one path interleave, and one reports success

- Area: backend
- Severity: **high**
- Test: `qa_concurrent_transfers_to_the_same_destination_do_not_overlap`
- Steps: `run_queue` with two jobs, parallelism 2, same destination, sources of `0x11` and `0x22`, each `2*CHUNK`.
- Expected: the writers do not overlap; the file is entirely one source, or the loser fails and the winner is intact.
- Actual: the writes overlapped. Finish states were `(Done, None)` and `(Failed, Some("no such file: /out/same.bin.part"))`. The file was 524288 bytes, first byte `0x22`, byte at `CHUNK` `0x11`.
- Evidence: `src/services/files.rs:474-536` limits concurrency with a semaphore and does not lock by destination. Pressing copy twice, or two files that map to the same name, can publish a mix and mark a job done.

### 8. Copying a FIFO blocks the transfer

- Area: backend
- Severity: **high**
- Test: `qa_fifo_copy_must_not_block`
- Steps: `mkfifo`, `copy_file` it with no writer, 500 ms budget.
- Expected: an error (not a regular file).
- Actual: still blocked at 500 ms (`copying a fifo with no writer blocked the transfer`).
- Evidence: `copy_file` never checks `EntryKind`. `LocalFs::read_at` (`src/infra/remote_fs/local.rs:126`) reads the pipe and waits. The file-browser connect path is awaited on the UI task (`src/tui/mod.rs` `handle_files_command`); a stuck read occupies a blocking worker. This is separate from bug 2: the read does not return, so the size-0 loop is not what stops it.

### 9. A peer that never speaks SFTP hangs the handshake

- Area: backend
- Severity: **high**
- Test: `qa_sftp_handshake_times_out_on_a_stalled_peer`
- Steps: `SftpFs::connect_with_env("/bin/sleep", ["30"], [])` with a 3 s budget. `ConnectTimeout` on real ssh covers the TCP connect, not a process that accepts and then says nothing.
- Expected: the handshake returns an error within the connect budget.
- Actual: still pending after 3 s.
- Evidence: `src/infra/remote_fs/sftp.rs:135` awaits `Sftp::new` with no timeout. `src/tui/mod.rs` awaits `connect_files_host` on the UI loop, so a stalled subsystem freezes the TUI. Garbage input does fail promptly (`qa_sftp_handshake_rejects_garbage_and_does_not_hang`).

### 10. `vcon push` / `vcon pull` throw away the I/O error

- Area: backend
- Severity: **high**
- Tests: `qa_cli_push_reports_the_filesystem_error`, `qa_cli_pull_reports_the_filesystem_error`
- Steps: push and pull against a local `sftp-server` into a directory mode `0555` (exit code 3).
- Expected: stderr includes permission denied.
- Actual: push prints `fixture: copy failed for <path>` and pull prints `failed: <src> → <dst>`. Neither says permission or denied.
- Evidence: `src/cli/commands/push.rs:127-139` uses `is_err()` and replaces the error. `src/cli/commands/pull.rs:84-97` prints the paths only. The TUI path keeps `error.to_string()` (`src/services/files.rs:526`). Disk-full and permission errors are visible from `copy_file` itself (`qa_permission_denied_is_surfaced_and_part_is_removed`, `qa_disk_full_surfaces_and_removes_the_part`) and are then dropped by the CLI.

### 11. The conflict dialog never opens, and the choice does nothing

- Area: frontend
- Severity: **high**
- Tests: `qa_tui_existing_name_does_not_open_the_conflict_dialog`, `qa_tui_conflict_choice_is_not_discarded`
- Steps: Files screen, both panes list `notes.txt`, press `c`. Separately, open the dialog struct and press `o`.
- Expected: ask before replacing an existing name; `o` / `s` / `r` actually overwrite, skip, or rename.
- Actual: `c` immediately returns `Files(Copy { direction: Upload, sources: ["/tmp/local/notes.txt"], dest_dir: "/tmp/remote", host: "web-1" })` with `conflict=None`. `o` clears the dialog and returns `commands=[]`.
- Evidence: `src/tui/app.rs:2161` `files_copy` never stats the destination. `src/tui/app.rs:2105-2109` is `let _ = action`. `conflict` is only ever assigned `None` (no `conflict = Some` in the repo). The dialog text in `src/tui/screens/files.rs:603-610` is unreachable. The engine then overwrites (`src/services/files.rs:430`). The manual documents overwrite for the CLI path shape; the TUI still ships a prompt that cannot be answered.

### 12. An SFTP drop leaves `.part` behind

- Area: backend
- Severity: **medium**
- Test: `qa_sftp_connection_drop_does_not_commit_a_partial_file`
- Steps: proxy in front of `sftp-server` closes after 120000 bytes from the client during a 2 MiB upload.
- Expected: error, no final file, no `.part`.
- Actual: the copy failed (`sftp write <path>.part: Background task failed: read/flush task failed.`), the final path was absent, and `<path>.part` was still on disk.
- Evidence: cleanup is `let _ = dst.remove_file(&part)` (`src/services/files.rs:380`). After the session is dead that remove fails and is ignored. The final name was not replaced, and nothing resumes from `.part` (the next `create` truncates). Local cancel and local I/O errors do delete `.part`.

### 13. Metadata errors are ignored

- Area: backend
- Severity: **medium**
- Test: `qa_metadata_failure_is_currently_ignored` (passes; it records the behavior)
- Steps: source mode `0600`, destination `set_meta` returns an error.
- Expected: the copy fails, or the mode is `0600`.
- Actual: `Ok(())`, destination mode is not `0600` (create default `0644`).
- Evidence: `src/services/files.rs:432` `let _ = dst.set_meta(...)`. When `set_meta` works, `0700` is kept (`qa_mode_is_preserved_on_the_happy_path`). A key copied while chmod fails is left world-readable and reported done.

## Dependency notes (not transfer-logic bugs)

`cargo audit` 0.23.0, exit 0, six warnings, no unpatched vulnerability that fails the default policy:

| Crate | Advisory | Note |
|---|---|---|
| `rustix` 1.1.5 | RUSTSEC-2026-0486, unsound xattr | Patched in `>=1.1.6`. Transitive. |
| `chrono` 0.4.45 | RUSTSEC-2026-0543, unmaintained | No upgrade. |
| `zbus_names` 4.3.5, `zvariant` 5.15.1, `zvariant_derive` 5.15.1 | RUSTSEC-2026-0370/0371/0372, unmaintained | Keyring stack. |
| `async-executor` 1.14.0 | yanked | No fixed version named by the tool. |

Area: schema-architecture. Severity: **low** for this pass (audit did not fail the build; `rustix` is the one with a patch).

## Confirmed behavior that is not a separate defect

- No resume API. Local failure deletes `.part` and a later copy starts at byte 0 (`qa_read_error_keeps_destination_and_deletes_part`).
- Chunk protocol is client-driven offsets, not peer chunk IDs. Out-of-order or duplicate frames are not a message the peer can submit; a peer that returns the wrong bytes for a read is bug 3 (no check).
- Huge `stat` sizes are not allocated up front. They are dangerous only when reads keep returning data (bug 2) or when the peer can actually supply that many bytes (disk fill by a lying size that is non-zero and honored up to `total`).
- Auth on the tested pubkey path fails closed: wrong key, unknown host key, `BatchMode` when no password is saved.
- `unwrap` / `expect` on test-only and UI-layout code were not seen to panic on bad SFTP bytes, a NUL path, or a 256-byte name. `src/services/tunnels.rs:257` `args.pop().expect(...)` is an internal argv invariant, not file contents. `src/infra/remote_fs/sftp.rs:321` uses `u32::try_from(len).unwrap_or(u32::MAX)` and does not panic; `copy_file` only requests `CHUNK`.
- Inventory `.bak` is one generation. Restoring it is manual (`MANUAL.md`). No schema migrations.

## Suspected, not demonstrated

- A stock OpenSSH server will not emit a slash in a filename, so bug 1 is not reachable against an unmodified OpenSSH directory listing. It is reachable against any server that can, and the client has no defense.
- Password SFTP might still mis-handle a bad password or a full keychain. That path was not executed, so this report does not claim it works or that it fails.
