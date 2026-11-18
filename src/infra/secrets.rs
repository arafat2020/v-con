//! Login passwords kept in the operating-system keychain.
//!
//! Nothing here is written to `hosts.toml`, the state file, or the audit log.
//! SSH argv is unchanged until a password has actually been saved for that
//! host. The password is never placed on the command line or in the
//! environment of `ssh`; a small askpass helper reads it back from the
//! keychain and only answers a login-password prompt for that exact user
//! and address. Host-key questions and key passphrases are refused.

use crate::domain::host::EffectiveHost;
use crate::error::Error;
#[cfg(test)]
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;

const SERVICE: &str = "vcon";

/// How ssh should consult the askpass helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskpassMode {
    /// A terminal is available. Prefer the keychain for passwords and leave
    /// host-key questions on the tty.
    Prefer,
    /// No terminal. Force the helper, which still refuses anything that is
    /// not this host's login password.
    Force,
}

/// What ssh should do for one host that may have a saved password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedAuth {
    /// When true, omit `BatchMode=yes` and limit password prompts to one.
    pub password_auth: bool,
    /// Environment for the ssh child. Empty when no password is saved.
    pub env: Vec<(String, String)>,
}

impl SavedAuth {
    fn plain() -> Self {
        Self {
            password_auth: false,
            env: Vec::new(),
        }
    }
}

/// Read one password from the user. The secret must not be logged.
pub trait PasswordPrompt: Send + Sync {
    /// Ask for the login password of `user` at `address`.
    fn ask(&self, user: &str, address: &str) -> Result<String, Error>;
}

/// Read and write one host's login password.
pub trait SecretStore: Send + Sync {
    /// `Ok(None)` when this host has no saved password.
    fn get(&self, account: &str) -> Result<Option<String>, Error>;
    /// Store `secret`. An empty secret is refused.
    fn set(&self, account: &str, secret: &str) -> Result<(), Error>;
    /// Remove a saved password. Missing entries are success.
    fn delete(&self, account: &str) -> Result<(), Error>;

    /// Whether a non-empty password is saved. A keychain error is `Err`.
    fn contains(&self, account: &str) -> Result<bool, Error> {
        Ok(self.get(account)?.is_some_and(|secret| !secret.is_empty()))
    }
}

/// Process-wide store. Tests get an empty memory map so they never touch
/// the login keychain. The installed binary uses the OS store.
pub fn system_store() -> Arc<dyn SecretStore> {
    #[cfg(test)]
    {
        Arc::new(MemoryStore::default())
    }
    #[cfg(not(test))]
    {
        Arc::new(KeyringStore)
    }
}

/// In-memory stand-in used by tests. The installed binary uses [`KeyringStore`].
#[cfg(test)]
#[derive(Debug, Default)]
pub struct MemoryStore {
    items: Mutex<BTreeMap<String, String>>,
}

#[cfg(test)]
impl SecretStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>, Error> {
        let items = self
            .items
            .lock()
            .map_err(|_| Error::Message("password store lock poisoned".into()))?;
        Ok(items.get(account).cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), Error> {
        if secret.is_empty() {
            return Err(Error::Message("password is empty".into()));
        }
        let mut items = self
            .items
            .lock()
            .map_err(|_| Error::Message("password store lock poisoned".into()))?;
        items.insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), Error> {
        let mut items = self
            .items
            .lock()
            .map_err(|_| Error::Message("password store lock poisoned".into()))?;
        items.remove(account);
        Ok(())
    }
}

/// macOS Keychain, Windows Credential Manager, or the Secret Service.
#[derive(Debug, Default)]
pub struct KeyringStore;

impl SecretStore for KeyringStore {
    fn get(&self, account: &str) -> Result<Option<String>, Error> {
        let entry = keyring::Entry::new(SERVICE, account).map_err(keyring_err)?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(keyring_err(error)),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), Error> {
        if secret.is_empty() {
            return Err(Error::Message("password is empty".into()));
        }
        let entry = keyring::Entry::new(SERVICE, account).map_err(keyring_err)?;
        entry.set_password(secret).map_err(keyring_err)
    }

    fn delete(&self, account: &str) -> Result<(), Error> {
        let entry = keyring::Entry::new(SERVICE, account).map_err(keyring_err)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(keyring_err(error)),
        }
    }
}

fn keyring_err(error: keyring::Error) -> Error {
    Error::Message(format!("keychain: {error}"))
}

/// Saved password for `host`, plus the environment ssh needs to read it.
///
/// No saved password, or a helper that cannot be installed, leaves ssh exactly
/// as it was: keys, agent, and an interactive prompt.
pub fn saved_auth(
    store: &dyn SecretStore,
    cache_dir: &Path,
    host: &EffectiveHost,
    mode: AskpassMode,
) -> SavedAuth {
    match store.contains(&host.name) {
        Ok(true) => {}
        _ => return SavedAuth::plain(),
    }
    let Ok(program) = askpass_program(cache_dir) else {
        return SavedAuth::plain();
    };
    let require = match mode {
        AskpassMode::Prefer => "prefer",
        AskpassMode::Force => "force",
    };
    SavedAuth {
        password_auth: true,
        env: vec![
            ("SSH_ASKPASS".into(), program.display().to_string()),
            ("SSH_ASKPASS_REQUIRE".into(), require.into()),
            ("VCON_ASKPASS_ACCOUNT".into(), host.name.clone()),
            ("VCON_ASKPASS_USER".into(), host.user.clone()),
            ("VCON_ASKPASS_ADDRESS".into(), host.address.clone()),
        ],
    }
}

/// `true` when ssh is asking for this host's login password.
///
/// Key passphrases, host-key confirmations, and another host's password
/// are refused so the saved secret cannot be sent to the wrong prompt.
pub fn prompt_is_login_password(prompt: &str, user: &str, address: &str) -> bool {
    if user.is_empty() || address.is_empty() {
        return false;
    }
    let lower = prompt.to_ascii_lowercase();
    if lower.contains("passphrase")
        || lower.contains("yes/no")
        || lower.contains("fingerprint")
        || lower.contains("authenticity")
        || lower.contains("continue connecting")
    {
        return false;
    }
    if !lower.contains("password") {
        return false;
    }
    let identity = format!("{user}@{address}");
    prompt.contains(&identity) || prompt.contains(address)
}

/// Pick the secret to print for `prompt`, if any.
///
/// A password supplied on the askpass file descriptor wins over the keychain
/// so a freshly typed password is what ssh tries. The secret is returned only
/// for this host's login prompt.
pub fn secret_for_prompt(
    prompt: &str,
    user: &str,
    address: &str,
    supplied: Option<&str>,
    stored: Option<&str>,
) -> Option<String> {
    if !prompt_is_login_password(prompt, user, address) {
        return None;
    }
    if let Some(secret) = supplied.filter(|secret| !secret.is_empty()) {
        return Some(secret.to_string());
    }
    stored
        .filter(|secret| !secret.is_empty())
        .map(str::to_string)
}

/// Entry point for the askpass helper ssh executes.
///
/// Prints the password on stdout only for a matching login prompt.
/// A `VCON_ASKPASS_FD` pipe supplies a password that is not in the keychain
/// yet. The password is never written to stderr.
pub fn answer_askpass(prompt: &str) -> ExitCode {
    let account = std::env::var("VCON_ASKPASS_ACCOUNT").unwrap_or_default();
    let user = std::env::var("VCON_ASKPASS_USER").unwrap_or_default();
    let address = std::env::var("VCON_ASKPASS_ADDRESS").unwrap_or_default();
    if account.is_empty() {
        return ExitCode::from(1);
    }
    let supplied = std::env::var("VCON_ASKPASS_SOCK")
        .ok()
        .and_then(|name| read_socket_secret(&name).ok())
        .or_else(|| {
            std::env::var("VCON_ASKPASS_FD")
                .ok()
                .and_then(|raw| read_fd_secret(&raw).ok())
        });
    let stored = if supplied.is_some() {
        None
    } else {
        KeyringStore.get(&account).ok().flatten()
    };
    match secret_for_prompt(
        prompt,
        &user,
        &address,
        supplied.as_deref(),
        stored.as_deref(),
    ) {
        Some(secret) => {
            println!("{secret}");
            ExitCode::SUCCESS
        }
        None => ExitCode::from(1),
    }
}

/// Read a password from an inherited pipe. The descriptor is left open.
///
/// The text is not logged. A cap stops a hostile writer from filling memory.
fn read_fd_secret(raw: &str) -> Result<String, ()> {
    let fd: i32 = raw.parse().map_err(|_| ())?;
    if fd < 0 {
        return Err(());
    }
    let mut buf = Vec::new();
    let mut tmp = [0u8; 256];
    loop {
        let n = unsafe { libc::read(fd, tmp.as_mut_ptr().cast(), tmp.len()) };
        if n == 0 {
            break;
        }
        if n < 0 {
            return Err(());
        }
        if buf.len() + n as usize > 4096 {
            return Err(());
        }
        buf.extend_from_slice(&tmp[..n as usize]);
    }
    let secret = String::from_utf8(buf).map_err(|_| ())?;
    let secret = secret.trim_end_matches(['\r', '\n']).to_string();
    if secret.is_empty() {
        Err(())
    } else {
        Ok(secret)
    }
}

/// `true` when `env` asks ssh to read a keychain password (not a fresh one).
pub fn uses_stored_password(env: &[(String, String)]) -> bool {
    let account = env.iter().any(|(key, _)| key == "VCON_ASKPASS_ACCOUNT");
    let fresh = env
        .iter()
        .any(|(key, _)| key == "VCON_ASKPASS_FD" || key == "VCON_ASKPASS_SOCK");
    account && !fresh
}

/// What to tell a non-interactive caller when the saved password was rejected.
pub fn stale_password_message(host: &str) -> String {
    format!(
        "{host}: saved password was rejected. Update it with P on that host in the TUI, or delete the vcon keychain entry for {host} and connect again."
    )
}

/// One stored-password attempt at a time per host.
///
/// The first authentication failure marks the host rejected. Later attempts
/// in this process do not contact the server until the rejection is cleared.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Busy,
    Rejected,
}

fn gate() -> &'static (
    std::sync::Mutex<std::collections::HashMap<String, SlotState>>,
    std::sync::Condvar,
) {
    use std::sync::{Mutex, OnceLock};
    static GATE: OnceLock<(
        Mutex<std::collections::HashMap<String, SlotState>>,
        std::sync::Condvar,
    )> = OnceLock::new();
    GATE.get_or_init(|| {
        (
            Mutex::new(std::collections::HashMap::new()),
            std::sync::Condvar::new(),
        )
    })
}

/// Whether a stored password for `host` already failed in this process.
pub fn is_rejected(host: &str) -> bool {
    let (lock, _) = gate();
    let map = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    map.get(host) == Some(&SlotState::Rejected)
}

/// Allow another stored-password attempt after the user supplies a new one.
pub fn clear_rejection(host: &str) {
    let (lock, cv) = gate();
    let mut map = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    map.remove(host);
    cv.notify_all();
}

/// Held while one stored-password ssh for `host` is in flight.
pub struct GateGuard {
    host: String,
    active: bool,
}

impl GateGuard {
    /// Wait until this host is free. `Err` when the saved password was rejected.
    pub fn enter(host: &str, stored: bool) -> Result<Self, ()> {
        if !stored {
            return Ok(Self {
                host: host.to_string(),
                active: false,
            });
        }
        let (lock, cv) = gate();
        let mut map = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            match map.get(host).copied() {
                Some(SlotState::Rejected) => return Err(()),
                Some(SlotState::Busy) => {
                    map = cv
                        .wait(map)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                None => {
                    map.insert(host.to_string(), SlotState::Busy);
                    return Ok(Self {
                        host: host.to_string(),
                        active: true,
                    });
                }
            }
        }
    }

    /// Release the slot. `auth_failed` blocks further stored-password attempts.
    pub fn finish(&mut self, auth_failed: bool) {
        if !self.active {
            return;
        }
        let (lock, cv) = gate();
        let mut map = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if auth_failed {
            map.insert(self.host.clone(), SlotState::Rejected);
        } else if map.get(&self.host) == Some(&SlotState::Busy) {
            map.remove(&self.host);
        }
        self.active = false;
        cv.notify_all();
    }
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        self.finish(false);
    }
}

/// Abstract-namespace socket that hands a password to the askpass helper.
///
/// OpenSSH closes inherited descriptors before it runs askpass, so a pipe does
/// not survive. The socket name is not the password. Nothing is written to a
/// file. The listener accepts one connection, then goes away.
pub struct SecretChannel {
    name: String,
}

impl SecretChannel {
    /// Bind an abstract socket and serve `secret` to the first connection.
    pub fn create(secret: &str) -> std::io::Result<Self> {
        let name = format!(
            "vcon-{}-{}",
            std::process::id(),
            token_hex().map_err(std::io::Error::other)?
        );
        use std::os::linux::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        let listener = std::os::unix::net::UnixListener::bind_addr(&addr)?;
        let payload = {
            let mut bytes = secret.as_bytes().to_vec();
            bytes.push(b'\n');
            bytes
        };
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            use std::io::Write;
            let _ = stream.write_all(&payload);
        });
        Ok(Self { name })
    }

    /// Abstract name to publish as `VCON_ASKPASS_SOCK`. It does not contain the secret.
    pub fn name(&self) -> &str {
        &self.name
    }
}

fn token_hex() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    let mut file = std::fs::File::open("/dev/urandom")?;
    use std::io::Read;
    file.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn read_socket_secret(name: &str) -> Result<String, ()> {
    if name.is_empty() || name.len() > 200 {
        return Err(());
    }
    use std::os::linux::net::SocketAddrExt;
    let addr =
        std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).map_err(|_| ())?;
    let mut stream = std::os::unix::net::UnixStream::connect_addr(&addr).map_err(|_| ())?;
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    use std::io::Read;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).map_err(|_| ())?;
    let secret = buf.trim_end_matches(['\r', '\n']).to_string();
    if secret.is_empty() {
        Err(())
    } else {
        Ok(secret)
    }
}

/// Pipe that carries a password to the askpass helper.
///
/// The read end is inherited by ssh. Only the descriptor number is placed in
/// the environment. Drop closes the parent's copy after `spawn`.
#[cfg(test)]
pub struct InheritedSecret {
    fd: i32,
}

#[cfg(test)]
impl InheritedSecret {
    /// Write `secret` into a pipe and keep the read end open for the child.
    pub fn create(secret: &str) -> std::io::Result<Self> {
        let mut fds = [0; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let read_fd = fds[0];
        let write_fd = fds[1];
        let mut payload = secret.as_bytes().to_vec();
        payload.push(b'\n');
        let mut offset = 0;
        while offset < payload.len() {
            let n = unsafe {
                libc::write(
                    write_fd,
                    payload[offset..].as_ptr().cast(),
                    payload.len() - offset,
                )
            };
            if n < 0 {
                let error = std::io::Error::last_os_error();
                unsafe {
                    libc::close(read_fd);
                    libc::close(write_fd);
                }
                return Err(error);
            }
            offset += n as usize;
        }
        unsafe { libc::close(write_fd) };
        Ok(Self { fd: read_fd })
    }

    /// Descriptor number to publish as `VCON_ASKPASS_FD`.
    pub fn fd(&self) -> i32 {
        self.fd
    }
}

#[cfg(test)]
impl Drop for InheritedSecret {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
            self.fd = -1;
        }
    }
}

/// Write `cache_dir/askpass`, a mode-0700 script that execs this binary.
///
/// The script contains no secret. ssh can only execute a program path, so
/// the script adds the `askpass` argument.
pub fn askpass_program(cache_dir: &Path) -> Result<PathBuf, Error> {
    let exe = std::env::current_exe().map_err(|error| {
        Error::Message(format!("cannot find the vcon binary for askpass: {error}"))
    })?;
    fs::create_dir_all(cache_dir).map_err(|error| {
        Error::Message(format!("cannot create {}: {error}", cache_dir.display()))
    })?;
    let path = cache_dir.join("askpass");
    let body = format!(
        "#!/bin/sh\nexec {} askpass \"$@\"\n",
        sh_quote(&exe.display().to_string())
    );
    let current = fs::read_to_string(&path).unwrap_or_default();
    if current != body {
        fs::write(&path, body)
            .map_err(|error| Error::Message(format!("cannot write {}: {error}", path.display())))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .map_err(|error| Error::Message(format!("cannot chmod {}: {error}", path.display())))?;
    }
    Ok(path)
}

fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_gate() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn only_the_matching_login_prompt_is_answered() {
        assert!(prompt_is_login_password(
            "deploy@203.0.113.10's password: ",
            "deploy",
            "203.0.113.10"
        ));
        assert!(!prompt_is_login_password(
            "admin@198.51.100.5's password: ",
            "deploy",
            "203.0.113.10"
        ));
        assert!(!prompt_is_login_password(
            "Enter passphrase for key '/Users/me/.ssh/id_ed25519': ",
            "deploy",
            "203.0.113.10"
        ));
        assert!(!prompt_is_login_password(
            "Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
            "deploy",
            "203.0.113.10"
        ));
        assert!(!prompt_is_login_password(
            "Password: ",
            "deploy",
            "203.0.113.10"
        ));
    }

    #[test]
    fn memory_store_round_trips_and_rejects_empty() {
        let store = MemoryStore::default();
        assert!(!store.contains("web-1").unwrap());
        assert!(store.set("web-1", "").is_err());
        store.set("web-1", "s3cret").unwrap();
        assert_eq!(store.get("web-1").unwrap().as_deref(), Some("s3cret"));
        store.delete("web-1").unwrap();
        assert!(!store.contains("web-1").unwrap());
        store.delete("missing").unwrap();
    }

    #[test]
    fn no_saved_password_leaves_ssh_unchanged() {
        let store = MemoryStore::default();
        let host = crate::domain::host::EffectiveHost {
            name: "web-1".into(),
            address: "203.0.113.10".into(),
            user: "deploy".into(),
            port: 22,
            identity_file: None,
            jump: None,
            tags: Vec::new(),
            notes: None,
            ssh_options: BTreeMap::new(),
            disabled: false,
            connect_timeout: None,
            tunnels: Vec::new(),
        };
        let auth = saved_auth(
            &store,
            Path::new("/tmp/vcon-askpass-test"),
            &host,
            AskpassMode::Force,
        );
        assert!(!auth.password_auth);
        assert!(auth.env.is_empty());
    }

    #[test]
    fn saved_password_sets_askpass_without_the_secret_in_the_environment() {
        let dir = std::env::temp_dir().join(format!("vcon-askpass-auth-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = MemoryStore::default();
        store.set("web-1", "s3cret").unwrap();
        let host = crate::domain::host::EffectiveHost {
            name: "web-1".into(),
            address: "203.0.113.10".into(),
            user: "deploy".into(),
            port: 22,
            identity_file: None,
            jump: None,
            tags: Vec::new(),
            notes: None,
            ssh_options: BTreeMap::new(),
            disabled: false,
            connect_timeout: None,
            tunnels: Vec::new(),
        };
        let auth = saved_auth(&store, &dir, &host, AskpassMode::Prefer);
        assert!(auth.password_auth);
        assert!(
            auth.env
                .iter()
                .any(|(key, value)| { key == "SSH_ASKPASS_REQUIRE" && value == "prefer" })
        );
        assert!(
            auth.env
                .iter()
                .any(|(key, value)| { key == "VCON_ASKPASS_ACCOUNT" && value == "web-1" })
        );
        assert!(auth.env.iter().all(|(_, value)| !value.contains("s3cret")));
        let script = fs::read_to_string(dir.join("askpass")).unwrap();
        assert!(!script.contains("s3cret"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn askpass_script_does_not_contain_the_password() {
        let dir = std::env::temp_dir().join(format!("vcon-askpass-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = askpass_program(&dir).unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("askpass"));
        assert!(!body.contains("s3cret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn supplied_password_is_used_only_for_the_login_prompt() {
        assert_eq!(
            secret_for_prompt(
                "deploy@203.0.113.10's password: ",
                "deploy",
                "203.0.113.10",
                Some("fresh"),
                Some("stored")
            )
            .as_deref(),
            Some("fresh")
        );
        assert_eq!(
            secret_for_prompt(
                "Enter passphrase for key '/tmp/id': ",
                "deploy",
                "203.0.113.10",
                Some("fresh"),
                Some("stored")
            ),
            None
        );
        assert_eq!(
            secret_for_prompt(
                "deploy@203.0.113.10's password: ",
                "deploy",
                "203.0.113.10",
                None,
                Some("stored")
            )
            .as_deref(),
            Some("stored")
        );
    }

    #[test]
    fn stored_password_failure_blocks_later_attempts() {
        let host = format!("gate-{}-{}", std::process::id(), unique_gate());
        clear_rejection(&host);
        let mut first = GateGuard::enter(&host, true).unwrap();
        let blocked = std::thread::spawn({
            let host = host.clone();
            move || GateGuard::enter(&host, true).is_err()
        });
        first.finish(true);
        assert!(blocked.join().unwrap(), "waiter should see the rejection");
        assert!(is_rejected(&host));
        assert!(GateGuard::enter(&host, true).is_err());
        clear_rejection(&host);
        let mut again = GateGuard::enter(&host, true).unwrap();
        again.finish(false);
        assert!(!is_rejected(&host));
    }

    #[test]
    fn secret_channel_name_is_not_the_password_and_the_secret_round_trips() {
        let channel = SecretChannel::create("s3cret-value").unwrap();
        assert!(!channel.name().contains("s3cret"));
        let got = read_socket_secret(channel.name()).unwrap();
        assert_eq!(got, "s3cret-value");
    }

    #[test]
    fn inherited_secret_round_trips_without_putting_it_in_the_env_value() {
        let pipe = InheritedSecret::create("s3cret-value").unwrap();
        let fd = pipe.fd().to_string();
        assert!(!fd.contains("s3cret"));
        let got = read_fd_secret(&fd).unwrap();
        assert_eq!(got, "s3cret-value");
        // The read consumed the pipe. Drop must not panic.
        drop(pipe);
    }
}
