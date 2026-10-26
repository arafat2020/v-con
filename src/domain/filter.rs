//! Filter the host table without drawing it.
//!
//! An empty query returns every host in the current tag scope, including
//! disabled hosts. Text that starts with `@` or `!`, or that contains `*`,
//! is parsed as a [selector](crate::domain::selector). Any other text is a
//! fuzzy query: each whitespace-separated word must match the combined name,
//! address, tags, and notes, and the rows are ranked by score.

use crate::domain::host::EffectiveHost;
use crate::domain::selector::{evaluate_selector, parse_selector};
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};

/// Which slice of the inventory the tag tree is showing.
///
/// `Recent` and `Favorites` are visible in the tree and stay empty until
/// phase 2 records connection history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    /// Every host in the inventory.
    All,
    /// Hosts that carry this tag.
    Tag(&'a str),
    /// Virtual group. Always empty in phase 1.
    Recent,
    /// Virtual group. Always empty in phase 1.
    Favorites,
}

impl<'a> Scope<'a> {
    /// Whether this scope includes `host` before the text query is applied.
    pub fn includes(self, host: &EffectiveHost) -> bool {
        match self {
            Scope::All => true,
            Scope::Tag(tag) => host.has_tag(tag),
            Scope::Recent | Scope::Favorites => false,
        }
    }
}

/// A column the fuzzy matcher can highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Address,
    Tags,
    Notes,
}

/// Character indexes, into one [`Field`], that a fuzzy query matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldMatch {
    /// Which column the indexes belong to.
    pub field: Field,
    /// Char offsets into that column's text, sorted and unique.
    pub indices: Vec<u32>,
}

/// One visible row: an index into the full host slice, a rank, and highlight spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostMatch {
    /// Index into the `hosts` slice passed to [`filter`], not into the visible rows.
    pub index: usize,
    /// Higher is a better fuzzy match. Selector matches all use `0` and keep inventory order.
    pub score: u32,
    /// Columns that contributed to the fuzzy match. Empty for selector matches.
    pub spans: Vec<FieldMatch>,
}

/// The visible rows plus any selector error or warning to show in the table title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterOutput {
    /// Matching rows. Fuzzy results are ranked; selector results keep inventory order.
    pub matches: Vec<HostMatch>,
    /// Parse failure or an empty selector result. The table shows this in red.
    pub selector_error: Option<String>,
    /// Terms that matched nothing but did not empty the whole result.
    pub warnings: Vec<String>,
}

/// Whether the search box should parse `query` as a selector instead of fuzzy text.
///
/// The rule is the Hosts screen rule: the text starts with `@` or `!`, or it
/// contains `*`. A `?` glob is still a glob once parsing has started, which is
/// how `vcon ls 'web-?'` works.
pub fn is_selector_query(query: &str) -> bool {
    let query = query.trim();
    query.starts_with('@') || query.starts_with('!') || query.contains('*')
}

/// Filter `hosts` by the tag-tree `scope` and the search `query`.
pub fn filter(hosts: &[EffectiveHost], query: &str, scope: Scope<'_>) -> FilterOutput {
    let query = query.trim();
    if query.is_empty() {
        return FilterOutput {
            matches: indexed(hosts, scope, |_| true, 0),
            selector_error: None,
            warnings: Vec::new(),
        };
    }

    if is_selector_query(query) {
        return filter_selector(hosts, query, scope);
    }

    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut matches = Vec::new();
    let mut haystack_buf = Vec::new();
    for (index, host) in hosts.iter().enumerate() {
        if !scope.includes(host) {
            continue;
        }
        let haystack = joined(host);
        let score = pattern.score(Utf32Str::new(&haystack, &mut haystack_buf), &mut matcher);
        let Some(score) = score else {
            continue;
        };
        matches.push(HostMatch {
            index,
            score,
            spans: field_spans(host, &pattern, &mut matcher),
        });
    }
    matches.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then(left.index.cmp(&right.index))
    });
    FilterOutput {
        matches,
        selector_error: None,
        warnings: Vec::new(),
    }
}

fn filter_selector(hosts: &[EffectiveHost], query: &str, scope: Scope<'_>) -> FilterOutput {
    let terms = match parse_selector(query) {
        Ok(terms) => terms,
        Err(error) => {
            return FilterOutput {
                matches: Vec::new(),
                selector_error: Some(error),
                warnings: Vec::new(),
            };
        }
    };
    let scoped: Vec<EffectiveHost> = hosts
        .iter()
        .filter(|host| scope.includes(host))
        .cloned()
        .collect();
    let selection = evaluate_selector(&terms, &scoped);
    let names: std::collections::HashSet<&str> = selection
        .hosts
        .iter()
        .map(|host| host.name.as_str())
        .collect();
    FilterOutput {
        matches: indexed(hosts, scope, |host| names.contains(host.name.as_str()), 0),
        selector_error: selection.error,
        warnings: selection.warnings,
    }
}

fn indexed(
    hosts: &[EffectiveHost],
    scope: Scope<'_>,
    include: impl Fn(&EffectiveHost) -> bool,
    score: u32,
) -> Vec<HostMatch> {
    hosts
        .iter()
        .enumerate()
        .filter(|(_, host)| scope.includes(host) && include(host))
        .map(|(index, _)| HostMatch {
            index,
            score,
            spans: Vec::new(),
        })
        .collect()
}

fn joined(host: &EffectiveHost) -> String {
    format!(
        "{} {} {} {}",
        host.name,
        host.address,
        host.tags.join(" "),
        host.notes.as_deref().unwrap_or("")
    )
}

fn field_spans(host: &EffectiveHost, pattern: &Pattern, matcher: &mut Matcher) -> Vec<FieldMatch> {
    let tags = host.tags.join(",");
    let notes = host.notes.clone().unwrap_or_default();
    let fields = [
        (Field::Name, host.name.as_str()),
        (Field::Address, host.address.as_str()),
        (Field::Tags, tags.as_str()),
        (Field::Notes, notes.as_str()),
    ];
    let mut spans = Vec::new();
    let mut buf = Vec::new();
    for (field, text) in fields {
        if text.is_empty() {
            continue;
        }
        let mut indices = Vec::new();
        for atom in &pattern.atoms {
            let before = indices.len();
            if atom
                .indices(Utf32Str::new(text, &mut buf), matcher, &mut indices)
                .is_none()
            {
                indices.truncate(before);
            }
        }
        if indices.is_empty() {
            continue;
        }
        indices.sort_unstable();
        indices.dedup();
        spans.push(FieldMatch { field, indices });
    }
    spans
}

/// Index of `current_name` inside `matches`.
///
/// When the host under the cursor is still visible, this returns its row so
/// the cursor does not jump. When it was filtered out, the first remaining row
/// is used. An empty list returns `None`.
pub fn cursor_position(
    current_name: Option<&str>,
    matches: &[HostMatch],
    hosts: &[EffectiveHost],
) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    if let Some(name) = current_name
        && let Some(position) = matches
            .iter()
            .position(|matched| hosts[matched.index].name == name)
    {
        return Some(position);
    }
    Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::host::{Defaults, Host, resolve};

    fn sample() -> Vec<EffectiveHost> {
        let defaults = Defaults::default();
        let rows = [
            (
                "web-1",
                "203.0.113.10",
                vec!["web", "prod"],
                Some("Main nginx box"),
            ),
            (
                "web-2",
                "203.0.113.11",
                vec!["web", "prod"],
                Some("Secondary nginx"),
            ),
            ("db-1", "10.0.0.20", vec!["db", "prod"], None),
            ("db-3", "10.0.0.23", vec!["db", "prod"], None),
            ("cache-1", "10.0.0.30", vec!["cache"], None),
        ];
        rows.into_iter()
            .map(|(name, address, tags, notes)| {
                resolve(
                    &Host {
                        name: name.into(),
                        address: address.into(),
                        tags: tags.into_iter().map(str::to_string).collect(),
                        notes: notes.map(str::to_string),
                        ..Host::default()
                    },
                    &defaults,
                    "ada",
                )
            })
            .collect()
    }

    fn names(output: &FilterOutput, hosts: &[EffectiveHost]) -> Vec<String> {
        output
            .matches
            .iter()
            .map(|matched| hosts[matched.index].name.clone())
            .collect()
    }

    #[test]
    fn empty_query_keeps_every_host_in_order() {
        let hosts = sample();
        let output = filter(&hosts, "   ", Scope::All);
        assert_eq!(
            names(&output, &hosts),
            vec!["web-1", "web-2", "db-1", "db-3", "cache-1"]
        );
    }

    #[test]
    fn fuzzy_query_requires_every_word() {
        let hosts = sample();
        let output = filter(&hosts, "pr ng", Scope::All);
        let mut found = names(&output, &hosts);
        found.sort();
        assert_eq!(found, vec!["web-1", "web-2"]);
        assert!(output.matches.iter().any(|matched| {
            matched.spans.iter().any(|span| {
                span.field == Field::Name || span.field == Field::Notes || span.field == Field::Tags
            })
        }));
    }

    #[test]
    fn selector_query_uses_selector_rules() {
        let hosts = sample();
        let output = filter(&hosts, "@db,!db-3", Scope::All);
        assert_eq!(names(&output, &hosts), vec!["db-1"]);
        assert!(output.selector_error.is_none());
    }

    #[test]
    fn glob_inside_the_query_is_a_selector() {
        let hosts = sample();
        let output = filter(&hosts, "web-*", Scope::All);
        assert_eq!(names(&output, &hosts), vec!["web-1", "web-2"]);
    }

    #[test]
    fn bad_selector_is_reported_inline() {
        let hosts = sample();
        let output = filter(&hosts, "@", Scope::All);
        assert!(output.matches.is_empty());
        assert!(output.selector_error.is_some());
    }

    #[test]
    fn tag_scope_and_recent_group() {
        let hosts = sample();
        assert_eq!(
            names(&filter(&hosts, "", Scope::Tag("db")), &hosts),
            vec!["db-1", "db-3"]
        );
        assert!(filter(&hosts, "", Scope::Recent).matches.is_empty());
        assert!(filter(&hosts, "", Scope::Favorites).matches.is_empty());
    }

    #[test]
    fn cursor_stays_on_the_same_host_when_it_remains_visible() {
        let hosts = sample();
        let all = filter(&hosts, "", Scope::All);
        let narrowed = filter(&hosts, "@db", Scope::All);
        let db1 = narrowed
            .matches
            .iter()
            .position(|matched| hosts[matched.index].name == "db-1");
        assert_eq!(cursor_position(Some("db-1"), &all.matches, &hosts), Some(2));
        assert_eq!(
            cursor_position(Some("db-1"), &narrowed.matches, &hosts),
            db1
        );
        assert_eq!(
            cursor_position(Some("cache-1"), &narrowed.matches, &hosts),
            Some(0)
        );
        assert_eq!(cursor_position(Some("web-1"), &[], &hosts), None);
    }
}
