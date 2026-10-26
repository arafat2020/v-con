//! `vcon ls`: print effective hosts as a table or as JSON.
//!
//! With no selector, disabled hosts are included. A selector uses the shared
//! grammar, so `vcon ls all` skips disabled hosts and `vcon ls db-3` still
//! finds a disabled `db-3`. Validation problems are warnings on stderr.
//! An empty selector result is a failure.

use crate::domain::host::EffectiveHost;
use crate::domain::selector::{Selection, evaluate_selector, parse_selector};
use crate::error::Error;
use crate::services::inventory::InventoryBundle;
use serde::Serialize;
use std::process::ExitCode;

#[derive(Serialize)]
struct HostJson<'a> {
    name: &'a str,
    address: &'a str,
    user: &'a str,
    port: u16,
    identity_file: Option<&'a str>,
    jump: Option<&'a str>,
    tags: &'a [String],
    notes: Option<&'a str>,
    disabled: bool,
    ssh_options: &'a std::collections::BTreeMap<String, String>,
}

/// Text for stdout and stderr, and the exit code the process should use.
pub struct ListResult {
    /// Table or JSON. Empty when the selector matched nothing.
    pub stdout: String,
    /// Validation warnings and selector errors.
    pub stderr: String,
    /// `SUCCESS`, or exit code 1 when the selector failed.
    pub code: ExitCode,
}

/// List `bundle.hosts`, optionally filtered by `selector`.
///
/// `json` selects a pretty-printed array of effective host fields.
pub fn run(
    bundle: &InventoryBundle,
    selector: Option<&str>,
    json: bool,
) -> Result<ListResult, Error> {
    let mut stderr_notes = Vec::new();
    for problem in &bundle.problems {
        stderr_notes.push(format!("warning: {problem}"));
    }

    let hosts: Vec<&EffectiveHost> = if let Some(selector) = selector {
        let terms = parse_selector(selector).map_err(Error::Message)?;
        let selection = evaluate_selector(&terms, &bundle.hosts);
        if let Some(error) = &selection.error {
            stderr_notes.push(error.clone());
            for warning in &selection.warnings {
                if warning != error {
                    stderr_notes.push(format!("warning: {warning}"));
                }
            }
            return Ok(ListResult {
                stdout: String::new(),
                stderr: stderr_notes.join("\n"),
                code: ExitCode::from(1),
            });
        }
        push_warnings(&mut stderr_notes, &selection);
        selection.hosts
    } else {
        bundle.hosts.iter().collect()
    };

    let body = if json {
        let rows: Vec<HostJson> = hosts
            .iter()
            .map(|host| HostJson {
                name: &host.name,
                address: &host.address,
                user: &host.user,
                port: host.port,
                identity_file: host.identity_file.as_deref(),
                jump: host.jump.as_deref(),
                tags: &host.tags,
                notes: host.notes.as_deref(),
                disabled: host.disabled,
                ssh_options: &host.ssh_options,
            })
            .collect();
        serde_json::to_string_pretty(&rows).map_err(|error| Error::Message(error.to_string()))?
    } else {
        render_table(&hosts)
    };

    Ok(ListResult {
        stdout: body,
        stderr: stderr_notes.join("\n"),
        code: ExitCode::SUCCESS,
    })
}

fn push_warnings(notes: &mut Vec<String>, selection: &Selection<'_>) {
    for warning in &selection.warnings {
        notes.push(format!("warning: {warning}"));
    }
}

fn render_table(hosts: &[&EffectiveHost]) -> String {
    if hosts.is_empty() {
        return "No hosts.".to_string();
    }
    let mut lines = vec![format!(
        "{:<16} {:<18} {:<10} {:<6} {}",
        "NAME", "ADDRESS", "USER", "PORT", "TAGS"
    )];
    for host in hosts {
        lines.push(format!(
            "{:<16} {:<18} {:<10} {:<6} {}",
            host.name,
            host.address,
            host.user,
            host.port,
            host.tags.join(",")
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::host::{Defaults, Host, resolve};
    use crate::services::inventory::InventoryBundle;
    use toml_edit::DocumentMut;

    fn bundle() -> InventoryBundle {
        let defaults = Defaults {
            user: Some("root".into()),
            ..Defaults::default()
        };
        let specs = [
            ("web-1", "203.0.113.10", vec!["web", "prod"], false),
            ("db-1", "10.0.0.20", vec!["db", "prod"], false),
            ("db-3", "10.0.0.23", vec!["db"], true),
        ];
        let hosts = specs
            .into_iter()
            .map(|(name, address, tags, disabled)| {
                resolve(
                    &Host {
                        name: name.into(),
                        address: address.into(),
                        tags: tags.into_iter().map(str::to_string).collect(),
                        disabled,
                        ..Host::default()
                    },
                    &defaults,
                    "ada",
                )
            })
            .collect();
        InventoryBundle {
            inventory: Default::default(),
            hosts,
            problems: Vec::new(),
            document: DocumentMut::new(),
        }
    }

    #[test]
    fn json_list_includes_disabled_hosts_when_unfiltered() {
        let result = run(&bundle(), None, true).unwrap();
        assert_eq!(result.code, ExitCode::SUCCESS);
        assert!(result.stdout.contains("\"name\": \"db-3\""));
        assert!(result.stdout.contains("\"disabled\": true"));
    }

    #[test]
    fn selector_filters_and_a_typo_fails() {
        let listed = run(&bundle(), Some("@db"), false).unwrap();
        assert!(listed.stdout.contains("db-1"));
        assert!(!listed.stdout.contains("db-3"));
        let missing = run(&bundle(), Some("wbe-1"), false).unwrap();
        assert_eq!(missing.code, ExitCode::from(1));
        assert!(missing.stderr.contains("did you mean `web-1`"));
    }
}
