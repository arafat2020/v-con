//! Snippet templates and shell-safe variable substitution.
//!
//! Templates use `{{var}}` (required) and `{{var|default}}` (optional default).
//! Every substituted value is wrapped in single quotes so spaces, `;`, and
//! `$()` cannot break out of the argument the template author intended.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// One saved command template from `snippets.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snippet {
    /// Unique short name used by `vcon snippet run`.
    pub name: String,
    /// Optional human description shown in the picker.
    #[serde(default)]
    pub description: Option<String>,
    /// Remote command template with `{{var}}` placeholders.
    pub command: String,
    /// Always ask before running when true.
    #[serde(default)]
    pub confirm: bool,
    /// Optional default host selector when the TUI has no selection.
    #[serde(default)]
    pub selector: Option<String>,
}

/// One variable discovered in a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarSpec {
    /// Placeholder name without braces.
    pub name: String,
    /// Default from `{{name|default}}`, if any.
    pub default: Option<String>,
}

/// Why [`render`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingVars {
    /// Variable names that had no value and no default.
    pub names: Vec<String>,
}

impl fmt::Display for MissingVars {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "missing variables: {}", self.names.join(", "))
    }
}

/// List unique variables in order of first appearance.
pub fn list_variables(command: &str) -> Vec<VarSpec> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (name, default) in iter_placeholders(command) {
        if seen.insert(name.clone()) {
            out.push(VarSpec { name, default });
        }
    }
    out
}

/// Substitute `vars` into `command`, shell-quoting every value.
///
/// Required placeholders without a map entry and without a default become
/// [`MissingVars`]. Defaults fill gaps; map values always win.
pub fn render(command: &str, vars: &BTreeMap<String, String>) -> Result<String, MissingVars> {
    let mut missing = Vec::new();
    let mut out = String::with_capacity(command.len());
    let mut rest = command;
    while let Some((before, name, default, after)) = next_placeholder(rest) {
        out.push_str(before);
        let value = vars
            .get(&name)
            .cloned()
            .or_else(|| default.clone())
            .or_else(|| {
                missing.push(name.clone());
                None
            });
        if let Some(value) = value {
            out.push_str(&shell_single_quote(&value));
        } else {
            // Keep a visible marker so partial renders are never silent.
            out.push_str(&format!("{{{{{name}}}}}"));
        }
        rest = after;
    }
    out.push_str(rest);
    if missing.is_empty() {
        Ok(out)
    } else {
        missing.sort();
        missing.dedup();
        Err(MissingVars { names: missing })
    }
}

/// Wrap `value` in single quotes; escape embedded `'` as `'\''`.
pub fn shell_single_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

fn iter_placeholders(command: &str) -> impl Iterator<Item = (String, Option<String>)> + '_ {
    PlaceholderIter { rest: command }
}

struct PlaceholderIter<'a> {
    rest: &'a str,
}

impl Iterator for PlaceholderIter<'_> {
    type Item = (String, Option<String>);

    fn next(&mut self) -> Option<Self::Item> {
        let (_, name, default, after) = next_placeholder(self.rest)?;
        self.rest = after;
        Some((name, default))
    }
}

fn next_placeholder(input: &str) -> Option<(&str, String, Option<String>, &str)> {
    let start = input.find("{{")?;
    let after_open = start + 2;
    let end_rel = input[after_open..].find("}}")?;
    let end = after_open + end_rel;
    let inner = input[after_open..end].trim();
    if inner.is_empty() {
        return next_placeholder(&input[end + 2..]).map(|(before, name, default, after)| {
            (&input[..start + 2 + before.len()], name, default, after)
        });
    }
    let (name, default) = match inner.split_once('|') {
        Some((name, default)) => (name.trim().to_string(), Some(default.to_string())),
        None => (inner.to_string(), None),
    };
    Some((&input[..start], name, default, &input[end + 2..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn lists_required_and_default_vars_in_order() {
        let specs = list_variables("sudo journalctl -u {{service}} -n {{lines|100}}");
        assert_eq!(
            specs,
            vec![
                VarSpec {
                    name: "service".into(),
                    default: None,
                },
                VarSpec {
                    name: "lines".into(),
                    default: Some("100".into()),
                },
            ]
        );
    }

    #[test]
    fn quotes_spaces() {
        let rendered = render("echo {{msg}}", &vars(&[("msg", "hello world")])).unwrap();
        assert_eq!(rendered, "echo 'hello world'");
    }

    #[test]
    fn escapes_embedded_single_quotes() {
        let rendered = render("echo {{msg}}", &vars(&[("msg", "it's")])).unwrap();
        assert_eq!(rendered, "echo 'it'\\''s'");
    }

    #[test]
    fn quotes_semicolon_injection() {
        let rendered = render("echo {{msg}}", &vars(&[("msg", "; rm -rf /")])).unwrap();
        assert_eq!(rendered, "echo '; rm -rf /'");
    }

    #[test]
    fn quotes_command_substitution() {
        let rendered = render("echo {{msg}}", &vars(&[("msg", "$(whoami)")])).unwrap();
        assert_eq!(rendered, "echo '$(whoami)'");
    }

    #[test]
    fn missing_required_vars_error() {
        let err = render("restart {{service}}", &BTreeMap::new()).unwrap_err();
        assert_eq!(err.names, vec!["service".to_string()]);
    }

    #[test]
    fn defaults_fill_missing_map_entries() {
        let rendered = render("n={{lines|100}}", &BTreeMap::new()).unwrap();
        assert_eq!(rendered, "n='100'");
    }

    #[test]
    fn map_overrides_default() {
        let rendered = render("n={{lines|100}}", &vars(&[("lines", "20")])).unwrap();
        assert_eq!(rendered, "n='20'");
    }

    #[test]
    fn list_variables_and_render_never_panic_on_random_templates() {
        use proptest::prelude::*;
        proptest!(|(template in ".*", key in "[a-z]{0,8}", value in ".*")| {
            let specs = list_variables(&template);
            let mut map = BTreeMap::new();
            if !key.is_empty() {
                map.insert(key, value);
            }
            let _ = render(&template, &map);
            let _ = specs.len();
        });
    }
}
