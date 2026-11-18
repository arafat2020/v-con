//! End-to-end CLI checks with a temporary `VCON_CONFIG_DIR`.

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn vcon() -> assert_cmd::Command {
    cargo_bin_cmd!("vcon")
}

fn with_config_dir(dir: &Path) -> assert_cmd::Command {
    let mut cmd = vcon();
    cmd.env("VCON_CONFIG_DIR", dir);
    cmd
}

#[test]
fn init_check_list_doctor_completions_log_snippet() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    with_config_dir(root)
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("Created"));

    with_config_dir(root)
        .arg("check")
        .assert()
        .success()
        .stdout(predicate::str::contains("valid"));

    with_config_dir(root)
        .args(["list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("example"));

    with_config_dir(root)
        .arg("doctor")
        .assert()
        .code(predicate::in_iter([0, 1]))
        .stdout(predicate::str::contains("[OK]").or(predicate::str::contains("[WARN]")));

    with_config_dir(root)
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("vcon"));

    with_config_dir(root).arg("log").assert().success();

    with_config_dir(root)
        .args(["snippet", "list"])
        .assert()
        .success();
}

#[test]
fn man_prints_troff() {
    let dir = tempdir().unwrap();
    with_config_dir(dir.path())
        .arg("man")
        .assert()
        .success()
        .stdout(predicate::str::contains(".TH").or(predicate::str::contains("vcon")));
}

#[test]
fn check_fails_on_corrupt_hosts_toml() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root).unwrap();
    fs::write(root.join("hosts.toml"), "[[hosts]\nname = \"broken\"\n").unwrap();

    with_config_dir(root)
        .arg("check")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("hosts.toml")
                .and(predicate::str::contains(":1:").or(predicate::str::contains("parse"))),
        );
}

#[test]
fn check_accepts_missing_hosts_as_empty_inventory() {
    let dir = tempdir().unwrap();
    with_config_dir(dir.path())
        .arg("check")
        .assert()
        .success()
        .stdout(predicate::str::contains("valid"));
}

#[test]
fn list_json_on_empty_inventory() {
    let dir = tempdir().unwrap();
    with_config_dir(dir.path())
        .args(["list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains('['));
}

/// Push and pull record `transfer` even when ssh cannot complete the copy.
#[test]
fn push_and_pull_write_transfer_audit() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let config = root.join("config");
    let state = root.join("state");
    let cache = root.join("cache");
    let data = root.join("data");
    fs::create_dir_all(&config).unwrap();
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let ssh = bin.join("ssh");
    fs::write(&ssh, "#!/bin/sh\nexit 255\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        config.join("hosts.toml"),
        "[[hosts]]\nname = \"web-1\"\naddress = \"127.0.0.1\"\nuser = \"test\"\n",
    )
    .unwrap();
    let local = root.join("payload.txt");
    fs::write(&local, "hello\n").unwrap();
    let dest = root.join("pulled");
    fs::create_dir_all(&dest).unwrap();

    let mut push = with_config_dir(&config);
    isolate(&mut push, &state, &cache, &data, &bin);
    push.args(["-y", "push", "web-1", local.to_str().unwrap(), "/var/www"])
        .assert()
        .code(3);

    let mut pull = with_config_dir(&config);
    isolate(&mut pull, &state, &cache, &data, &bin);
    pull.args(["pull", "web-1", "/var/log/app.log", dest.to_str().unwrap()])
        .assert()
        .code(1);

    let audit = fs::read_to_string(state.join("vcon/audit.jsonl")).unwrap();
    let lines: Vec<&str> = audit.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(lines.len(), 2, "{audit}");
    assert!(lines[0].contains("\"action\":\"transfer\""), "{audit}");
    assert!(lines[0].contains("\"direction\":\"upload\""), "{audit}");
    assert!(lines[0].contains("\"src\":"), "{audit}");
    assert!(lines[0].contains(local.to_str().unwrap()), "{audit}");
    assert!(lines[0].contains("\"dst\":\"/var/www\""), "{audit}");
    assert!(lines[0].contains("web-1"), "{audit}");
    assert!(lines[0].contains("/var/www"), "{audit}");
    assert!(
        lines[0].contains("\"web-1\":255"),
        "audit should keep ssh exit 255, got {audit}"
    );
    assert!(lines[1].contains("\"action\":\"transfer\""), "{audit}");
    assert!(lines[1].contains("\"direction\":\"download\""), "{audit}");
    assert!(lines[1].contains("\"src\":\"/var/log/app.log\""), "{audit}");
    assert!(lines[1].contains(dest.to_str().unwrap()), "{audit}");
    assert!(
        lines[1].contains("\"web-1\":255"),
        "audit should keep ssh exit 255, got {audit}"
    );

    let mut log = with_config_dir(&config);
    isolate(&mut log, &state, &cache, &data, &bin);
    log.args(["log", "--action", "transfer"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("transfer")
                .and(predicate::str::contains("upload"))
                .and(predicate::str::contains("download"))
                .and(predicate::str::contains("255")),
        );
}

fn isolate(cmd: &mut assert_cmd::Command, state: &Path, cache: &Path, data: &Path, bin: &Path) {
    let path = std::env::join_paths(std::iter::once(bin.to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    cmd.env("XDG_STATE_HOME", state)
        .env("XDG_CACHE_HOME", cache)
        .env("XDG_DATA_HOME", data)
        .env("PATH", path);
}
