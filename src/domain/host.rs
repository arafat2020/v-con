//! Host, defaults, and resolution into an effective host.

use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fmt::format,
};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub jump: Option<String>,
    #[serde(default)]
    pub ssh_options: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub name: String,
    pub address: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub jump: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub note: Option<String>,
    #[serde(default)]
    pub ssh_options: HashMap<String, String>,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveHost {
    pub name: String,
    pub address: String,
    pub user: String,
    pub port: u16,
    pub identity_file: Option<String>,
    pub jump: Option<String>,
    pub tags: HashSet<String>,
    pub note: Option<String>,
    pub ssh_options: HashMap<String, String>,
    pub disabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Inventory {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub hosts: Vec<Host>,
}

pub fn resolve(host: &Host, defaults: &Defaults, local_user: &str) -> EffectiveHost {
    let user = host
        .user
        .clone()
        .or_else(|| defaults.user.clone())
        .unwrap_or_else(|| local_user.to_string());

    let port = host.port.or(defaults.port).unwrap_or(22);

    let identity_file = host
        .identity_file
        .clone()
        .or_else(|| defaults.identity_file.clone());

    let jump: Option<String> = host.jump.clone().or_else(|| defaults.jump.clone());

    let mut ssh_options: HashMap<String, String> = defaults.ssh_options.clone();
    ssh_options.extend(host.ssh_options.clone());

    EffectiveHost {
        name: host.name.clone(),
        address: host.address.clone(),
        user,
        port,
        identity_file,
        jump,
        tags: host.tags.iter().cloned().collect(),
        note: host.note.clone(),
        ssh_options,
        disabled: host.disabled,
    }
}

pub fn validate_inventory(inventory: &Inventory) -> Vec<String> {
    let mut errors: Vec<_> = Vec::new();
    let mut names: HashSet<_> = HashSet::new();
    let host_names: HashSet<&str> = inventory.hosts.iter().map(|h| h.name.as_str()).collect();

    for host in &inventory.hosts {
        if host.name.is_empty() {
            errors.push("Host name cannot be empty".to_string());
        } else if !names.insert(&host.name) {
            errors.push(format!("Host name '{}' is not unique", host.name));
        }

        if (!host
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
        {
            errors.push(format!(
                "Host name '{}' contains invalid characters. Only alphanumeric characters, hyphens, and underscores are allowed.",
                host.name
            ));
        }

        if host.address.is_empty() {
            errors.push(format!("Host '{}' address cannot be empty", host.name));
        }

        if let Some(port) = host.port {
            if port == 0 {
                errors.push(format!(
                    "Host '{}' has an invalid port number: {}. Port must be between 1 and 65535.",
                    host.name, port
                ));
            }
        }

        if let Some(ref jump) = host.jump {
            if !jump.contains('@') && !jump.contains(":") && host_names.contains(jump.as_str()) {
                errors.push(format!(
                    "Host '{}' has an invalid jump host '{}'. Jump host must be a valid hostname or user@hostname.",
                    host.name, jump
                ));
            } else if detect_jump_cycle(host, &inventory.hosts) {
                errors.push(format!(
                    "Host '{}' is part of a jump host cycle", host.name
                ));
            }
        }
    }

    errors
}

fn detect_jump_cycle(start_host: &Host, hosts: &Vec<Host>) -> bool {
    let mut visited: HashSet<_> = HashSet::new();
    let mut current: Option<String> = start_host.jump.clone();

    while let Some(jump_target) = current {
        if !visited.insert(jump_target.clone()) {
            return true; // Cycle detected
        }

        current = hosts
            .iter()
            .find(|h| h.name == jump_target)
            .and_then(|h| h.jump.clone());
    }
    false
}
