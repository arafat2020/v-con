//! QA: `vcon push` / `vcon pull` against a local sftp-server and a local sshd.
//! No production host is contacted. `127.0.0.1` only.

use assert_cmd::cargo::cargo_bin_cmd;
use std::fs;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

fn vcon() -> assert_cmd::Command {
    cargo_bin_cmd!("vcon")
}

struct Isolated {
    root: PathBuf,
}

impl Isolated {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir").keep();
        for name in ["config", "state", "cache", "data", "bin", "home"] {
            fs::create_dir_all(root.join(name)).unwrap();
        }
        fs::write(root.join("config/config.toml"), "multiplexing = false\n").unwrap();
        Self { root }
    }

    fn config(&self) -> PathBuf {
        self.root.join("config")
    }

    fn apply(&self, cmd: &mut assert_cmd::Command, path_prefix: Option<&Path>) {
        let mut paths = Vec::new();
        if let Some(prefix) = path_prefix {
            paths.push(prefix.to_path_buf());
        }
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        cmd.env("VCON_CONFIG_DIR", self.config())
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("HOME", self.root.join("home"))
            .env("PATH", std::env::join_paths(paths).unwrap());
    }
}

fn write_hosts(config: &Path, body: &str) {
    fs::write(config.join("hosts.toml"), body).unwrap();
}

fn sftp_server() -> Option<PathBuf> {
    [
        "/usr/lib/openssh/sftp-server",
        "/usr/libexec/openssh/sftp-server",
        "/usr/libexec/sftp-server",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

fn install_fake_ssh(bin: &Path, server: &Path) {
    let ssh = bin.join("ssh");
    // sftp-server speaks the subsystem on stdin/stdout. ssh's own argv would
    // make it exit before the handshake.
    fs::write(&ssh, format!("#!/bin/sh\nexec {}\n", server.display())).unwrap();
    let mut perms = fs::metadata(&ssh).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&ssh, perms).unwrap();
}

fn host_for_fake() -> &'static str {
    r#"[[hosts]]
name = "fixture"
address = "127.0.0.1"
user = "test"
"#
}

struct Sshd {
    /// Kept so the host key and config outlive `sshd`.
    #[allow(dead_code)]
    dir: tempfile::TempDir,
    port: u16,
    key: PathBuf,
    known: PathBuf,
    user: String,
    child: Child,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Sshd {
    fn start() -> Option<Self> {
        if !Path::new("/usr/sbin/sshd").is_file() {
            eprintln!("SKIP sshd: /usr/sbin/sshd missing");
            return None;
        }
        let dir = tempfile::tempdir().expect("sshd dir");
        let host_key = dir.path().join("host");
        let user_key = dir.path().join("id");
        assert!(
            Command::new("ssh-keygen")
                .args([
                    "-t",
                    "ed25519",
                    "-f",
                    host_key.to_str().unwrap(),
                    "-N",
                    "",
                    "-q"
                ])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("ssh-keygen")
                .args([
                    "-t",
                    "ed25519",
                    "-f",
                    user_key.to_str().unwrap(),
                    "-N",
                    "",
                    "-q"
                ])
                .status()
                .unwrap()
                .success()
        );
        let user =
            String::from_utf8(Command::new("id").args(["-un"]).output().unwrap().stdout).unwrap();
        let user = user.trim().to_string();
        let authorized = dir.path().join("authorized_keys");
        fs::copy(dir.path().join("id.pub"), &authorized).unwrap();
        fs::set_permissions(&authorized, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&user_key, fs::Permissions::from_mode(0o600)).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let config = dir.path().join("sshd_config");
        fs::write(
            &config,
            format!(
                "\
Port {port}
ListenAddress 127.0.0.1
HostKey {host}
PidFile {pid}
AuthorizedKeysFile {auth}
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
PubkeyAuthentication yes
AllowUsers {user}
Subsystem sftp internal-sftp
",
                host = host_key.display(),
                pid = dir.path().join("sshd.pid").display(),
                auth = authorized.display(),
            ),
        )
        .unwrap();
        let log = fs::File::create(dir.path().join("sshd.log")).unwrap();
        let child = Command::new("/usr/sbin/sshd")
            .args(["-f", config.to_str().unwrap(), "-D", "-e"])
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("spawn sshd");
        let mut ready = false;
        for _ in 0..50 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                ready = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if !ready {
            let text = fs::read_to_string(dir.path().join("sshd.log")).unwrap_or_default();
            panic!("sshd did not listen: {text}");
        }
        let pubkey = String::from_utf8(
            Command::new("ssh-keygen")
                .args(["-y", "-f", host_key.to_str().unwrap()])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let known = dir.path().join("known_hosts");
        fs::write(&known, format!("[127.0.0.1]:{port} {}", pubkey.trim())).unwrap();
        Some(Self {
            dir,
            port,
            key: user_key,
            known,
            user,
            child,
        })
    }
}

fn ssh_options(sshd: &Sshd, known: &Path) -> String {
    format!(
        r#"[[hosts]]
name = "fixture"
address = "127.0.0.1"
port = {port}
user = "{user}"
identity_file = "{key}"

[hosts.ssh_options]
UserKnownHostsFile = "{known}"
GlobalKnownHostsFile = "/dev/null"
StrictHostKeyChecking = "yes"
IdentitiesOnly = "yes"
HashKnownHosts = "no"
"#,
        port = sshd.port,
        user = sshd.user,
        key = sshd.key.display(),
        known = known.display(),
    )
}

#[test]
fn qa_cli_push_pull_roundtrip_binary_unicode_empty_and_directory() {
    let Some(server) = sftp_server() else {
        eprintln!("SKIP: sftp-server binary not installed");
        return;
    };
    let iso = Isolated::new();
    install_fake_ssh(&iso.root.join("bin"), &server);
    write_hosts(&iso.config(), host_for_fake());
    let local = iso.root.join("payload.bin");
    let mut bytes = vec![0_u8, 1, 2, 255, 10, b'a'];
    bytes.extend(std::iter::repeat_n(0x5a, 300_000));
    fs::write(&local, &bytes).unwrap();
    let empty = iso.root.join("empty.dat");
    fs::write(&empty, b"").unwrap();
    let spaced = iso.root.join("my file.txt");
    fs::write(&spaced, "hello unicode 文件\n").unwrap();
    let tree = iso.root.join("proj");
    fs::create_dir_all(tree.join("sub")).unwrap();
    fs::write(tree.join("a.txt"), "A").unwrap();
    fs::write(tree.join("sub/b.txt"), "B").unwrap();
    let remote_root = iso.root.join("remote");
    fs::create_dir_all(&remote_root).unwrap();

    let mut push = vcon();
    iso.apply(&mut push, Some(&iso.root.join("bin")));
    let output = push
        .args([
            "-y",
            "push",
            "fixture",
            local.to_str().unwrap(),
            remote_root.join("payload.bin").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(remote_root.join("payload.bin")).unwrap(), bytes);

    let mut push_empty = vcon();
    iso.apply(&mut push_empty, Some(&iso.root.join("bin")));
    assert!(
        push_empty
            .args([
                "-y",
                "push",
                "fixture",
                empty.to_str().unwrap(),
                remote_root.join("empty.dat").to_str().unwrap(),
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read(remote_root.join("empty.dat")).unwrap(), b"");

    let mut push_name = vcon();
    iso.apply(&mut push_name, Some(&iso.root.join("bin")));
    let weird_dir = remote_root.join("names");
    fs::create_dir_all(&weird_dir).unwrap();
    let output = push_name
        .args([
            "-y",
            "push",
            "fixture",
            spaced.to_str().unwrap(),
            &format!("{}/", weird_dir.display()),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "unicode/space push: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(weird_dir.join("my file.txt")).unwrap(),
        b"hello unicode \xe6\x96\x87\xe4\xbb\xb6\n"
    );

    let mut push_tree = vcon();
    iso.apply(&mut push_tree, Some(&iso.root.join("bin")));
    let tree_dest = remote_root.join("tree");
    fs::create_dir_all(&tree_dest).unwrap();
    let output = push_tree
        .args([
            "-y",
            "push",
            "fixture",
            tree.to_str().unwrap(),
            &format!("{}/", tree_dest.display()),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "directory push: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(tree_dest.join("proj/a.txt")).unwrap(), b"A");
    assert_eq!(fs::read(tree_dest.join("proj/sub/b.txt")).unwrap(), b"B");

    fs::write(&local, b"second").unwrap();
    let mut again = vcon();
    iso.apply(&mut again, Some(&iso.root.join("bin")));
    assert!(
        again
            .args([
                "-y",
                "push",
                "fixture",
                local.to_str().unwrap(),
                remote_root.join("payload.bin").to_str().unwrap(),
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        fs::read(remote_root.join("payload.bin")).unwrap(),
        b"second"
    );

    let pulled = iso.root.join("pulled");
    fs::create_dir_all(&pulled).unwrap();
    let mut pull = vcon();
    iso.apply(&mut pull, Some(&iso.root.join("bin")));
    let output = pull
        .args([
            "pull",
            "fixture",
            remote_root.join("payload.bin").to_str().unwrap(),
            pulled.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "pull failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(pulled.join("payload.bin")).unwrap(), b"second");
}

#[test]
fn qa_cli_push_reports_the_filesystem_error() {
    let Some(server) = sftp_server() else {
        eprintln!("SKIP: sftp-server binary not installed");
        return;
    };
    let iso = Isolated::new();
    install_fake_ssh(&iso.root.join("bin"), &server);
    write_hosts(&iso.config(), host_for_fake());
    let local = iso.root.join("payload.bin");
    fs::write(&local, b"data").unwrap();
    let ro = iso.root.join("ro");
    fs::create_dir(&ro).unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    let dest = ro.join("nope.bin");
    let mut push = vcon();
    iso.apply(&mut push, Some(&iso.root.join("bin")));
    let output = push
        .args([
            "-y",
            "push",
            "fixture",
            local.to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).ok();
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.to_ascii_lowercase().contains("permission")
            || stderr.to_ascii_lowercase().contains("denied"),
        "push dropped the I/O error and only reported a generic failure: {stderr}"
    );
}

#[test]
fn qa_cli_pull_reports_the_filesystem_error() {
    let Some(server) = sftp_server() else {
        eprintln!("SKIP: sftp-server binary not installed");
        return;
    };
    let iso = Isolated::new();
    install_fake_ssh(&iso.root.join("bin"), &server);
    write_hosts(&iso.config(), host_for_fake());
    let remote = iso.root.join("remote-file.txt");
    fs::write(&remote, b"data").unwrap();
    let ro = iso.root.join("ro");
    fs::create_dir(&ro).unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    let mut pull = vcon();
    iso.apply(&mut pull, Some(&iso.root.join("bin")));
    let output = pull
        .args([
            "pull",
            "fixture",
            remote.to_str().unwrap(),
            ro.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).ok();
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.to_ascii_lowercase().contains("permission")
            || stderr.to_ascii_lowercase().contains("denied"),
        "pull dropped the I/O error: {stderr}"
    );
}

#[test]
fn qa_cli_push_relative_dotdot_uploads_that_file() {
    let Some(server) = sftp_server() else {
        eprintln!("SKIP: sftp-server binary not installed");
        return;
    };
    let iso = Isolated::new();
    install_fake_ssh(&iso.root.join("bin"), &server);
    write_hosts(&iso.config(), host_for_fake());
    let work = iso.root.join("work");
    fs::create_dir_all(&work).unwrap();
    fs::write(iso.root.join("secret"), b"RIGHT").unwrap();
    fs::write(work.join("secret"), b"WRONG").unwrap();
    let remote = iso.root.join("remote-secret");
    let mut push = vcon();
    iso.apply(&mut push, Some(&iso.root.join("bin")));
    let output = push
        .current_dir(&work)
        .args([
            "-y",
            "push",
            "fixture",
            "../secret",
            remote.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let got = fs::read(&remote).unwrap_or_default();
    assert_eq!(
        got,
        b"RIGHT",
        "vcon push ../secret uploaded {:?} instead of the parent file",
        String::from_utf8_lossy(&got)
    );
}

#[test]
fn qa_cli_wrong_key_cannot_upload_and_right_key_can() {
    let Some(sshd) = Sshd::start() else {
        return;
    };
    let iso = Isolated::new();
    let wrong = iso.root.join("wrong");
    assert!(
        Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-f",
                wrong.to_str().unwrap(),
                "-N",
                "",
                "-q"
            ])
            .status()
            .unwrap()
            .success()
    );
    let local = iso.root.join("payload.txt");
    fs::write(&local, b"secret-bytes").unwrap();
    let remote = iso.root.join("from-client.txt");
    let _ = fs::remove_file(&remote);

    write_hosts(
        &iso.config(),
        &ssh_options(&sshd, &sshd.known)
            .replace(&sshd.key.display().to_string(), wrong.to_str().unwrap()),
    );
    let mut denied = vcon();
    iso.apply(&mut denied, None);
    let output = denied
        .args([
            "-y",
            "push",
            "fixture",
            local.to_str().unwrap(),
            remote.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "wrong key was accepted: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!remote.exists(), "wrong key created {}", remote.display());

    let empty_known = iso.root.join("empty_known");
    fs::write(&empty_known, b"").unwrap();
    write_hosts(&iso.config(), &ssh_options(&sshd, &empty_known));
    let mut unknown = vcon();
    iso.apply(&mut unknown, None);
    let output = unknown
        .args([
            "-y",
            "push",
            "fixture",
            local.to_str().unwrap(),
            remote.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "unknown host key was accepted: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!remote.exists(), "unverified host created a file");

    write_hosts(&iso.config(), &ssh_options(&sshd, &sshd.known));
    let mut ok = vcon();
    iso.apply(&mut ok, None);
    let output = ok
        .args([
            "-y",
            "push",
            "fixture",
            local.to_str().unwrap(),
            remote.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "authenticated push failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&remote).unwrap(), b"secret-bytes");

    let pulled = iso.root.join("down");
    fs::create_dir_all(&pulled).unwrap();
    let mut pull = vcon();
    iso.apply(&mut pull, None);
    let output = pull
        .args([
            "pull",
            "fixture",
            remote.to_str().unwrap(),
            pulled.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "authenticated pull failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(pulled.join("from-client.txt")).unwrap(),
        b"secret-bytes"
    );
}

#[test]
fn qa_cli_pull_unknown_host_fails() {
    let iso = Isolated::new();
    write_hosts(&iso.config(), host_for_fake());
    let mut pull = vcon();
    iso.apply(&mut pull, None);
    let output = pull
        .args(["pull", "no-such-host", "/etc/hostname", "/tmp"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown host"),
        "expected an unknown-host error, got {stderr}"
    );
}
