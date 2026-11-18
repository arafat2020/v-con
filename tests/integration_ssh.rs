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
use std::time::{Duration, Instant};

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
