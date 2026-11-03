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

/// Entry point for the askpass helper ssh executes.
///
/// Prints the password on stdout only for a matching login prompt.
pub fn answer_askpass(prompt: &str) -> ExitCode {
    let account = std::env::var("VCON_ASKPASS_ACCOUNT").unwrap_or_default();
    let user = std::env::var("VCON_ASKPASS_USER").unwrap_or_default();
    let address = std::env::var("VCON_ASKPASS_ADDRESS").unwrap_or_default();
    if account.is_empty() || !prompt_is_login_password(prompt, &user, &address) {
        return ExitCode::from(1);
    }
    match KeyringStore.get(&account) {
        Ok(Some(secret)) if !secret.is_empty() => {
            println!("{secret}");
            ExitCode::SUCCESS
        }
        _ => ExitCode::from(1),
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
}
