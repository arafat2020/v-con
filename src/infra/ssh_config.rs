//! Parse `~/.ssh/config` into host records that can be imported.
//!
//! Only `Host`, `HostName`, `User`, `Port`, `IdentityFile`, and `ProxyJump`
//! affect the result. Other keywords are ignored. Wildcard patterns are
//! reported as skipped instead of becoming hosts. `Include` is followed, and
//! a file that cannot be read becomes a warning.

use crate::domain::host::Host;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// One discovered row, either a concrete host or a pattern that was skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEntry {
    /// The `Host` pattern.
    pub pattern: String,
    /// Present when the pattern is a single concrete name.
    pub host: Option<Host>,
    /// Why a pattern was not imported. `None` for a concrete host.
    pub skip_reason: Option<String>,
}

/// Hosts, skipped patterns, and warnings from one parse.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParseOutput {
    /// Concrete hosts and skipped patterns, in file order.
    pub entries: Vec<ConfigEntry>,
    /// Include failures and other notes that do not reject the file.
    pub warnings: Vec<String>,
}

/// `~/.ssh/config` for the current user.
pub fn default_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".ssh").join("config")
}

/// Parse `text` without reading `Include` files.
pub fn parse(text: &str) -> ParseOutput {
    parse_blocks(text, &[])
}

/// Parse `path` and follow `Include` directives.
pub fn load(path: &Path) -> ParseOutput {
    let mut seen = HashSet::new();
    let mut output = ParseOutput::default();
    load_file(path, &mut seen, &mut output, 0);
    output
}

fn load_file(path: &Path, seen: &mut HashSet<PathBuf>, output: &mut ParseOutput, depth: usize) {
    if depth > 8 {
        output
            .warnings
            .push(format!("stopped following includes at {}", path.display()));
        return;
    }
    let Ok(canonical) = path.canonicalize() else {
        output
            .warnings
            .push(format!("could not read {}", path.display()));
        return;
    };
    if !seen.insert(canonical) {
        return;
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            output
                .warnings
                .push(format!("could not read {}: {error}", path.display()));
            return;
        }
    };
    let includes = includes_in(&text);
    let parsed = parse(&text);
    output.entries.extend(parsed.entries);
    output.warnings.extend(parsed.warnings);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    for include in includes {
        for file in expand_include(parent, &include) {
            load_file(&file, seen, output, depth + 1);
        }
    }
}

fn parse_blocks(text: &str, extra_warnings: &[String]) -> ParseOutput {
    let mut output = ParseOutput {
        warnings: extra_warnings.to_vec(),
        entries: Vec::new(),
    };
    let mut patterns: Vec<String> = Vec::new();
    let mut values = BlockValues::default();
    let mut skipping_match = false;

    for raw in text.lines() {
        let Some((keyword, argument)) = split_directive(raw) else {
            continue;
        };
        let keyword = keyword.to_ascii_lowercase();
        if keyword == "host" {
            skipping_match = false;
            flush(&patterns, &values, &mut output);
            patterns = argument;
            values = BlockValues::default();
            continue;
        }
        if keyword == "match" {
            flush(&patterns, &values, &mut output);
            patterns.clear();
            values = BlockValues::default();
            skipping_match = true;
            output
                .warnings
                .push("skipped a Match block; only Host blocks are imported".to_string());
            continue;
        }
        if skipping_match || patterns.is_empty() {
            continue;
        }
        match keyword.as_str() {
            "hostname" => values.hostname = Some(argument.join(" ")),
            "user" => values.user = Some(argument.join(" ")),
            "identityfile" => {
                values.identity_file = Some(expand_user_path(&argument.join(" ")));
            }
            "proxyjump" => values.proxy_jump = Some(argument.join(" ")),
            "port" => {
                let argument = argument.join(" ");
                match argument.parse::<u16>() {
                    Ok(port) if port != 0 => values.port = Some(port),
                    _ => output
                        .warnings
                        .push(format!("ignored invalid port '{argument}'")),
                }
            }
            _ => {}
        }
    }
    flush(&patterns, &values, &mut output);
    output
}

fn includes_in(text: &str) -> Vec<String> {
    let mut includes = Vec::new();
    for raw in text.lines() {
        let Some((keyword, argument)) = split_directive(raw) else {
            continue;
        };
        if keyword.eq_ignore_ascii_case("include") {
            includes.extend(argument);
        }
    }
    includes
}

#[derive(Debug, Default, Clone)]
struct BlockValues {
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity_file: Option<String>,
    proxy_jump: Option<String>,
}

fn flush(patterns: &[String], values: &BlockValues, output: &mut ParseOutput) {
    for pattern in patterns {
        if let Some(reason) = skip_reason(pattern) {
            output.entries.push(ConfigEntry {
                pattern: pattern.clone(),
                host: None,
                skip_reason: Some(reason),
            });
            continue;
        }
        let mut jump = values.proxy_jump.clone();
        if let Some(proxy) = jump.as_deref()
            && proxy.contains(',')
        {
            let first = proxy.split(',').next().unwrap_or(proxy).trim().to_string();
            output.warnings.push(format!(
                "{pattern} ProxyJump lists multiple hops; imported the first ({first})"
            ));
            jump = Some(first);
        }
        output.entries.push(ConfigEntry {
            pattern: pattern.clone(),
            host: Some(Host {
                name: pattern.clone(),
                address: values.hostname.clone().unwrap_or_else(|| pattern.clone()),
                user: values.user.clone(),
                port: values.port,
                identity_file: values.identity_file.clone(),
                jump,
                tags: vec!["imported".to_string()],
                ..Host::default()
            }),
            skip_reason: None,
        });
    }
}

fn skip_reason(pattern: &str) -> Option<String> {
    if pattern.is_empty()
        || pattern.contains('*')
        || pattern.contains('?')
        || pattern.starts_with('!')
    {
        Some("skipped (wildcard)".to_string())
    } else {
        None
    }
}

fn split_directive(raw: &str) -> Option<(String, Vec<String>)> {
    let stripped = strip_comment(raw);
    let line = stripped.trim();
    if line.is_empty() {
        return None;
    }
    let mut tokens = tokenize(line);
    if tokens.is_empty() {
        return None;
    }
    let keyword = tokens.remove(0);
    if keyword.eq_ignore_ascii_case("host") || keyword.eq_ignore_ascii_case("include") {
        return Some((keyword, tokens));
    }
    if tokens.first().is_some_and(|token| token == "=") {
        tokens.remove(0);
    }
    Some((keyword, vec![tokens.join(" ")]))
}

fn strip_comment(raw: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for ch in raw.chars() {
        if escaped {
            out.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => {
                escaped = true;
                out.push(ch);
            }
            '"' => {
                quoted = !quoted;
                out.push(ch);
            }
            '#' if !quoted => break,
            _ => out.push(ch),
        }
    }
    out
}

fn tokenize(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for ch in line.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => quoted = !quoted,
            ch if ch.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn expand_user_path(path: &str) -> String {
    shellexpand::tilde(path).into_owned()
}

fn expand_include(parent: &Path, pattern: &str) -> Vec<PathBuf> {
    let expanded = shellexpand::tilde(pattern).into_owned();
    let path = PathBuf::from(&expanded);
    let path = if path.is_absolute() {
        path
    } else {
        parent.join(path)
    };
    let text = path.to_string_lossy();
    if text.contains('*') || text.contains('?') {
        return glob::glob(&text)
            .map(|paths| {
                paths
                    .filter_map(Result::ok)
                    .filter(|candidate| candidate.is_file())
                    .collect()
            })
            .unwrap_or_default();
    }
    if path.is_dir() {
        let mut files = std::fs::read_dir(&path)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|candidate| candidate.is_file())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        files.sort();
        return files;
    }
    vec![path]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_concrete_hosts_and_skips_wildcards() {
        let output = parse(
            r#"
            # a comment
            Host web-1 web-*
                HostName 203.0.113.10
                User deploy
                Port 2222
                IdentityFile ~/.ssh/id_ed
                ProxyJump bastion
                ForwardAgent yes

            Host *
                User root

            Match host web-1
                Port 9
            "#,
        );
        assert!(
            output
                .warnings
                .iter()
                .any(|warning| warning.contains("Match"))
        );
        let web = output
            .entries
            .iter()
            .find(|entry| entry.pattern == "web-1")
            .unwrap();
        let host = web.host.as_ref().unwrap();
        assert_eq!(host.address, "203.0.113.10");
        assert_eq!(host.user.as_deref(), Some("deploy"));
        assert_eq!(host.port, Some(2222));
        assert!(
            host.identity_file
                .as_deref()
                .unwrap()
                .ends_with(".ssh/id_ed")
        );
        assert_eq!(host.jump.as_deref(), Some("bastion"));
        assert_eq!(host.tags, vec!["imported".to_string()]);
        let skipped = output
            .entries
            .iter()
            .find(|entry| entry.pattern == "web-*")
            .unwrap();
        assert!(skipped.skip_reason.as_ref().unwrap().contains("wildcard"));
        assert!(output.entries.iter().any(|entry| entry.pattern == "*"));
        assert!(output.entries.iter().all(|entry| entry.pattern != "root"));
    }

    #[test]
    fn include_is_followed_and_a_missing_file_warns() {
        let dir = tempfile::tempdir().unwrap();
        let extra = dir.path().join("extra");
        std::fs::write(&extra, "Host db-1\n    HostName 10.0.0.20\n    User db\n").unwrap();
        let main = dir.path().join("config");
        std::fs::write(
            &main,
            format!(
                "Include {}\nHost web-1\n    HostName 203.0.113.10\n",
                extra.display()
            ),
        )
        .unwrap();
        let output = load(&main);
        assert!(output.entries.iter().any(|entry| entry.pattern == "web-1"));
        assert!(output.entries.iter().any(|entry| entry.pattern == "db-1"));

        let missing = dir.path().join("nope");
        std::fs::write(&main, format!("Include {}\n", missing.display())).unwrap();
        let output = load(&main);
        assert!(
            output
                .warnings
                .iter()
                .any(|warning| warning.contains("could not read"))
        );
    }

    #[test]
    fn quoted_hostname_and_invalid_port_are_handled() {
        let output = parse("Host \"web 1\"\n    HostName \"203.0.113.10\"\n    Port zero\n");
        assert_eq!(output.entries[0].host.as_ref().unwrap().name, "web 1");
        assert_eq!(
            output.entries[0].host.as_ref().unwrap().address,
            "203.0.113.10"
        );
        assert!(
            output
                .warnings
                .iter()
                .any(|warning| warning.contains("port"))
        );
    }
}
