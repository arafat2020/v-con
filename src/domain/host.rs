//! Host, defaults, and resolution into an effective host.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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
