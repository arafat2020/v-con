# vcon QA report — file transfer

## Password push/pull and symlink-parent usability

These tests live on `cursor/qa-file-transfer-134b`. This branch does not contain PR #19's fixes. The product source is unchanged.

`tests/integration_ssh.rs` gained four `#[ignore]` tests. They run only in the integration (docker) job, against the fixture sshd, the same way as the existing ignored tests (`DOCKER=1 cargo test --test integration_ssh -- --ignored`).

| Test | What it checks |
|---|---|
| `password_push_and_pull_round_trip_matches_bytes` | Password-authenticated `vcon push` and `vcon pull`. Byte-for-byte for a 64-byte file, a 256 KiB chunk-boundary file, a 256 KiB + 1 file, and a 3 MiB file. The password must not appear in the transfer output or the log. |
| `wrong_password_push_and_pull_fail_without_leaking` | A wrong saved password exits non-zero, creates no remote file and no local file, and does not put the password in argv, `vcon.log`, `audit.jsonl`, the askpass script, or the error text. |
| `password_pull_replaces_existing_file` | Pull onto a local file that already exists. The published bytes match the remote file and no `.part` sibling remains. |
| `user_chosen_symlink_parent_destination_succeeds` | Usability check, separate from the hostile-listing escape test. A destination the user chose under a symlink parent (Linux stand-in for macOS `/tmp` → `/private/tmp`) must succeed. `ensure_dir` must not refuse that link. |

CI run [44464002119](https://github.com/arafat2020/v-con/actions/runs/44464002119) on `4d1c140`. The integration (docker) job ran `cargo test --test integration_ssh -- --ignored` and finished `ok. 7 passed; 0 failed` in 503.66 s. All four new tests passed:

| Test | Result |
|---|---|
| `password_push_and_pull_round_trip_matches_bytes` | pass |
| `wrong_password_push_and_pull_fail_without_leaking` | pass |
| `password_pull_replaces_existing_file` | pass |
| `user_chosen_symlink_parent_destination_succeeds` | pass |

The three older ignored tests also passed. The ubuntu and macOS `test` jobs are still red: those jobs run the QA assertions that describe behavior this branch does not have. This commit does not change that.

The existing-file pull passed on the current copy path, which still removes the destination before renaming the `.part` into place. The fixture rename succeeded, so the published bytes matched and no `.part` was left. The test does not inject a rename failure after that removal. The symlink-parent pull passed because `ensure_dir` on this branch still accepts a symlink that already exists. PR #19's `ensure_dir` refuses every symlink directory, including a root link such as macOS `/tmp` → `/private/tmp`. That refusal is not in this branch. The new test is the usability check for it, and it is separate from the hostile-listing escape test.

## Retest 2 of PR #19 at `ba2e7be`

Verdict: **NO-GO**.

Bugs A, B, C, and E from the first retest are fixed on the paths that were re-run. Bug D is only partly fixed: overwrite-one and skip-one with three conflicting names never prompt the third name. Password `push` / `pull` was still not executed. That is an unverified critical path.

### Commands on `ba2e7be`

| Command | Result |
|---|---|
| `cargo test -- --test-threads=8` | 253 + 6 + 2 passed, 3 Docker tests ignored. `qa_cli_transfer` 6 passed |
| `VCON_QA_LARGE=1 cargo test --bin vcon -- --nocapture qa_large_file_streams` | pass. 2147483648 bytes in 5.06 s (404 MiB/s), RSS 9076 KiB → 16288 KiB (+7212 KiB), HWM 16288 KiB |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --all-targets -- -D warnings` | pass |
| `cargo audit` 0.23.0 | exit 0, 5 warnings, `rustix` not listed |
| `cargo build --release` | pass |
| `qa_retest_*` carried on this commit, plus a local 30 s greeting probe and a symlink-root probe | all of those passed except the new three-name conflict test |

GitHub Actions run [44461281493](https://github.com/arafat2020/v-con/actions/runs/44461281493), head `ba2e7be`, conclusion success. Every job ran tests:

| Job | What actually ran |
|---|---|
| test (ubuntu-latest) | success. 253 + 6 + 2 passed, 3 ignored, then `qa_cli_transfer` 6 passed |
| test (macos-latest) | success. 252 + 6 + 2 passed, 3 ignored, then 6 passed |
| integration (docker) | success. 3 passed in 257 s: `password_reuse_reprompt_tunnel_and_single_failure`, `exec_true_on_fixture_host`, `status_against_fixture_host`. Not `push` or `pull` |

CI does not set `VCON_QA_LARGE`, so the 2 GiB test returns immediately there. The timed run above is local. This VM has no Docker.

### Test-edit audit against `cursor/qa-file-transfer-134b` (`6a664ec`)

`git diff 6a664ec..ba2e7be -- tests qa` touches only `qa/file_transfer_tests.rs`. `qa/tui_conflict.rs` and `tests/qa_cli_transfer.rs` are unchanged. The four edits are:

1. `qa_metadata_failure_is_currently_ignored` → `qa_metadata_failure_is_surfaced`. Stricter: the copy must return an error containing `metadata`, and the destination must not be published. Legitimate.
2. `qa_sftp_handshake_times_out_on_a_stalled_peer` calls `connect_with_env_timeout(..., 1s)` and still wraps that in a 3 s deadline. The silent peer must still fail. It does not weaken the check, and it no longer depends on the 30 s default. Legitimate.
3. `qa_sftp_connection_drop_does_not_commit_a_partial_file` now has four checks: the copy fails with an error that mentions the dropped connection, the finished path is absent, an unrelated local file is untouched, and a retry replaces a stale `.part` with the full payload. It no longer requires `.part` to disappear before the retry. That matches the removal of the local `remove_file` fallback. It is not weaker about publishing a partial as the finished file.
4. `qa_fifo_copy_must_not_block` holds a nonblocking read end so the test's own write-open in `Drop` can finish. The assertions are unchanged: the copy must finish inside 500 ms and must be an error. A product path that blocked in `read` would still fail that timeout, because `Drop` runs only after the timed future ends. The product rejects a non-regular file from `lstat` before `open` (`src/services/files.rs:485-492`). The helper does not mask that.

No other QA assertion changed.

Grep of `src/` found no transfer behavior keyed on an executable name, a `cfg(test)` branch inside the copy or connect path, or an environment variable that relaxes checks under test. `cfg(test)` in product files only selects test modules or the in-memory keychain. `VCON_*` variables are the real config directory and askpass channel, not a test switch.

### Claims

| Claim | Result |
|---|---|
| C. `connect_with_env_timeout` is public. Default greeting wait is `SFTP_CONNECT_TIMEOUT_SECS` (30). Push, pull, and the TUI pass that same value as ssh `ConnectTimeout`. No product caller passes a shorter greeting wait | **holds.** `src/infra/remote_fs/sftp.rs:24-30` and `:98-103`. Call sites use `connect_with_env`, which uses the 30 s default. `qa_retest_default_greeting_timeout_fails_a_stalled_cli_peer` failed a `sleep 120` peer in about 30 s and under 45 s. A peer that speaks after 3 s succeeds (`qa_retest_sftp_slow_handshake_is_not_killed`). Child exit and stdout EOF are selected ahead of the sleep (`src/infra/remote_fs/sftp.rs:166-218`) |
| C. TUI connect is off the UI task, covered by `ui_loop_ticks_while_sftp_connect_is_pending` | **holds.** `spawn_sftp_connect` (`src/tui/mod.rs:364`) and that test are in the 253 passing tests. Disconnect and a newer connect abort the task (`src/tui/mod.rs:741-743`, `:846-848`). `CancelTransfers` cancels copies, not the greeting |
| E. Local `remove_file` fallback is gone. Only the destination backend removes `.part`, and a failure is logged. `create` truncates under the destination lock | **holds.** `src/services/files.rs:460-468` and `:500-502`. `qa_retest_failed_cleanup_does_not_unlink_a_local_part_file` passed. The drop test's retry compared the full payload |
| FIFO rejected from `lstat` before open | **holds**, as in the FIFO edit above |
| A. Symlink component escape | **fixed.** `qa_retest_symlink_component_in_listing_name_cannot_escape` passed. A symlink directory root that points outside its parent is also refused (`qa_retest_symlink_destination_root_is_not_followed`) |
| B. Equivalent-path lock | **fixed** for `./`. `qa_retest_equivalent_destination_paths_do_not_interleave` passed. The lock key is `normalize`d first (`src/services/files.rs:446-447`) |
| D. Overwrite-one | **not fixed for three names.** Two names work (`qa_retest_overwrite_one_does_not_replace_every_marked_name` passed). Three names drop the third |

### Original bugs 1–13 and A–E

| ID | Status on `ba2e7be` | Evidence |
|---|---|---|
| 1 path `..` | fixed | original hostile test passed |
| 2 size 0 / `/dev/zero` | fixed | both original tests passed |
| 3 early EOF | fixed | short-read test passed |
| 4 symlink copied as bytes | fixed | both symlink tests passed |
| 5 `../secret` | fixed | normalize test and CLI test passed |
| 6 rename deletes the old file | fixed | rename-failure test passed |
| 7 identical-path interleave | fixed | original concurrent test passed |
| 8 FIFO blocks | fixed | FIFO test passed |
| 9 silent handshake hangs | fixed | 1 s injected timeout still fails inside 3 s; default stalls at ~30 s |
| 10 CLI hides the I/O error | fixed | both CLI tests passed |
| 11 dialog never opens | fixed | both original TUI tests passed. Skip and Esc for two names passed |
| 12 drop publishes a partial | fixed | finished path stays absent; retry replaces a stale `.part` |
| 13 `set_meta` ignored | fixed | renamed test passed |
| A symlink-directory escape | fixed | retest passed |
| B `./` lock | fixed | retest passed |
| C 2 s handshake | fixed | 3 s startup succeeds; silent peer still fails |
| D overwrite-one | not fixed for a third name | `qa_retest_three_conflicts_do_not_drop_the_third_name` |
| E local unlink | fixed | retest passed |

### New bug

**Three conflicting names never reach the third prompt**

- Area: frontend
- Severity: **medium**
- Test: `qa_retest_three_conflicts_do_not_drop_the_third_name` (failed on `ba2e7be`)
- Steps: mark `a.txt`, `b.txt`, and `c.txt`, all already present on the other pane. Press `c`, then `o`, then `o` again. Separately, press `c`, `s`, `s`.
- Expected: the second overwrite prompts `c.txt`. After skipping `a` and `b`, the prompt is `c.txt`.
- Actual: the second overwrite prompts `/tmp/remote/a.txt` again. After two skips the prompt is `/tmp/remote/a.txt` again. `c.txt` is not queued and not prompted. Overwrite-all (`O`) did queue all three names.
- Evidence: `queue_files_copy_overwrite_one` keeps only one deferred conflict (`src/tui/app.rs:2270-2283`). `queue_files_copy_skipping` filters only the current path and then `conflicting_destination` walks the still-marked set from the start (`src/tui/app.rs:2210-2248`), so `a` is chosen again.

### Checked and not filed

- A hard-linked pair of destination paths was copied concurrently once and did not interleave. Not repeated enough to call it safe.
- Case-insensitive filesystems cannot be executed on this Linux VM.
- A TOCTOU swap of a directory for a symlink between `lstat` and a later `write_at` was not won. `LocalFs::write_at` reopens the path (`src/infra/remote_fs/local.rs:184-188`), so the window exists. Not demonstrated.
- `ensure_dir` refuses every symlink directory, including a root symlink such as macOS `/tmp` → `/private/tmp`, even though `symlink_leaves_parent` allows a link whose parent is `/` (`src/services/files.rs:551-556` and `:412-417`). macOS CI used canonical temp paths, so this was not executed. It is a suspected macOS regression, not an escape.
- Transfer errors that were inspected name paths and ssh exit text. They do not include a keychain password. The stalled-greeting error was checked for `s3cret` and `password=`.
- The destination lock is a local guard. It is dropped when `copy_file` returns or unwinds. Cancel still returns, so the lock is released. Not a separate stress test.

### Still untested

- Password-authenticated `push` and `pull`. The Docker password test is exec and tunnels.
- Multi-GB over SFTP.
- Release-build throughput. The 404 MiB/s figure is the debug test binary.
- A case-insensitive volume, and a won TOCTOU between `lstat` and `open`.

---

## Retest of PR #19 (`9affe2a`, `cursor/fix-file-transfer-83c7`)

Verdict for PR #19: **NO-GO**.

The 13 original defects are fixed on the paths the first tests cover, `rustix` is 1.1.6, and the developer's suite is green. The patch also adds four new failures: a hostile listing name can still leave the destination through a symlink directory, a failed remote cleanup deletes a local `.part` file, a 3 second SSH startup is killed by a 2 second handshake timeout, and `/out/same.bin` is not the same lock as `/out/./same.bin`. `o` overwrites every marked name.

Checked out `origin/cursor/fix-file-transfer-83c7` in a separate worktree. Product code on this QA branch was not modified.

### Commands on the fix

| Command | Result |
|---|---|
| `cargo test -- --skip qa_retest_` with `VCON_QA_LARGE=1` | bin 244 passed (includes every original `qa_*` test and the 2 GiB copy); `tests/cli.rs` 6 passed; `tests/integration_ssh.rs` 2 passed, 3 ignored (no local Docker); `tests/qa_cli_transfer.rs` 6 passed, including the three that failed before the fix |
| `VCON_QA_LARGE=1 cargo test --bin vcon -- --nocapture qa_large_file_streams` | pass. 2147483648 bytes in 3.52 s (582 MiB/s), RSS 9140 KiB → 16760 KiB (+7620 KiB), HWM 16760 KiB |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --all-targets -- -D warnings` | pass |
| `cargo audit` 0.23.0 | exit 0. `rustix` 1.1.6 is no longer reported. Five warnings remain: unmaintained `chrono`, `zbus_names`, `zvariant`, `zvariant_derive`; yanked `async-executor` 1.14.0 |
| `cargo build --release` | pass |

New tests were run on the fix before they were committed here: 3 passed, 5 failed. The failures are the new bugs below.

### GitHub Actions run 44456088116 (this VM has no Docker)

| Job | Result |
|---|---|
| test (ubuntu-latest) | success. fmt, clippy, 244 + 6 + 6 passed. The 3 Docker tests stayed ignored |
| test (macos-latest) | success. 243 + 6 + 6 passed (one Linux-only test is absent) |
| integration (docker) | success. `password_reuse_reprompt_tunnel_and_single_failure`, `exec_true_on_fixture_host`, and `status_against_fixture_host` passed (3 passed in 254 s) |

CI does not set `VCON_QA_LARGE`, so the 2 GiB test did not run there. It ran locally, above. The Docker job does not exercise `push` or `pull`.

### Were the original tests weakened?

Diff of `tests/` and `qa/` from `d0eed6a` (this branch's first commit) to `9affe2a`:

- `tests/qa_cli_transfer.rs`: no diff.
- `qa/tui_conflict.rs`: no diff on the fix commit.
- `qa/file_transfer_tests.rs`: one test renamed. `qa_metadata_failure_is_currently_ignored` became `qa_metadata_failure_is_surfaced`.

That rename is legitimate. The old test required `Ok(())` and a mode other than `0600`, which recorded the bug. The new test requires an error whose text contains `metadata`, and it requires the destination not to be published (`mode_of("/out")` is `None`). The implementation applies metadata to `.part` and renames only after that succeeds (`src/services/files.rs:508-513` on the fix). No other assertion was deleted or loosened.

### Original bugs

| # | Bug | On `9affe2a` | Evidence |
|---|---|---|---|
| 1 | Listing `../../file` escapes the destination | **fixed** | `qa_hostile_directory_entry_cannot_escape_the_destination` passed. `destination_escapes` rejects a `..` component (`src/services/files.rs:372-374`) before create. Absolute names, `..\..\escaped.bin`, `%2e%2e/...`, and `.` stayed inside dest (`qa_retest_absolute_backslash_and_dot_names_stay_inside_dest`) |
| 2 | Size 0 reader never stops (`/dev/zero`) | **fixed** | `qa_dev_zero_copy_must_stop` and `qa_zero_size_infinite_reader_must_stop` passed. Non-regular files are refused; a size of 0 that still returns bytes is an error (`src/services/files.rs:463-486`). `qa_retest_declared_u64_max_does_not_copy_forever` passed: immediate EOF at `u64::MAX` is an error, not a hang |
| 3 | Early EOF reported as success | **fixed** | `qa_short_read_must_not_report_success` passed. A short or empty read before `total` returns an error (`src/services/files.rs:494-497`) |
| 4 | Symlink copied as a regular file | **fixed** for the symlink itself | `qa_symlink_outside_the_tree_is_not_copied_as_bytes` and `qa_sftp_symlink_is_not_followed` passed. `kind == Symlink` is refused (`src/services/files.rs:460-462`). A symlink directory component is still followed (new bug A) |
| 5 | `push ../secret` uploads `./secret` | **fixed** | `qa_normalize_keeps_relative_parent_segments` and `qa_cli_push_relative_dotdot_uploads_that_file` passed. `normalize("../secret")` stays `../secret` (`src/infra/remote_fs/path.rs:56-61`) |
| 6 | Failed rename deletes the old file | **fixed** | `qa_rename_failure_must_not_delete_the_existing_destination` passed. Rename is no longer preceded by `remove_file` (`src/services/files.rs:511-513`) |
| 7 | Concurrent copies to one path interleave | **fixed** for the identical path string | `qa_concurrent_transfers_to_the_same_destination_do_not_overlap` passed. The lock is keyed by the raw string (`src/services/files.rs:358-368`), so `/out/same.bin` and `/out/./same.bin` still interleave (new bug B) |
| 8 | FIFO copy blocks | **fixed** | `qa_fifo_copy_must_not_block` passed. A FIFO is not a regular file and is refused before `read` |
| 9 | Stalled SFTP handshake hangs | **fixed** for a peer that never speaks | `qa_sftp_handshake_times_out_on_a_stalled_peer` passed. The timeout is 2 s (`src/infra/remote_fs/sftp.rs:20`), which also kills a slow but valid start (new bug C) |
| 10 | CLI drops the I/O error | **fixed** | `qa_cli_push_reports_the_filesystem_error` and `qa_cli_pull_reports_the_filesystem_error` passed. Stderr now includes the underlying error (`src/cli/commands/push.rs:138-140`, `src/cli/commands/pull.rs:96`) |
| 11 | Conflict dialog never opens | **partially fixed** | `qa_tui_existing_name_does_not_open_the_conflict_dialog` and `qa_tui_conflict_choice_is_not_discarded` passed. Skip and Esc were checked with `qa_retest_skip_leaves_the_other_conflict_and_cancel_copies_nothing` (passed on the fix). `o` still replaces every marked name (new bug D) |
| 12 | SFTP drop leaves `.part` | **fixed** for the local `sftp-server` case | `qa_sftp_connection_drop_does_not_commit_a_partial_file` passed. The fallback that makes that pass deletes a local file (new bug E) |
| 13 | `set_meta` failure ignored | **fixed** | `qa_metadata_failure_is_surfaced` passed, as described above |
| — | `rustix` 1.1.5 unsound xattr | **fixed** | `Cargo.toml` pins `rustix = "1.1.6"`. `Cargo.lock` resolves 1.1.6. `cargo audit` no longer lists RUSTSEC-2026-0486 |

### New bugs

**A. A listing name with a slash writes through a symlink directory**

- Area: backend
- Severity: **high**
- Test: `qa_retest_symlink_component_in_listing_name_cannot_escape` (failed on `9affe2a`)
- Steps: `{dest}/inbox/sub` is a symlink to `{tmp}/outside`. A directory listing returns one file named `sub/pwned.txt`. `plan_copies` + `copy_file` into `{dest}`.
- Expected: the name is rejected, or the bytes stay under `{dest}`.
- Actual: `Ok(())`, and the bytes are `{tmp}/outside/pwned.txt`.
- Evidence: `destination_escapes` only looks for a component equal to `..` (`src/services/files.rs:372-374`). `ensure_dir` treats any successful `stat` as a directory (`src/services/files.rs:524-526`). `LocalFs::create` then follows the symlink. The same hostile-listing threat as bug 1, with a slash instead of `..`. Encoded `%2e%2e`, a backslash, an absolute name, and a name that is only `.` did not escape (`qa_retest_absolute_backslash_and_dot_names_stay_inside_dest`).

**B. Equivalent destination paths are not one lock**

- Area: backend
- Severity: **high**
- Test: `qa_retest_equivalent_destination_paths_do_not_interleave` (failed on `9affe2a`; repeated runs)
- Steps: `run_queue` of two jobs, destinations `/out/same.bin` and `/out/./same.bin`, sources `0x11` and `0x22`, each `2*CHUNK`.
- Expected: one lock, and the file is entirely one source.
- Actual: the writes overlapped. In 5 of 8 runs the file mixed the two sources, for example first byte `0x22` and the byte at `CHUNK` `0x11` (`Some((524288, Some(34), Some(17)))`).
- Evidence: `destination_lock` uses the destination string as the map key (`src/services/files.rs:358-363`) and does not normalize it. `LabFs` (and the local filesystem) treat `/out/./same.bin` as `/out/same.bin`.

**C. The handshake timeout is shorter than a slow SSH startup**

- Area: backend
- Severity: **high**
- Test: `qa_retest_sftp_slow_handshake_is_not_killed` (failed on `9affe2a`)
- Steps: the spawned program sleeps 3 seconds, then execs `sftp-server`. `connect_with_env` is given 15 seconds.
- Expected: the session comes up. Three seconds is inside the 30 second `ConnectTimeout` `push` passes to ssh.
- Actual: `sftp handshake timed out waiting for the server`.
- Evidence: `SFTP_HANDSHAKE_TIMEOUT` is 2 seconds (`src/infra/remote_fs/sftp.rs:20-20`) and it wraps `Sftp::new` (`src/infra/remote_fs/sftp.rs:142-154`), which cannot return until ssh has connected and started the subsystem. A peer that never speaks is correctly cut off (bug 9). A peer that speaks after 3 seconds is not.

**D. Overwrite-one replaces every marked name**

- Area: frontend
- Severity: **medium**
- Test: `qa_retest_overwrite_one_does_not_replace_every_marked_name` (failed on `9affe2a`)
- Steps: mark `a.txt` and `b.txt`. Both names already exist on the other pane. Press `c`, then `o`.
- Expected: replace only the file the dialog named, or ask again about the other.
- Actual: one `Copy` command with sources `["/tmp/local/a.txt", "/tmp/local/b.txt"]` and `conflict=None`.
- Evidence: `Overwrite` and `OverwriteAll` share one arm (`src/tui/app.rs:2109-2111`) and `queue_files_copy(false, ...)` copies every selected path (`src/tui/app.rs:2185-2201`). Skip and Esc do what the dialog says (`qa_retest_skip_leaves_the_other_conflict_and_cancel_copies_nothing` passed on the fix).

**E. Failed remote cleanup deletes a local file**

- Area: backend
- Severity: **high**
- Test: `qa_retest_failed_cleanup_does_not_unlink_a_local_part_file` (failed on `9affe2a`)
- Steps: a real file `{tmp}/notes.txt.part` contains `LOCAL-KEEP`. The destination backend's `remove_file` returns an error and `set_meta` fails, so the copy fails.
- Expected: the local file is untouched. It is not the remote `.part`.
- Actual: the file is gone (`read` returned `[]`).
- Evidence: `remove_part` (`src/services/files.rs:378-382`) calls `tokio::fs::remove_file(part)` whenever `dst.remove_file` fails. For an upload, `part` is the remote path plus `.part`. A dead SFTP session then unlinks that path on the local machine. This is what makes bug 12's test pass when `sftp-server` is a local process.

### Still not tested

- `push` / `pull` with a password. The Docker password test that passed is exec, tunnel, and a single auth failure, not a file transfer.
- Multi-GB bytes over SFTP. The 2 GiB run is `LocalFs` to `LocalFs`.
- Release-build throughput. `cargo build --release` succeeded; the 582 MiB/s figure is the debug test binary.
- A file that grows or shrinks during `read_at` was inspected, not executed. The loop stops at the size from `stat` and treats an early empty read as an error, so a shrink fails the copy and growth past that size is not copied.
- No secret-bearing error string showed up. Push and pull now append the I/O error, which includes paths, not file bytes or the keychain password.

---

## First pass (before PR #19)

Verdict at that commit: **NO-GO** for production.

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
