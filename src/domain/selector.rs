//! Selector grammar shared by `vcon ls` and the Hosts screen search box.
//!
//! A selector is a comma-separated list of terms. Positive terms are unioned.
//! A selector made only of `!` exclusions starts from every enabled host, then
//! exclusions are removed. The result stays in inventory order.

use crate::domain::host::{EffectiveHost, is_plain_jump};

/// One term in a selector such as `web-1`, `web-*`, `@prod`, `@web+prod`, `all`, or `!web-3`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// Exact host name, including disabled hosts.
    Exact(String),
    /// Name glob using `*` and `?`. Disabled hosts are skipped.
    Glob(String),
    /// Every enabled host that has this tag.
    Tag(String),
    /// Every enabled host that has all of these tags.
    TagIntersection(Vec<String>),
    /// Every enabled host.
    All,
    /// Remove hosts matched by the inner term.
    Exclude(Box<Term>),
}

/// Hosts selected by a parsed selector, plus warnings for terms that matched nothing.
///
/// `error` is set when the final set is empty. A typo in an otherwise useful
/// selector is only a warning, so `@web,no-such` still returns the web hosts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection<'a> {
    /// Matching hosts in inventory order.
    pub hosts: Vec<&'a EffectiveHost>,
    /// Terms that matched nothing, including "did you mean?" text for bare names.
    pub warnings: Vec<String>,
    /// Set when nothing remains after exclusions. Running a command on zero hosts is refused.
    pub error: Option<String>,
}

/// Parse a comma-separated selector. Whitespace around terms is ignored.
///
/// An empty input, an empty tag (`@`), or a broken glob is an error. A bare
/// word that is not a host name is still a valid term; evaluation reports it.
pub fn parse_selector(input: &str) -> Result<Vec<Term>, String> {
    let mut terms = Vec::new();
    for part in input.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(negated) = part.strip_prefix('!') {
            if negated.is_empty() {
                return Err("empty exclusion".to_string());
            }
            terms.push(Term::Exclude(Box::new(parse_single_term(negated)?)));
        } else {
            terms.push(parse_single_term(part)?);
        }
    }
    if terms.is_empty() {
        return Err("empty selector".to_string());
    }
    Ok(terms)
}

fn parse_single_term(term: &str) -> Result<Term, String> {
    if term.is_empty() {
        return Err("empty selector term".to_string());
    }
    if term == "all" {
        return Ok(Term::All);
    }
    if let Some(tags) = term.strip_prefix('@') {
        if tags.is_empty() {
            return Err("empty tag selector".to_string());
        }
        if tags.contains('+') {
            let tags: Vec<String> = tags.split('+').map(|tag| tag.trim().to_string()).collect();
            if tags.iter().any(|tag| tag.is_empty()) {
                return Err(format!(
                    "invalid tag selector '@{tags}'",
                    tags = tags.join("+")
                ));
            }
            return Ok(Term::TagIntersection(tags));
        }
        return Ok(Term::Tag(tags.to_string()));
    }
    if term.contains('*') || term.contains('?') {
        glob::Pattern::new(term).map_err(|error| format!("invalid glob '{term}': {error}"))?;
        return Ok(Term::Glob(term.to_string()));
    }
    if !is_plain_jump(term) && term.contains(':') {
        return Err(format!("'{term}' is not a host name"));
    }
    Ok(Term::Exact(term.to_string()))
}

/// Apply the evaluation rules and suggest a close host name for an unknown exact name.
///
/// Suggestions use Jaro-Winkler and only appear when the closest name scores
/// at least 0.84, which catches a transposition such as `wbe-1` → `web-1`.
pub fn evaluate_selector<'a>(terms: &[Term], hosts: &'a [EffectiveHost]) -> Selection<'a> {
    let mut warnings = Vec::new();
    let positives: Vec<&Term> = terms.iter().filter(|term| !is_exclude(term)).collect();
    let mut matched = vec![false; hosts.len()];

    if positives.is_empty() {
        for (index, host) in hosts.iter().enumerate() {
            matched[index] = !host.disabled;
        }
    } else {
        for term in positives {
            let mut any = false;
            for (index, host) in hosts.iter().enumerate() {
                if matches_positive(term, host) {
                    matched[index] = true;
                    any = true;
                }
            }
            if !any {
                warnings.push(unmatched_message(term, hosts));
            }
        }
    }

    for term in terms {
        let Term::Exclude(inner) = term else {
            continue;
        };
        let mut any = false;
        for (index, host) in hosts.iter().enumerate() {
            if matches_positive(inner, host) {
                matched[index] = false;
                any = true;
            }
        }
        if !any {
            warnings.push(unmatched_message(inner, hosts));
        }
    }

    let selected: Vec<&EffectiveHost> = hosts
        .iter()
        .enumerate()
        .filter(|(index, _)| matched[*index])
        .map(|(_, host)| host)
        .collect();

    let error = if selected.is_empty() {
        Some(if warnings.len() == 1 {
            warnings[0].clone()
        } else {
            "selector matched no hosts".to_string()
        })
    } else {
        None
    };

    Selection {
        hosts: selected,
        warnings,
        error,
    }
}

fn is_exclude(term: &Term) -> bool {
    matches!(term, Term::Exclude(_))
}

/// `all`, tags, and globs ignore disabled hosts. An exact name still matches one.
fn matches_positive(term: &Term, host: &EffectiveHost) -> bool {
    match term {
        Term::Exact(name) => host.name == *name,
        Term::Glob(pattern) => {
            glob::Pattern::new(pattern)
                .map(|pattern| pattern.matches(&host.name))
                .unwrap_or(false)
                && !host.disabled
        }
        Term::Tag(tag) => !host.disabled && host.has_tag(tag),
        Term::TagIntersection(tags) => !host.disabled && tags.iter().all(|tag| host.has_tag(tag)),
        Term::All => !host.disabled,
        Term::Exclude(_) => false,
    }
}

fn unmatched_message(term: &Term, hosts: &[EffectiveHost]) -> String {
    match term {
        Term::Exact(name) => match suggest(name, hosts) {
            Some(guess) => format!("no host `{name}`, did you mean `{guess}`?"),
            None => format!("no host `{name}`"),
        },
        Term::Glob(pattern) => format!("glob `{pattern}` matched no hosts"),
        Term::Tag(tag) => format!("no host tagged `{tag}`"),
        Term::TagIntersection(tags) => format!("no host tagged `{}`", tags.join("+")),
        Term::All => "no enabled hosts".to_string(),
        Term::Exclude(_) => "exclusion matched no hosts".to_string(),
    }
}

fn suggest(name: &str, hosts: &[EffectiveHost]) -> Option<String> {
    let mut best: Option<(f64, &str)> = None;
    for host in hosts {
        if host.name == name {
            continue;
        }
        let score = strsim::jaro_winkler(name, &host.name);
        let replace = match best {
            None => score >= 0.84,
            Some((best_score, best_name)) => {
                score > best_score
                    || ((score - best_score).abs() < f64::EPSILON && host.name.as_str() < best_name)
            }
        };
        if replace {
            best = Some((score, host.name.as_str()));
        }
    }
    best.map(|(_, guess)| guess.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::host::{Defaults, Host, resolve};
    use std::collections::BTreeSet;

    fn hosts() -> Vec<EffectiveHost> {
        let defaults = Defaults::default();
        let specs = [
            ("bastion", "198.51.100.5", vec!["infra"], false),
            ("web-1", "203.0.113.10", vec!["web", "prod"], false),
            ("web-2", "203.0.113.11", vec!["web", "prod"], false),
            ("web-3", "203.0.113.12", vec!["web", "staging"], false),
            ("db-1", "10.0.0.20", vec!["db", "prod"], false),
            ("db-3", "10.0.0.23", vec!["db", "prod"], true),
        ];
        specs
            .into_iter()
            .map(|(name, address, tags, disabled)| {
                let mut host = Host {
                    name: name.to_string(),
                    address: address.to_string(),
                    tags: tags.into_iter().map(str::to_string).collect(),
                    disabled,
                    ..Host::default()
                };
                host.disabled = disabled;
                resolve(&host, &defaults, "ada")
            })
            .collect()
    }

    fn selected(selector: &str) -> BTreeSet<String> {
        let hosts = hosts();
        let terms = parse_selector(selector).unwrap();
        evaluate_selector(&terms, &hosts)
            .hosts
            .into_iter()
            .map(|host| host.name.clone())
            .collect()
    }

    fn outcome(selector: &str) -> (Vec<String>, Vec<String>, Option<String>) {
        let hosts = hosts();
        let terms = parse_selector(selector).unwrap();
        let selection = evaluate_selector(&terms, &hosts);
        (
            selection
                .hosts
                .into_iter()
                .map(|host| host.name.clone())
                .collect(),
            selection.warnings,
            selection.error,
        )
    }

    #[test]
    fn examples_from_the_data_model() {
        assert_eq!(selected("web-1"), set(&["web-1"]));
        assert_eq!(selected("web-*"), set(&["web-1", "web-2", "web-3"]));
        assert_eq!(selected("@prod"), set(&["web-1", "web-2", "db-1"]));
        assert_eq!(selected("@web+prod"), set(&["web-1", "web-2"]));
        assert_eq!(
            selected("@web,@db,!web-3"),
            set(&["web-1", "web-2", "db-1"])
        );
        assert_eq!(
            selected("!@staging"),
            set(&["bastion", "web-1", "web-2", "db-1"])
        );
    }

    #[test]
    fn keeps_inventory_order() {
        let hosts = hosts();
        let terms = parse_selector("@prod").unwrap();
        let names: Vec<_> = evaluate_selector(&terms, &hosts)
            .hosts
            .iter()
            .map(|host| host.name.as_str())
            .collect();
        assert_eq!(names, ["web-1", "web-2", "db-1"]);
    }

    #[test]
    fn exact_name_selects_a_disabled_host() {
        assert_eq!(selected("db-3"), set(&["db-3"]));
        assert!(!selected("all").contains("db-3"));
        assert!(!selected("@db").contains("db-3"));
        assert!(!selected("db-*").contains("db-3"));
    }

    #[test]
    fn suggests_a_close_host_name() {
        let (hosts, warnings, error) = outcome("wbe-1");
        assert!(hosts.is_empty());
        assert_eq!(
            warnings,
            vec!["no host `wbe-1`, did you mean `web-1`?".to_string()]
        );
        assert_eq!(
            error.as_deref(),
            Some("no host `wbe-1`, did you mean `web-1`?")
        );
    }

    #[test]
    fn unmatched_term_warns_without_failing_a_wider_selector() {
        let (hosts, warnings, error) = outcome("@web,no-such");
        assert_eq!(
            hosts,
            vec![
                "web-1".to_string(),
                "web-2".to_string(),
                "web-3".to_string()
            ]
        );
        assert!(warnings.iter().any(|warning| warning.contains("no-such")));
        assert!(error.is_none());
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }
}
