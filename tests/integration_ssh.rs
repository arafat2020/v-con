//! Integration tests against a real OpenSSH server in Docker.
//!
//! These tests are `#[ignore]` by default. Run locally with:
//!
//! ```text
//! DOCKER=1 cargo test --test integration_ssh -- --ignored
//! ```
//!
//! The tests start `tests/fixtures/sshd` themselves (pubkey auth, user
//! `test`, port 2222) and tear it down afterwards. CI sets `DOCKER=1`
//! when `docker info` succeeds. Default `cargo test` stays offline.

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// `services::files::CHUNK`. One sample is this long and one is one byte longer.
const TRANSFER_CHUNK: usize = 256 * 1024;
const CORRECT_PASSWORD: &str = "Correct-Horse-1";
/// Distinctive on purpose so a leak into argv, a log, or an error is obvious.
const WRONG_PASSWORD: &str = "QaWrong-7f3a-NotInArgv";

const COMPOSE_FILE: &str = "tests/fixtures/sshd/docker-compose.yml";
const PROJECT: &str = "vcon-sshd-test";
const PORT: u16 = 2222;

/// What an ignored Docker test should do for this process.
#[derive(Debug, PartialEq, Eq)]
enum DockerGate {
    /// `DOCKER` is unset. The test passes without touching a daemon.
    Skip,
    /// `DOCKER` is set and `docker` / `docker compose` both work.
    Run,
    /// `DOCKER` is set but docker or compose cannot be used.
    Fail(String),
}

fn decide_docker(env_set: bool, usable: Result<(), String>) -> DockerGate {
    if !env_set {
        DockerGate::Skip
    } else {
        match usable {
            Ok(()) => DockerGate::Run,
            Err(error) => DockerGate::Fail(error),
        }
    }
}

fn docker_usable() -> Result<(), String> {
    for args in [["info"].as_slice(), ["compose", "version"].as_slice()] {
        let output = Command::new("docker")
            .args(args)
            .output()
            .map_err(|error| format!("docker {}: {error}", args.join(" ")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(format!(
                "docker {} exited {}: {} {}",
                args.join(" "),
                output.status,
                stderr.trim(),
                stdout.trim()
            ));
        }
    }
    Ok(())
}

struct Fixture {
    config: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    data: PathBuf,
    /// Stops the sshd container when the fixture drops, including after a panic.
    _compose: ComposeGuard,
}

/// Runs `docker compose down` with the same public key `up` interpolated.
struct ComposeGuard {
    program: PathBuf,
    pubkey: String,
}

impl ComposeGuard {
    fn arm(pubkey: String) -> Self {
        Self {
            program: PathBuf::from("docker"),
            pubkey,
        }
    }
}

impl Drop for ComposeGuard {
    fn drop(&mut self) {
        let _ = compose_down(&self.program, &self.pubkey);
    }
}

impl Fixture {
    fn start() -> Result<Self, String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        // Keep the directory after the TempDir drops; cleanup is compose down.
        let root = root.keep();
        let key = root.join("id_ed25519");
        let status = Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-f",
                &key.display().to_string(),
                "-N",
                "",
                "-q",
            ])
            .status()
            .map_err(|error| format!("ssh-keygen: {error}"))?;
        if !status.success() {
            let _ = fs::remove_dir_all(&root);
            return Err(format!("ssh-keygen exited with {status}"));
        }
        let pubkey = fs::read_to_string(root.join("id_ed25519.pub"))
            .map_err(|error| error.to_string())?
            .trim()
            .to_string();
        let up = Command::new("docker")
            .args(["compose", "-f", COMPOSE_FILE, "-p", PROJECT, "up", "-d"])
            .env("VCON_TEST_PUBLIC_KEY", &pubkey)
            .env("VCON_TEST_SSH_PORT", PORT.to_string())
            .output()
            .map_err(|error| format!("docker compose: {error}"))?;
        // Armed before any later failure or panic so `down` still runs.
        let compose = ComposeGuard::arm(pubkey);
        if !up.status.success() {
            let _ = fs::remove_dir_all(&root);
            return Err(format!(
                "docker compose up failed: {}",
                String::from_utf8_lossy(&up.stderr)
            ));
        }
        if !wait_for_ssh(&key) {
            let _ = fs::remove_dir_all(&root);
            return Err("sshd fixture did not accept pubkey auth".into());
        }
        let config = root.join("config");
        fs::create_dir_all(&config).map_err(|error| error.to_string())?;
        let hosts = format!(
            r#"[[hosts]]
name = "fixture"
address = "127.0.0.1"
port = {PORT}
user = "test"
identity_file = "{key}"
ssh_options = {{ StrictHostKeyChecking = "no", UserKnownHostsFile = "/dev/null", IdentitiesOnly = "yes" }}
"#,
            key = key.display(),
        );
        fs::write(config.join("hosts.toml"), hosts).map_err(|error| error.to_string())?;
        Ok(Self {
            config,
            state: root.join("state"),
            cache: root.join("cache"),
            data: root.join("data"),
            _compose: compose,
        })
    }

    fn vcon(&self) -> assert_cmd::Command {
        let mut cmd = cargo_bin_cmd!("vcon");
        cmd.env("XDG_STATE_HOME", &self.state)
            .env("XDG_CACHE_HOME", &self.cache)
            .env("XDG_DATA_HOME", &self.data)
            .arg("--config-dir")
            .arg(&self.config);
        cmd
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // `compose` drops with this value and stops sshd even if the test panicked.
        if let Some(root) = self.config.parent() {
            let _ = fs::remove_dir_all(root);
        }
    }
}

fn compose_down(program: &Path, pubkey: &str) -> std::io::Result<std::process::ExitStatus> {
    Command::new(program)
        .args(["compose", "-f", COMPOSE_FILE, "-p", PROJECT, "down", "-v"])
        .env("VCON_TEST_PUBLIC_KEY", pubkey)
        .env("VCON_TEST_SSH_PORT", PORT.to_string())
        .status()
}

fn wait_for_ssh(key: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        let ok = Command::new("ssh")
            .args([
                "-i",
                &key.display().to_string(),
                "-p",
                &PORT.to_string(),
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "ConnectTimeout=2",
                "test@127.0.0.1",
                "echo",
                "fixture-ready",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if ok {
            return true;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    false
}

fn with_fixture<F>(test: F)
where
    F: FnOnce(&Fixture),
{
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match decide_docker(std::env::var_os("DOCKER").is_some(), docker_usable()) {
        DockerGate::Skip => {
            eprintln!("skipping: DOCKER is unset");
            return;
        }
        DockerGate::Fail(error) => {
            panic!("DOCKER is set but docker/compose is unusable: {error}");
        }
        DockerGate::Run => {}
    }
    let fixture = Fixture::start().expect("start sshd fixture");
    test(&fixture);
}

/// `vcon exec` against the fixture host returns the command output and status 0.
#[test]
#[ignore = "requires Docker sshd fixture"]
fn exec_true_on_fixture_host() {
    with_fixture(|fixture| {
        fixture
            .vcon()
            .args(["--json", "exec", "fixture", "--", "echo", "vcon-fixture-ok"])
            .assert()
            .success()
            .stdout(
                predicate::str::contains("vcon-fixture-ok")
                    .and(predicate::str::contains("\"ok\":1"))
                    .and(predicate::str::contains("\"code\":0")),
            );
    });
}

/// `vcon status --no-cpu` reports the fixture host reachable.
#[test]
#[ignore = "requires Docker sshd fixture"]
fn status_against_fixture_host() {
    with_fixture(|fixture| {
        fixture
            .vcon()
            .args(["--json", "status", "fixture", "--no-cpu"])
            .assert()
            .success()
            .stdout(
                predicate::str::contains("\"reachable\": true")
                    .and(predicate::str::contains("fixture")),
            );
    });
}

/// Password auth against the fixture: store, reuse, replace, tunnel, one failure.
#[test]
#[ignore = "requires Docker sshd fixture"]
fn password_reuse_reprompt_tunnel_and_single_failure() {
    with_password_fixture(|fix| {
        let host = "alicepw";
        let first = "Correct-Horse-1";
        let second = "New-Battery-2";
        pty_password(
            &fix.env,
            &fix.vcon_args(),
            &["connect", host],
            first,
            "OpenSSH Server",
        );
        assert_eq!(secret(&fix.env, host), first);
        fix.exit_master();
        let reused = fix.vcon(&["x", host, "--", "echo", "RECONNECT-OK"]);
        assert!(reused.status.success(), "{}", reused.stderr);
        assert!(reused.stdout.contains("RECONNECT-OK"), "{}", reused.stdout);
        assert!(!reused.stdout.to_ascii_lowercase().contains("password:"));
        assert!(!reused.stderr.to_ascii_lowercase().contains("password:"));

        fix.chpasswd(second);
        fix.exit_master();
        let before = fix.failed_passwords();
        let stale = fix.vcon(&["x", host, "--", "echo", "SHOULD-NOT"]);
        assert!(!stale.status.success(), "{}", stale.stdout);
        assert!(
            stale.stderr.contains("saved password was rejected"),
            "{}",
            stale.stderr
        );
        let stale_delta = fix.failed_passwords() - before;
        assert_eq!(stale_delta, 1, "stale password must be tried once");

        pty_password(
            &fix.env,
            &fix.vcon_args(),
            &["x", host, "--", "echo", "NEW-OK"],
            second,
            "NEW-OK",
        );
        assert_eq!(secret(&fix.env, host), second);

        fix.allow_forwarding(second);
        let forwarding = fix.forwarding_setting();
        fix.exit_master();
        let local_port = 18000 + (std::process::id() % 2000) as u16;
        let spec = format!("L:{local_port}:127.0.0.1:2222");
        let mut tunnel = fix.spawn(&["tunnel", host, &spec]);
        let banner = read_ssh_banner(local_port, Duration::from_secs(8));
        let states = descendant_states(tunnel.id());
        let still_running = tunnel.try_wait().ok().flatten().is_none();
        let _ = tunnel.kill();
        let _ = tunnel.wait();
        assert!(
            still_running && banner.starts_with("SSH-"),
            "tunnel on {local_port} running={still_running} banner={banner:?} states={states:?} forward={forwarding:?}"
        );
        assert!(
            states.iter().all(|state| state != "T"),
            "tunnel ssh must not be stopped on the tty: {states:?}"
        );

        set_secret(&fix.env, host, "Wrong-Pass-9");
        fix.exit_master();
        let before = fix.failed_passwords();
        let _status = fix.vcon(&["status", host, "--no-cpu"]);
        assert_eq!(
            fix.failed_passwords() - before,
            1,
            "a stale password must not be tried by every parallel probe"
        );
    });
}

/// Password-authenticated `vcon push` and `vcon pull` must match byte for byte.
///
/// Samples: a small file, a file that ends on a transfer-chunk boundary, a
/// file one byte past that boundary, and a few-megabyte file.
#[test]
#[ignore = "requires Docker sshd fixture"]
fn password_push_and_pull_round_trip_matches_bytes() {
    with_password_fixture(|fix| {
        password_only_host(fix);
        set_secret(&fix.env, "alicepw", CORRECT_PASSWORD);
        let home = container_home(&fix.container, "alice");
        let samples = [
            ("small.bin", 64_usize),
            ("chunk.bin", TRANSFER_CHUNK),
            ("chunk-plus.bin", TRANSFER_CHUNK + 1),
            ("few-mb.bin", 3 * 1024 * 1024),
        ];
        let mut transcripts = String::new();
        for (name, len) in samples {
            transcripts.push_str(&round_trip(fix, &home, name, len));
        }
        assert_secret_absent("round-trip output", &transcripts, CORRECT_PASSWORD);
        assert_secret_absent("round-trip logs", &read_logs(fix), CORRECT_PASSWORD);
    });
}

/// A wrong saved password fails the transfer, writes nothing, and stays out of
/// argv, the log, and the error text.
#[test]
#[ignore = "requires Docker sshd fixture"]
fn wrong_password_push_and_pull_fail_without_leaking() {
    with_password_fixture(|fix| {
        password_only_host(fix);
        set_secret(&fix.env, "alicepw", WRONG_PASSWORD);
        let home = container_home(&fix.container, "alice");
        let remote = format!("{home}/vcon-qa/must-not-appear.bin");
        let local_src = fix.root.join("wrong-src.bin");
        fs::write(&local_src, b"should-not-upload").unwrap();
        let local_dir = fix.root.join("wrong-pull-dest");
        fs::create_dir_all(&local_dir).unwrap();

        let watch = ArgvWatch::start(WRONG_PASSWORD);
        let pushed = fix.vcon(&[
            "-y",
            "-vv",
            "push",
            "alicepw",
            &local_src.display().to_string(),
            &remote,
        ]);
        let pulled = fix.vcon(&[
            "-y",
            "-vv",
            "pull",
            "alicepw",
            &remote,
            &local_dir.display().to_string(),
        ]);
        let saw_ssh = watch.finish();

        assert!(
            !pushed.status.success(),
            "wrong-password push must exit non-zero: {}",
            pushed.stdout
        );
        assert!(
            !pulled.status.success(),
            "wrong-password pull must exit non-zero: {}",
            pulled.stdout
        );
        let errors = format!("{}\n{}", pushed.stderr, pulled.stderr);
        assert!(
            errors.contains("saved password was rejected"),
            "expected a clean rejection, got {errors}"
        );
        assert!(
            !remote_exists(&fix.container, &remote),
            "wrong-password push created {remote}"
        );
        assert!(
            !local_dir.join("must-not-appear.bin").exists(),
            "wrong-password pull created a local file"
        );
        assert!(
            !local_dir.join("must-not-appear.bin.part").exists(),
            "wrong-password pull left a local partial"
        );
        assert_secret_absent(
            "wrong-password output",
            &format!("{errors}\n{}", pushed.stdout),
            WRONG_PASSWORD,
        );
        assert_secret_absent("wrong-password logs", &read_logs(fix), WRONG_PASSWORD);
        assert!(
            saw_ssh,
            "the ssh child was never visible in /proc, so argv was not checked"
        );
    });
}

/// Pull onto a file that is already there. The published bytes must be the
/// remote file, and the `.part` sibling must be gone.
#[test]
#[ignore = "requires Docker sshd fixture"]
fn password_pull_replaces_existing_file() {
    with_password_fixture(|fix| {
        password_only_host(fix);
        set_secret(&fix.env, "alicepw", CORRECT_PASSWORD);
        let home = container_home(&fix.container, "alice");
        let name = "replace-me.bin";
        let remote = format!("{home}/vcon-qa/{name}");
        let local_src = fix.root.join(name);
        let bytes = patterned(8 * 1024);
        fs::write(&local_src, &bytes).unwrap();
        let pushed = fix.vcon(&[
            "-y",
            "push",
            "alicepw",
            &local_src.display().to_string(),
            &remote,
        ]);
        assert!(pushed.status.success(), "setup push: {}", pushed.stderr);

        let dest_dir = fix.root.join("replace-dest");
        fs::create_dir_all(&dest_dir).unwrap();
        let dest_file = dest_dir.join(name);
        let old = b"old-local-bytes-must-be-replaced";
        fs::write(&dest_file, old).unwrap();
        let pulled = fix.vcon(&[
            "-y",
            "-vv",
            "pull",
            "alicepw",
            &remote,
            &dest_dir.display().to_string(),
        ]);
        assert!(
            pulled.status.success(),
            "pull onto an existing file: {}",
            pulled.stderr
        );
        let got = fs::read(&dest_file).unwrap();
        assert_eq!(
            got, bytes,
            "existing file was not replaced with the remote bytes"
        );
        assert_ne!(got, old);
        assert!(
            !dest_dir.join(format!("{name}.part")).exists(),
            "partial file left beside the replaced file"
        );
        assert!(dest_file.is_file(), "replaced path is not a regular file");
    });
}

/// Usability check. This is not the hostile-listing escape test.
///
/// macOS `/tmp` is a symlink to `/private/tmp`. A destination the user chose
/// under a link of that shape must succeed. `ensure_dir` refusing every
/// symlink directory would break that ordinary path. On Linux the same shape
/// is a directory we create and a symlink whose parent is that tree's root.
#[test]
#[ignore = "requires Docker sshd fixture"]
fn user_chosen_symlink_parent_destination_succeeds() {
    with_fixture(|fixture| {
        let container = container_named("vcon-sshd-test-sshd");
        let home = container_home(&container, "test");
        let name = "symlink-usability.bin";
        let bytes = b"user-picked-destination-under-symlink\n".to_vec();
        let local_src = fixture.config.parent().unwrap().join(name);
        fs::write(&local_src, &bytes).unwrap();
        let remote = format!("{home}/vcon-qa/{name}");
        fixture
            .vcon()
            .args([
                "-y",
                "push",
                "fixture",
                &local_src.display().to_string(),
                &remote,
            ])
            .assert()
            .success();

        // `tmp` stands in for macOS `/tmp` → `/private/tmp`.
        let base = fixture.config.parent().unwrap().join("symlink-root");
        let real = base.join("private/tmp");
        fs::create_dir_all(&real).unwrap();
        let link = base.join("tmp");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let chosen = link.join("inbox");
        let mut pull = fixture.vcon();
        let output = pull
            .args([
                "-y",
                "pull",
                "fixture",
                &remote,
                &chosen.display().to_string(),
            ])
            .output()
            .expect("pull");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "user-chosen destination under a symlink parent must succeed: {stderr}"
        );
        assert!(
            !stderr.contains("refusing to use symlink"),
            "ensure_dir refused a symlink the user picked: {stderr}"
        );
        let landed = real.join("inbox").join(name);
        assert_eq!(
            fs::read(&landed).expect("bytes through the real directory"),
            bytes,
            "file did not land through the symlink"
        );
        assert_eq!(fs::read(chosen.join(name)).unwrap(), bytes);
    });
}

struct PasswordFix {
    root: PathBuf,
    env: Vec<(String, String)>,
    port: u16,
    container: String,
}

struct CmdOut {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

impl PasswordFix {
    fn vcon_args(&self) -> Vec<String> {
        vec![
            assert_cmd::cargo::cargo_bin("vcon").display().to_string(),
            "--config-dir".into(),
            self.root.join("config").display().to_string(),
        ]
    }

    fn vcon(&self, args: &[&str]) -> CmdOut {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("vcon"));
        cmd.args(["--config-dir"])
            .arg(self.root.join("config"))
            .args(args);
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        cmd.env_remove("SSH_AUTH_SOCK");
        let out = cmd.output().expect("vcon");
        CmdOut {
            status: out.status,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn spawn(&self, args: &[&str]) -> std::process::Child {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("vcon"));
        cmd.args(["--config-dir"])
            .arg(self.root.join("config"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        cmd.env_remove("SSH_AUTH_SOCK");
        cmd.spawn().expect("spawn vcon")
    }

    fn chpasswd(&self, password: &str) {
        let status = Command::new("docker")
            .args(["exec", "-i", &self.container, "chpasswd"])
            .stdin(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(mut stdin) = child.stdin.take() {
                    writeln!(stdin, "alice:{password}")?;
                }
                child.wait()
            })
            .expect("chpasswd");
        assert!(status.success(), "chpasswd failed");
    }

    fn allow_forwarding(&self, password: &str) {
        // The image starts sshd with forwarding disabled. Edit the file, then
        // kill the listener so s6 starts a process that reads the new setting.
        let script = r#"
sed -i 's/AllowTcpForwarding no/AllowTcpForwarding yes/' /config/sshd/sshd_config
for d in /proc/[0-9]*; do
  cmd=$(tr '\0' ' ' < "$d/cmdline" 2>/dev/null || true)
  case "$cmd" in
    sshd.pam:*) kill "${d#/proc/}" || true ;;
  esac
done
exit 0
"#;
        let status = Command::new("docker")
            .args(["exec", &self.container, "sh", "-c", script])
            .status()
            .expect("sshd restart");
        assert!(status.success(), "could not restart sshd");
        assert!(
            wait_for_password(self.port, password),
            "sshd did not come back with forwarding enabled"
        );
    }

    fn forwarding_setting(&self) -> String {
        let out = Command::new("docker")
            .args([
                "exec",
                &self.container,
                "grep",
                "AllowTcpForwarding",
                "/config/sshd/sshd_config",
            ])
            .output()
            .expect("sshd config");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn failed_passwords(&self) -> usize {
        let out = Command::new("docker")
            .args([
                "exec",
                &self.container,
                "grep",
                "-c",
                "Failed password",
                "/config/logs/openssh/current",
            ])
            .output()
            .expect("sshd log");
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .unwrap_or(0)
    }

    fn exit_master(&self) {
        let cm = self.root.join("cache/vcon/cm");
        let _ = Command::new("ssh")
            .args([
                "-o",
                &format!(
                    "ControlPath={}/cm/%C",
                    self.root.join("cache/vcon").display()
                ),
                "-o",
                "ControlMaster=no",
                "-p",
                &self.port.to_string(),
                "-O",
                "exit",
                "alice@127.0.0.1",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = cm;
        let _ = Command::new("pkill")
            .args([
                "-f",
                &format!("ControlPath={}/", self.root.join("cache/vcon/cm").display()),
            ])
            .status();
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn with_password_fixture(test: impl FnOnce(&PasswordFix)) {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match decide_docker(std::env::var_os("DOCKER").is_some(), docker_usable()) {
        DockerGate::Skip => {
            eprintln!("skipping: DOCKER is unset");
            return;
        }
        DockerGate::Fail(error) => panic!("DOCKER is set but docker/compose is unusable: {error}"),
        DockerGate::Run => {}
    }
    for tool in [
        "dbus-launch",
        "gnome-keyring-daemon",
        "secret-tool",
        "sshpass",
        "python3",
    ] {
        require_password_tool(tool);
    }
    let root = tempfile::tempdir().unwrap().keep();
    let key = root.join("id_ed25519");
    let status = Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-f",
            &key.display().to_string(),
            "-N",
            "",
            "-q",
        ])
        .status()
        .expect("ssh-keygen");
    assert!(status.success());
    let pubkey = fs::read_to_string(root.join("id_ed25519.pub")).unwrap();
    let port = 2234u16;
    let project = "vcon-sshd-pwtest";
    let up = Command::new("docker")
        .args([
            "compose",
            "-f",
            COMPOSE_FILE,
            "-p",
            project,
            "--profile",
            "password",
            "up",
            "-d",
            "sshd-pw",
        ])
        .env("VCON_TEST_PUBLIC_KEY", pubkey.trim())
        .env("VCON_TEST_PASSWORD", "Correct-Horse-1")
        .env("VCON_TEST_PW_PORT", port.to_string())
        .env("VCON_TEST_SSH_PORT", "2224")
        .output()
        .expect("compose up");
    let container = format!("{project}-sshd-pw-1");
    let fix_env = start_keyring();
    let fix = PasswordFix {
        root: root.clone(),
        env: {
            let mut env = fix_env;
            env.push((
                "XDG_STATE_HOME".into(),
                root.join("state").display().to_string(),
            ));
            env.push((
                "XDG_CACHE_HOME".into(),
                root.join("cache").display().to_string(),
            ));
            env.push((
                "XDG_DATA_HOME".into(),
                root.join("data").display().to_string(),
            ));
            env
        },
        port,
        container: container.clone(),
    };
    let down_root = root.clone();
    let down_container = container;
    let down = move || {
        let _ = Command::new("docker")
            .args(["rm", "-f", &down_container])
            .status();
        let _ = Command::new("docker")
            .args(["compose", "-f", COMPOSE_FILE, "-p", project, "down", "-v"])
            .env("VCON_TEST_PUBLIC_KEY", pubkey.trim())
            .env("VCON_TEST_PASSWORD", "Correct-Horse-1")
            .env("VCON_TEST_PW_PORT", port.to_string())
            .env("VCON_TEST_SSH_PORT", "2224")
            .status();
        let _ = fs::remove_dir_all(&down_root);
    };
    if !up.status.success() {
        down();
        panic!("compose up failed: {}", String::from_utf8_lossy(&up.stderr));
    }
    if !wait_for_password(port, "Correct-Horse-1") {
        down();
        panic!("password sshd did not accept Correct-Horse-1");
    }
    fs::create_dir_all(root.join("config")).unwrap();
    fs::write(
        root.join("config/hosts.toml"),
        format!(
            r#"[[hosts]]
name = "alicepw"
address = "127.0.0.1"
port = {port}
user = "alice"
ssh_options = {{ StrictHostKeyChecking = "no", UserKnownHostsFile = "/dev/null" }}
"#
        ),
    )
    .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| test(&fix)));
    down();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn wait_for_password(port: u16, password: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        let ok = Command::new("sshpass")
            .args([
                "-p",
                password,
                "ssh",
                "-p",
                &port.to_string(),
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "PreferredAuthentications=password",
                "-o",
                "PubkeyAuthentication=no",
                "-o",
                "NumberOfPasswordPrompts=1",
                "-o",
                "ConnectTimeout=2",
                "alice@127.0.0.1",
                "echo",
                "ready",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if ok {
            return true;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    false
}

/// `DOCKER=1` means the password test must run. A missing tool is a failure.
fn require_password_tool(name: &str) {
    let found = Command::new("sh")
        .args(["-c", &format!("command -v {name}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !found {
        panic!("DOCKER is set but `{name}` was not found");
    }
}

fn start_keyring() -> Vec<(String, String)> {
    let launch = Command::new("dbus-launch")
        .arg("--sh-syntax")
        .output()
        .unwrap_or_else(|error| panic!("DOCKER is set but `dbus-launch` was not found: {error}"));
    if !launch.status.success() {
        panic!(
            "keyring could not start: dbus-launch failed: {}",
            String::from_utf8_lossy(&launch.stderr).trim()
        );
    }
    let mut env = parse_shell_env(&String::from_utf8_lossy(&launch.stdout));
    let mut daemon = Command::new("gnome-keyring-daemon");
    daemon
        .args(["--unlock", "--components=secrets"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &env {
        daemon.env(key, value);
    }
    let mut child = daemon.spawn().unwrap_or_else(|error| {
        panic!("DOCKER is set but `gnome-keyring-daemon` was not found: {error}")
    });
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(b"\n");
    }
    let out = child.wait_with_output().unwrap_or_else(|error| {
        panic!("keyring could not start: gnome-keyring-daemon failed: {error}")
    });
    if !out.status.success() {
        panic!(
            "keyring could not start: gnome-keyring-daemon failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    env.extend(parse_shell_env(&String::from_utf8_lossy(&out.stdout)));
    if !env.iter().any(|(key, _)| key == "DBUS_SESSION_BUS_ADDRESS") {
        panic!("keyring could not start: dbus session missing");
    }
    env
}

fn parse_shell_env(text: &str) -> Vec<(String, String)> {
    let mut env = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_end_matches(';');
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            continue;
        }
        let value = value.trim_matches('\'').trim_matches('"');
        env.push((key.to_string(), value.to_string()));
    }
    env
}

fn secret(env: &[(String, String)], account: &str) -> String {
    let mut cmd = Command::new("secret-tool");
    cmd.args(["lookup", "service", "vcon", "username", account]);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("secret-tool");
    assert!(out.status.success(), "no keychain entry for {account}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn set_secret(env: &[(String, String)], account: &str, password: &str) {
    let mut cmd = Command::new("secret-tool");
    cmd.args([
        "store", "--label", "vcon", "service", "vcon", "username", account,
    ])
    .stdin(Stdio::piped());
    for (key, value) in env {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().expect("secret-tool store");
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(password.as_bytes()).unwrap();
    }
    let status = child.wait().unwrap();
    assert!(status.success(), "could not store a keychain secret");
}

fn pty_password(
    env: &[(String, String)],
    vcon: &[String],
    args: &[&str],
    password: &str,
    marker: &str,
) {
    let script = r#"
import os, pty, select, time, sys
argv = sys.argv[1:]
marker = os.environ["VCON_MARKER"].encode()
password = os.environ["VCON_PW"].encode() + b"\n"
pid, fd = pty.fork()
if pid == 0:
    os.execvp(argv[0], argv)
buf = b""
sent = 0
deadline = time.time() + 30
while time.time() < deadline:
    ready, _, _ = select.select([fd], [], [], 0.3)
    if fd not in ready:
        wpid, _ = os.waitpid(pid, os.WNOHANG)
        if wpid:
            break
        continue
    try:
        chunk = os.read(fd, 4096)
    except OSError:
        break
    if not chunk:
        break
    buf += chunk
    if sent == 0 and b"password:" in buf.lower():
        os.write(fd, password)
        sent = 1
    if marker in buf:
        os.write(fd, b"exit\n")
        time.sleep(0.3)
        break
try:
    os.kill(pid, 15)
except ProcessLookupError:
    pass
try:
    os.waitpid(pid, 0)
except ChildProcessError:
    pass
sys.stderr.buffer.write(buf)
sys.exit(0 if sent == 1 and marker in buf else 1)
"#;
    let mut cmd = Command::new("python3");
    cmd.arg("-c")
        .arg(script)
        .args(vcon)
        .args(args)
        .env("VCON_PW", password)
        .env("VCON_MARKER", marker);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("python pty");
    assert!(
        out.status.success(),
        "pty session failed\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn read_ssh_banner(port: u16, limit: Duration) -> String {
    let deadline = Instant::now() + limit;
    let mut last = String::new();
    while Instant::now() < deadline {
        if let Ok(stream) = std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_secs(1),
        ) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            use std::io::Read;
            let mut stream = stream;
            let mut buf = [0u8; 32];
            if let Ok(n) = stream.read(&mut buf)
                && n > 0
            {
                last = String::from_utf8_lossy(&buf[..n]).into_owned();
                if last.starts_with("SSH-") {
                    return last;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    last
}

fn descendant_states(pid: u32) -> Vec<String> {
    let mut states = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return states;
    };
    for entry in entries.flatten() {
        let Ok(id) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let stat = fs::read_to_string(format!("/proc/{id}/stat")).unwrap_or_default();
        let mut parts = stat.split_whitespace();
        let _ = parts.next();
        let _ = parts.next();
        let state = parts.next().unwrap_or("").to_string();
        let ppid = parts.next().unwrap_or("");
        if ppid == pid.to_string() {
            states.push(state);
        }
    }
    states
}

fn password_only_host(fix: &PasswordFix) {
    fs::write(
        fix.root.join("config/hosts.toml"),
        format!(
            r#"[[hosts]]
name = "alicepw"
address = "127.0.0.1"
port = {port}
user = "alice"
ssh_options = {{ StrictHostKeyChecking = "no", UserKnownHostsFile = "/dev/null", PubkeyAuthentication = "no", PreferredAuthentications = "password", IdentitiesOnly = "yes" }}
"#,
            port = fix.port,
        ),
    )
    .unwrap();
}

fn container_named(prefix: &str) -> String {
    let out = Command::new("docker")
        .args([
            "ps",
            "--format",
            "{{.Names}}",
            "--filter",
            &format!("name={prefix}"),
        ])
        .output()
        .expect("docker ps");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    assert_eq!(names.len(), 1, "containers matching {prefix}: {names:?}");
    names[0].clone()
}

fn container_home(container: &str, user: &str) -> String {
    let out = Command::new("docker")
        .args(["exec", container, "getent", "passwd", user])
        .output()
        .expect("getent");
    assert!(
        out.status.success(),
        "getent {user}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let line = String::from_utf8_lossy(&out.stdout);
    let home = line.trim().split(':').nth(5).unwrap_or("").to_string();
    assert!(home.starts_with('/'), "home for {user}: {line}");
    home
}

fn remote_exists(container: &str, path: &str) -> bool {
    Command::new("docker")
        .args(["exec", container, "test", "-e", path])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn remote_bytes(container: &str, path: &str) -> Vec<u8> {
    let out = Command::new("docker")
        .args(["exec", container, "cat", path])
        .output()
        .expect("docker exec cat");
    assert!(
        out.status.success(),
        "cat {path}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn patterned(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
}

fn round_trip(fix: &PasswordFix, home: &str, name: &str, len: usize) -> String {
    let local = fix.root.join("src").join(name);
    fs::create_dir_all(local.parent().unwrap()).unwrap();
    let bytes = patterned(len);
    fs::write(&local, &bytes).unwrap();
    let remote = format!("{home}/vcon-qa/{name}");
    let pushed = fix.vcon(&[
        "-y",
        "-vv",
        "push",
        "alicepw",
        &local.display().to_string(),
        &remote,
    ]);
    assert!(
        pushed.status.success(),
        "push {name} ({len} bytes): {}",
        pushed.stderr
    );
    assert_eq!(
        remote_bytes(&fix.container, &remote),
        bytes,
        "remote bytes differ after push of {name}"
    );
    let dest = fix.root.join("pulled").join(name);
    fs::create_dir_all(&dest).unwrap();
    let pulled = fix.vcon(&[
        "-y",
        "-vv",
        "pull",
        "alicepw",
        &remote,
        &dest.display().to_string(),
    ]);
    assert!(
        pulled.status.success(),
        "pull {name} ({len} bytes): {}",
        pulled.stderr
    );
    assert_eq!(
        fs::read(dest.join(name)).unwrap(),
        bytes,
        "local bytes differ after pull of {name}"
    );
    assert!(!dest.join(format!("{name}.part")).exists());
    format!("{}\n{}", pushed.stderr, pulled.stderr)
}

fn read_logs(fix: &PasswordFix) -> String {
    let mut text = String::new();
    for path in [
        fix.root.join("state/vcon/vcon.log"),
        fix.root.join("state/vcon/audit.jsonl"),
        fix.root.join("cache/vcon/askpass"),
    ] {
        if let Ok(body) = fs::read_to_string(&path) {
            text.push_str(&body);
            text.push('\n');
        }
    }
    text
}

fn assert_secret_absent(label: &str, text: &str, secret: &str) {
    assert!(!text.contains(secret), "{label} contains the password");
}

/// Watches `/proc` while a transfer runs. The password must not appear in any
/// process argv. `finish` is true when an `ssh` child was actually observed.
struct ArgvWatch {
    stop: Arc<AtomicBool>,
    saw_secret: Arc<AtomicBool>,
    saw_ssh: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ArgvWatch {
    fn start(secret: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let saw_secret = Arc::new(AtomicBool::new(false));
        let saw_ssh = Arc::new(AtomicBool::new(false));
        let secret = secret.as_bytes().to_vec();
        let stop_flag = Arc::clone(&stop);
        let secret_flag = Arc::clone(&saw_secret);
        let ssh_flag = Arc::clone(&saw_ssh);
        let handle = std::thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                scan_argv(&secret, &secret_flag, &ssh_flag);
                std::thread::sleep(Duration::from_millis(1));
            }
            scan_argv(&secret, &secret_flag, &ssh_flag);
        });
        Self {
            stop,
            saw_secret,
            saw_ssh,
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> bool {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        assert!(
            !self.saw_secret.load(Ordering::Relaxed),
            "password appeared in a process argv"
        );
        self.saw_ssh.load(Ordering::Relaxed)
    }
}

fn scan_argv(secret: &[u8], saw_secret: &AtomicBool, saw_ssh: &AtomicBool) {
    let Ok(proc) = fs::read_dir("/proc") else {
        return;
    };
    for entry in proc.flatten() {
        let cmdline = fs::read(entry.path().join("cmdline")).unwrap_or_default();
        if !secret.is_empty() && cmdline.windows(secret.len()).any(|window| window == secret) {
            saw_secret.store(true, Ordering::Relaxed);
        }
        if cmdline
            .split(|byte| *byte == 0)
            .any(|arg| arg == b"ssh" || arg.ends_with(b"/ssh"))
        {
            saw_ssh.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod gate {
    use super::*;

    #[test]
    fn unset_docker_skips_and_set_without_daemon_fails() {
        assert_eq!(
            decide_docker(false, Ok(())),
            DockerGate::Skip,
            "unset DOCKER skips even when a daemon exists"
        );
        assert_eq!(decide_docker(false, Err("down".into())), DockerGate::Skip);
        assert!(matches!(
            decide_docker(true, Err("docker info exited 1".into())),
            DockerGate::Fail(_)
        ));
        assert_eq!(decide_docker(true, Ok(())), DockerGate::Run);
    }

    #[cfg(unix)]
    #[test]
    fn compose_guard_runs_down_when_the_test_panics() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("called");
        let script = dir.path().join("docker");
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$VCON_TEST_PUBLIC_KEY\" >> '{}'\nprintf '%s\\n' \"$VCON_TEST_SSH_PORT\" >> '{}'\nprintf '%s\\n' \"$*\" >> '{}'\n",
            log.display(),
            log.display(),
            log.display()
        );
        fs::write(&script, body).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut guard = ComposeGuard::arm("ssh-ed25519 AAAATEST".into());
            guard.program = script.clone();
            panic!("fixture test panicked");
        }));
        assert!(caught.is_err());
        let text = fs::read_to_string(&log).expect("down should have recorded the call");
        assert!(
            text.contains("ssh-ed25519 AAAATEST"),
            "down must receive VCON_TEST_PUBLIC_KEY, got {text}"
        );
        assert!(text.contains("2222"), "{text}");
        assert!(text.contains("down"), "{text}");
    }
}
