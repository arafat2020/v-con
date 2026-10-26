//! Host inventory types, resolution, and validation.
//!
//! This module does not read or write files. [`resolve`] applies `[defaults]`
//! and the built-in fallbacks. [`validate_inventory`] returns every problem in
//! one pass: names, addresses, ports, unknown jump hosts, and jump cycles.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

/// Values applied to every host that does not set the same field.
///
/// Unknown keys are rejected so a typo in `hosts.toml` fails at load time.
/// Optional keys may be omitted; they deserialize as `None` or an empty map.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Login name used when a host leaves `user` unset.
    #[serde(default)]
    pub user: Option<String>,
    /// TCP port used when a host leaves `port` unset. `0` is invalid.
    #[serde(default)]
    pub port: Option<u16>,
    /// Private key passed to `ssh -i` when a host leaves `identity_file` unset.
    /// `~` is expanded later, when the command line is built.
    #[serde(default)]
    pub identity_file: Option<String>,
    /// Jump used when a host leaves `jump` unset.
    ///
    /// A plain name must be another host in the inventory. A value containing
    /// `@` or `:` is a raw OpenSSH destination and is not looked up.
    #[serde(default)]
    pub jump: Option<String>,
    /// Extra `-o Key=Value` options. A host map overrides keys with the same name.
    #[serde(default)]
    pub ssh_options: BTreeMap<String, String>,
    /// Seconds passed as `ConnectTimeout` when the caller does not override it.
    #[serde(default)]
    pub connect_timeout: Option<u64>,
}

/// A saved port forward stored on a host.
///
/// The spec string is not parsed here. Phase 8 owns tunnel syntax. The field
/// exists so an inventory that already lists tunnels still loads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostTunnel {
    /// Short name shown later in the tunnels screen.
    pub name: String,
    /// Compact forward, for example `L:3000:localhost:3000`.
    pub spec: String,
}

/// One record from `[[hosts]]` before defaults are applied.
///
/// `name` and `address` are required. Every other field may be omitted.
/// `disabled` hosts stay in the browser and can still be selected by exact name,
/// but `all`, tag selectors, and globs skip them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    /// Unique inventory name. Lowercase letters, digits, `-`, `_`, and `.`.
    pub name: String,
    /// IP address or DNS name. Must be non-empty.
    pub address: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub identity_file: Option<String>,
    #[serde(default)]
    pub jump: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub ssh_options: BTreeMap<String, String>,
    /// Saved port forwards. Parsing the spec is phase 8; the field is accepted so inventories stay valid.
    #[serde(default)]
    pub tunnels: Vec<HostTunnel>,
    #[serde(default)]
    pub disabled: bool,
}

/// A host after `[defaults]` and the built-in fallbacks have been applied.
///
/// The CLI, the TUI, and the SSH command builder take this type and do not
/// look at [`Host`] or [`Defaults`] again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveHost {
    pub name: String,
    pub address: String,
    pub user: String,
    pub port: u16,
    pub identity_file: Option<String>,
    pub jump: Option<String>,
    pub tags: Vec<String>,
    pub notes: Option<String>,
    pub ssh_options: BTreeMap<String, String>,
    pub disabled: bool,
    pub connect_timeout: Option<u64>,
    pub tunnels: Vec<HostTunnel>,
}

impl EffectiveHost {
    /// Returns whether `tag` is one of this host's tags.
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|candidate| candidate == tag)
    }

    /// `user@address`, without a port. The port is a separate `-p` argument.
    pub fn destination(&self) -> String {
        format!("{}@{}", self.user, self.address)
    }
}

/// The parsed `hosts.toml` document: defaults plus the host list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub hosts: Vec<Host>,
}

/// Where a validation problem was found, so the loader can attach a line number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemSite {
    /// Index into [`Inventory::hosts`].
    Host(usize),
    /// The `[defaults]` table.
    Defaults,
}

/// One validation problem. `line` is filled in by the inventory loader when it
/// can see the TOML source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Which table the problem belongs to.
    pub site: ProblemSite,
    /// Human-readable description, including the host name when there is one.
    pub message: String,
    /// 1-based line in `hosts.toml`, when known.
    pub line: Option<usize>,
}

impl fmt::Display for Problem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(formatter, "line {line}: {}", self.message),
            None => write!(formatter, "{}", self.message),
        }
    }
}

/// Resolve one host.
///
/// For each field the host value wins, then `[defaults]`, then a built-in:
/// `local_user` for the login name, port 22, and "do not pass it" for the
/// identity file and jump host. Host `ssh_options` override defaults key by key.
/// `connect_timeout` exists only on defaults.
pub fn resolve(host: &Host, defaults: &Defaults, local_user: &str) -> EffectiveHost {
    let mut ssh_options = defaults.ssh_options.clone();
    ssh_options.extend(host.ssh_options.clone());

    EffectiveHost {
        name: host.name.clone(),
        address: host.address.clone(),
        user: host
            .user
            .clone()
            .or_else(|| defaults.user.clone())
            .unwrap_or_else(|| local_user.to_string()),
        port: host.port.or(defaults.port).unwrap_or(22),
        identity_file: host
            .identity_file
            .clone()
            .or_else(|| defaults.identity_file.clone()),
        jump: host.jump.clone().or_else(|| defaults.jump.clone()),
        tags: host.tags.clone(),
        notes: host.notes.clone(),
        ssh_options,
        disabled: host.disabled,
        connect_timeout: defaults.connect_timeout,
        tunnels: host.tunnels.clone(),
    }
}

/// Check the whole inventory and return every problem, not just the first.
///
/// Names must be unique and use only `a-z`, digits, `-`, `_`, and `.`.
/// Addresses must be non-empty and ports must be in `1..=65535` (`0` is the
/// only out-of-range `u16`). A jump that is a plain host name must exist.
/// Cycles are detected on the effective jump, so a cycle that goes through
/// `[defaults].jump` is reported for every host on it.
pub fn validate_inventory(inventory: &Inventory) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut seen = HashMap::<&str, usize>::new();

    if let Some(port) = inventory.defaults.port
        && port == 0
    {
        problems.push(problem(
            ProblemSite::Defaults,
            format!("defaults port {port} is outside 1-65535"),
        ));
    }
    if let Some(jump) = inventory.defaults.jump.as_deref() {
        if jump.is_empty() {
            problems.push(problem(ProblemSite::Defaults, "defaults jump is empty"));
        } else if is_plain_jump(jump) && !inventory.hosts.iter().any(|host| host.name == jump) {
            problems.push(problem(
                ProblemSite::Defaults,
                format!("defaults jump '{jump}' does not exist"),
            ));
        }
    }

    for (index, host) in inventory.hosts.iter().enumerate() {
        let site = ProblemSite::Host(index);
        if host.name.is_empty() {
            problems.push(problem(site, "host name is empty"));
        } else if !valid_name(&host.name) {
            problems.push(problem(
                site,
                format!(
                    "host name '{}' contains invalid characters; allowed: a-z, 0-9, '-', '_', '.'",
                    host.name
                ),
            ));
        }

        if !host.name.is_empty() {
            let count = seen.entry(host.name.as_str()).or_insert(0);
            *count += 1;
            if *count > 1 {
                problems.push(problem(
                    site,
                    format!("host name '{}' is duplicated", host.name),
                ));
            }
        }

        if host.address.is_empty() {
            let label = display_name(host);
            problems.push(problem(site, format!("host '{label}' address is empty")));
        }

        if let Some(port) = host.port
            && port == 0
        {
            let label = display_name(host);
            problems.push(problem(
                site,
                format!("host '{label}' port {port} is outside 1-65535"),
            ));
        }

        if let Some(jump) = host.jump.as_deref() {
            let label = display_name(host);
            if jump.is_empty() {
                problems.push(problem(site, format!("host '{label}' jump is empty")));
            } else if is_plain_jump(jump) && !inventory.hosts.iter().any(|other| other.name == jump)
            {
                problems.push(problem(
                    site,
                    format!("host '{label}' jump '{jump}' does not exist"),
                ));
            }
        }
    }

    let edges = jump_edges(inventory);
    for (index, host) in inventory.hosts.iter().enumerate() {
        if host.name.is_empty() {
            continue;
        }
        if chain_cycles(&host.name, &edges) {
            problems.push(problem(
                ProblemSite::Host(index),
                format!("host '{}' is part of a jump cycle", host.name),
            ));
        }
    }

    problems
}

/// A jump that names another inventory host, rather than a raw `user@host[:port]`.
pub fn is_plain_jump(jump: &str) -> bool {
    !jump.is_empty() && !jump.contains('@') && !jump.contains(':')
}

fn valid_name(name: &str) -> bool {
    name.chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_' | '.'))
}

fn display_name(host: &Host) -> &str {
    if host.name.is_empty() {
        "(unnamed)"
    } else {
        host.name.as_str()
    }
}

fn problem(site: ProblemSite, message: impl Into<String>) -> Problem {
    Problem {
        site,
        message: message.into(),
        line: None,
    }
}

/// Directed edges `host -> jump` for plain names that exist in the inventory.
/// A host with no `jump` of its own inherits `[defaults].jump`.
fn jump_edges(inventory: &Inventory) -> HashMap<&str, &str> {
    let names: HashSet<&str> = inventory
        .hosts
        .iter()
        .filter(|host| !host.name.is_empty())
        .map(|host| host.name.as_str())
        .collect();
    let mut edges = HashMap::new();
    for host in &inventory.hosts {
        if host.name.is_empty() || !names.contains(host.name.as_str()) {
            continue;
        }
        let jump = host.jump.as_deref().or(inventory.defaults.jump.as_deref());
        if let Some(jump) = jump
            && is_plain_jump(jump)
            && names.contains(jump)
        {
            edges.insert(host.name.as_str(), jump);
        }
    }
    edges
}

/// Walk `start` through `edges`. A repeated name means this host sits on a cycle,
/// including a host that jumps to itself.
fn chain_cycles<'a>(start: &'a str, edges: &HashMap<&'a str, &'a str>) -> bool {
    let mut seen = HashSet::new();
    let mut current = start;
    loop {
        if !seen.insert(current) {
            return true;
        }
        match edges.get(current) {
            Some(next) => current = next,
            None => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Inventory {
        toml_edit::de::from_str(text).expect("inventory toml")
    }

    #[test]
    fn resolution_prefers_host_then_defaults_then_builtin() {
        let inventory = parse(
            r#"
            [defaults]
            user = "root"
            port = 22
            identity_file = "~/.ssh/id_ed25519"
            connect_timeout = 5
            jump = "bastion"

            [defaults.ssh_options]
            ServerAliveInterval = "30"
            StrictHostKeyChecking = "accept-new"

            [[hosts]]
            name = "web-1"
            address = "203.0.113.10"
            user = "deploy"
            port = 2222
            notes = "Main nginx box."
            tags = ["web", "prod"]

            [hosts.ssh_options]
            ServerAliveInterval = "15"
            "#,
        );
        let effective = resolve(&inventory.hosts[0], &inventory.defaults, "ada");
        assert_eq!(effective.user, "deploy");
        assert_eq!(effective.port, 2222);
        assert_eq!(
            effective.identity_file.as_deref(),
            Some("~/.ssh/id_ed25519")
        );
        assert_eq!(effective.jump.as_deref(), Some("bastion"));
        assert_eq!(effective.connect_timeout, Some(5));
        assert_eq!(
            effective
                .ssh_options
                .get("ServerAliveInterval")
                .map(String::as_str),
            Some("15")
        );
        assert_eq!(
            effective
                .ssh_options
                .get("StrictHostKeyChecking")
                .map(String::as_str),
            Some("accept-new")
        );
        assert_eq!(effective.notes.as_deref(), Some("Main nginx box."));

        let bare = parse(
            r#"
            [[hosts]]
            name = "solo"
            address = "10.0.0.2"
            "#,
        );
        let effective = resolve(&bare.hosts[0], &bare.defaults, "ada");
        assert_eq!(effective.user, "ada");
        assert_eq!(effective.port, 22);
        assert!(effective.identity_file.is_none());
        assert!(effective.jump.is_none());
    }

    #[test]
    fn optional_fields_default_and_unknown_fields_fail() {
        let inventory = parse(
            r#"
            [[hosts]]
            name = "web-1"
            address = "203.0.113.10"
            "#,
        );
        assert!(inventory.hosts[0].user.is_none());
        assert!(inventory.hosts[0].tags.is_empty());
        assert!(!inventory.hosts[0].disabled);

        let unknown = toml_edit::de::from_str::<Inventory>(
            r#"
            [[hosts]]
            name = "web-1"
            adress = "203.0.113.10"
            "#,
        );
        assert!(unknown.is_err());
    }

    #[test]
    fn documented_example_parses() {
        let inventory = parse(
            r#"
            [defaults]
            user = "root"
            port = 22
            identity_file = "~/.ssh/id_ed25519"
            connect_timeout = 5

            [[hosts]]
            name = "bastion"
            address = "198.51.100.5"
            user = "admin"
            tags = ["infra"]

            [[hosts]]
            name = "web-1"
            address = "203.0.113.10"
            user = "deploy"
            tags = ["web", "prod", "hetzner"]
            jump = "bastion"
            notes = "Main nginx box. Certs renew via certbot timer."

            [[hosts.tunnels]]
            name = "grafana"
            spec = "L:3000:localhost:3000"

            [[hosts]]
            name = "db-1"
            address = "10.0.0.20"
            port = 2222
            tags = ["db", "prod"]
            jump = "bastion"
            identity_file = "~/.ssh/db_key"

            [[hosts]]
            name = "staging-1"
            address = "staging.example.com"
            tags = ["web", "staging"]
            "#,
        );
        assert!(validate_inventory(&inventory).is_empty());
        assert_eq!(inventory.hosts[1].tunnels[0].name, "grafana");
        let staging = resolve(inventory.hosts.last().unwrap(), &inventory.defaults, "ada");
        assert_eq!(staging.user, "root");
        assert_eq!(staging.port, 22);
    }

    #[test]
    fn validation_reports_every_problem() {
        let inventory = parse(
            r#"
            [defaults]
            port = 0
            jump = "missing-default"

            [[hosts]]
            name = "Web-1"
            address = ""
            port = 0
            jump = "ghost"

            [[hosts]]
            name = "ok"
            address = "10.0.0.1"

            [[hosts]]
            name = "ok"
            address = "10.0.0.2"
            jump = ""

            [[hosts]]
            name = ""
            address = "10.0.0.3"
            "#,
        );
        let messages: Vec<_> = validate_inventory(&inventory)
            .into_iter()
            .map(|problem| problem.message)
            .collect();
        assert!(
            messages
                .iter()
                .any(|message| message.contains("defaults port"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("defaults jump"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("invalid characters"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("address is empty"))
        );
        assert!(messages.iter().any(|message| message.contains("port 0")));
        assert!(
            messages
                .iter()
                .any(|message| message.contains("does not exist"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("duplicated"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("jump is empty"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("host name is empty"))
        );
    }

    #[test]
    fn unknown_plain_jump_is_rejected_and_raw_jump_is_allowed() {
        let unknown = parse(
            r#"
            [[hosts]]
            name = "web-1"
            address = "10.0.0.1"
            jump = "bastion"
            "#,
        );
        let messages: Vec<_> = validate_inventory(&unknown)
            .into_iter()
            .map(|problem| problem.message)
            .collect();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("does not exist"));

        let raw = parse(
            r#"
            [[hosts]]
            name = "web-1"
            address = "10.0.0.1"
            jump = "deploy@198.51.100.5:22"
            "#,
        );
        assert!(validate_inventory(&raw).is_empty());
    }

    #[test]
    fn jump_cycles_include_defaults() {
        let inventory = parse(
            r#"
            [defaults]
            jump = "a"

            [[hosts]]
            name = "a"
            address = "10.0.0.1"
            jump = "b"

            [[hosts]]
            name = "b"
            address = "10.0.0.2"

            [[hosts]]
            name = "self"
            address = "10.0.0.3"
            jump = "self"
            "#,
        );
        let messages: Vec<_> = validate_inventory(&inventory)
            .into_iter()
            .map(|problem| problem.message)
            .collect();
        assert!(messages.iter().any(|message| message.contains("host 'a'")));
        assert!(messages.iter().any(|message| message.contains("host 'b'")));
        assert!(
            messages
                .iter()
                .any(|message| message.contains("host 'self'"))
        );
    }
}
