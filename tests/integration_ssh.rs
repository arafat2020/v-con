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

fn docker_available() -> bool {
    std::env::var_os("DOCKER").is_some_and(|value| value != "0")
        && Command::new("docker")
            .arg("info")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
}

struct Fixture {
    config: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    data: PathBuf,
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
        if !up.status.success() {
            let _ = compose_down();
            let _ = fs::remove_dir_all(&root);
            return Err(format!(
                "docker compose up failed: {}",
                String::from_utf8_lossy(&up.stderr)
            ));
        }
        if !wait_for_ssh(&key) {
            let _ = compose_down();
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
        let _ = compose_down();
        if let Some(root) = self.config.parent() {
            let _ = fs::remove_dir_all(root);
        }
    }
}

fn compose_down() -> std::io::Result<std::process::ExitStatus> {
    Command::new("docker")
        .args(["compose", "-f", COMPOSE_FILE, "-p", PROJECT, "down", "-v"])
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
    if !docker_available() {
        eprintln!("skipping: set DOCKER=1 with a working docker daemon");
        return;
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
