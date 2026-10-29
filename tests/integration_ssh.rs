//! Integration tests against a real OpenSSH server in Docker.
//!
//! These tests are `#[ignore]` by default. Run locally with:
//!
//! ```text
//! docker compose -f tests/fixtures/sshd/docker-compose.yml up -d --build
//! DOCKER=1 cargo test --test integration_ssh -- --ignored
//! docker compose -f tests/fixtures/sshd/docker-compose.yml down -v
//! ```
//!
//! CI sets `DOCKER=1` when `docker info` succeeds and runs the ignored suite
//! in a separate job that may continue on error.

fn docker_available() -> bool {
    std::env::var_os("DOCKER").is_some_and(|value| value != "0")
        && std::process::Command::new("docker")
            .arg("info")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
}

fn skip_without_docker() {
    if !docker_available() {
        eprintln!("skipping: set DOCKER=1 and start tests/fixtures/sshd");
    }
}

/// Placeholder: exec `true` on the fixture host once compose is up.
#[test]
#[ignore = "requires Docker sshd fixture"]
fn exec_true_on_fixture_host() {
    skip_without_docker();
    if !docker_available() {
        return;
    }
    // Scaffold only — wire a temp VCON_CONFIG_DIR and `vcon exec` against
    // the published sshd port when the multi-container suite is built out.
    assert!(
        std::path::Path::new("tests/fixtures/sshd/docker-compose.yml").exists(),
        "docker-compose fixture missing"
    );
}

/// Placeholder: health sample against a live host.
#[test]
#[ignore = "requires Docker sshd fixture"]
fn status_against_fixture_host() {
    skip_without_docker();
    if !docker_available() {
        return;
    }
    assert!(std::path::Path::new("tests/fixtures/sshd/docker-compose.yml").exists());
}
