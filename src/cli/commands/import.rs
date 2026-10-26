//! `vcon import ssh-config`: add hosts from an OpenSSH config file.
//!
//! `--dry-run` prints the plan and does not write. Existing names are skipped
//! unless `--overwrite` is set. Imported hosts are tagged `imported`.

use crate::error::Error;
use crate::infra::inventory_store;
use crate::infra::ssh_config::{self, ConfigEntry};
use crate::services::inventory;
use std::path::Path;

/// Text to print, and whether the import wrote the inventory.
pub struct ImportOutput {
    /// One row per host, plus a summary line.
    pub text: String,
}

/// Import `config_path` into `inventory_path`.
pub fn run(
    inventory_path: &Path,
    config_path: &Path,
    dry_run: bool,
    overwrite: bool,
    local_user: &str,
) -> Result<ImportOutput, Error> {
    if !config_path.exists() {
        return Err(Error::Message(format!(
            "no ssh config at {}",
            config_path.display()
        )));
    }
    let loaded = inventory_store::load(inventory_path)?;
    let parsed = ssh_config::load(config_path);
    let mut lines = Vec::new();
    let mut selected = Vec::new();
    for entry in parsed.entries {
        match classify(&entry, &loaded) {
            Row::Skipped { pattern, reason } => lines.push(format!("skipped  {pattern}  {reason}")),
            Row::Exists { name, address, .. } if !overwrite => {
                lines.push(format!("exists   {name}  {address}"));
            }
            Row::Exists { host, .. } => {
                lines.push(format!("replace  {}  {}", host.name, host.address));
                selected.push(host);
            }
            Row::New { host } => {
                lines.push(format!("import   {}  {}", host.name, host.address));
                selected.push(host);
            }
        }
    }
    for warning in &parsed.warnings {
        lines.push(format!("warning  {warning}"));
    }
    if dry_run {
        lines.push(format!("would import {} hosts", selected.len()));
        return Ok(ImportOutput {
            text: lines.join("\n"),
        });
    }
    if !selected.is_empty() {
        inventory::import_hosts(inventory_path, local_user, &selected, true)?;
    }
    lines.push(format!("imported {} hosts", selected.len()));
    Ok(ImportOutput {
        text: lines.join("\n"),
    })
}

enum Row {
    Skipped {
        pattern: String,
        reason: String,
    },
    Exists {
        name: String,
        address: String,
        host: crate::domain::host::Host,
    },
    New {
        host: crate::domain::host::Host,
    },
}

fn classify(entry: &ConfigEntry, loaded: &inventory_store::LoadedInventory) -> Row {
    if let Some(reason) = &entry.skip_reason {
        return Row::Skipped {
            pattern: entry.pattern.clone(),
            reason: reason.clone(),
        };
    }
    let host = entry.host.clone().expect("concrete entries include a host");
    if loaded
        .inventory
        .hosts
        .iter()
        .any(|existing| existing.name == host.name)
    {
        Row::Exists {
            name: host.name.clone(),
            address: host.address.clone(),
            host,
        }
    } else {
        Row::New { host }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_does_not_write_and_overwrite_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let inventory = dir.path().join("hosts.toml");
        std::fs::write(
            &inventory,
            "[[hosts]]\nname = \"web-1\"\naddress = \"203.0.113.10\"\n",
        )
        .unwrap();
        let config = dir.path().join("config");
        std::fs::write(
            &config,
            "Host web-1\n  HostName 203.0.113.99\n  User deploy\nHost db-1\n  HostName 10.0.0.20\nHost *\n  User root\n",
        )
        .unwrap();
        let dry = run(&inventory, &config, true, false, "ada").unwrap();
        assert!(dry.text.contains("exists   web-1"));
        assert!(dry.text.contains("import   db-1"));
        assert!(dry.text.contains("skipped  *"));
        assert!(dry.text.contains("would import 1"));
        assert!(
            !std::fs::read_to_string(&inventory)
                .unwrap()
                .contains("db-1")
        );

        let wrote = run(&inventory, &config, false, true, "ada").unwrap();
        assert!(wrote.text.contains("imported 2"));
        let text = std::fs::read_to_string(&inventory).unwrap();
        assert!(text.contains("203.0.113.99"), "{text}");
        assert!(text.contains("db-1"), "{text}");
        assert!(text.contains("imported"), "{text}");
    }
}
