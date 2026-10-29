//! `vcon init`: create the config directory and a commented `hosts.toml`.
//!
//! The command refuses to overwrite an existing inventory. The template is a
//! valid file, with one example host and the other fields shown as comments.

use crate::error::Error;
use crate::infra::fsutil;
use std::path::Path;

const TEMPLATE: &str = r#"# vcon inventory.
# Values under [defaults] apply to every host unless that host overrides them.
# Names are lowercase letters, digits, '-', '_' and '.'.

[defaults]
user = "root"
port = 22
identity_file = "~/.ssh/id_ed25519"
# connect_timeout = 5
# jump = "bastion"

# [[hosts]]
# name = "bastion"
# address = "198.51.100.5"
# user = "admin"
# tags = ["infra"]

[[hosts]]
name = "example"
address = "203.0.113.10"
# user = "deploy"
# port = 2222
# jump = "bastion"            # another host's name, or a raw user@host:port
# identity_file = "~/.ssh/id_ed25519"
tags = ["web", "prod"]
notes = "Replace this example with your servers."
# ssh_options = { ServerAliveInterval = "30" }
"#;

/// Create `config_dir/hosts.toml` when it does not already exist.
///
/// Returns the success message. An existing file is [`Error::Message`].
/// The directory is `0700` and `hosts.toml` is `0600` on unix.
pub fn run(config_dir: &Path) -> Result<String, Error> {
    fsutil::create_dir_secure(config_dir).map_err(|source| Error::Write {
        path: config_dir.to_path_buf(),
        source,
    })?;
    let path = config_dir.join("hosts.toml");
    if path.exists() {
        return Err(Error::Message(format!(
            "hosts.toml already exists at {}",
            path.display()
        )));
    }
    fsutil::write_secure(&path, TEMPLATE).map_err(|source| Error::Write {
        path: path.clone(),
        source,
    })?;
    Ok(format!("Created {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_a_commented_inventory_and_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let message = run(dir.path()).unwrap();
        let path = dir.path().join("hosts.toml");
        assert!(message.contains(&path.display().to_string()));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# vcon inventory"));
        assert!(text.contains("[[hosts]]"));
        let error = run(dir.path()).unwrap_err();
        assert!(error.to_string().contains("already exists"));
        assert!(std::fs::read_to_string(&path).unwrap().contains("example"));
    }
}
