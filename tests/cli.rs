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
