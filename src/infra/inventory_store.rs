//! Read `hosts.toml`.
//!
//! The parsed [`toml_edit::DocumentMut`] is kept beside the serde value so a
//! later edit can change one host without rewriting comments or key order.
//! Writes themselves arrive in phase 3.
//!
//! A missing file is an empty inventory, not an error. A syntax or type error
//! includes the file path and a 1-based line.

use crate::domain::host::{Inventory, Problem, ProblemSite};
use crate::error::{Error, line_number};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use toml_edit::{DocumentMut, Item};

/// A `hosts.toml` that parsed, plus the editable document and source lines.
#[derive(Debug)]
pub struct LoadedInventory {
    /// Path that was requested. Absent files still record the path they would have used.
    #[allow(dead_code)]
    pub path: PathBuf,
    /// Deserialized inventory. Empty when the file does not exist.
    pub inventory: Inventory,
    /// Original document, comments included. Phase 3 edits this instead of reserializing.
    pub document: DocumentMut,
    /// 1-based line of each `[[hosts]]` table, aligned with [`Inventory::hosts`].
    pub host_lines: Vec<Option<usize>>,
    /// 1-based line of `[defaults]`, when that table exists.
    pub defaults_line: Option<usize>,
}

impl LoadedInventory {
    /// An inventory with no hosts and an empty document. Used when the file is missing.
    pub fn empty(path: PathBuf) -> Self {
        Self {
            path,
            inventory: Inventory::default(),
            document: DocumentMut::new(),
            host_lines: Vec::new(),
            defaults_line: None,
        }
    }
}

/// Load `path`. A missing file returns [`LoadedInventory::empty`].
pub fn load(path: &Path) -> Result<LoadedInventory, Error> {
    if !path.exists() {
        return Ok(LoadedInventory::empty(path.to_path_buf()));
    }
    let text = fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse_text(path, &text)
}

/// Parse `text` as an inventory. `path` is only used in error messages.
pub fn parse_text(path: &Path, text: &str) -> Result<LoadedInventory, Error> {
    let document = DocumentMut::from_str(text)
        .map_err(|error| Error::parse(path, text, error.span(), error.message()))?;
    let inventory: Inventory = toml_edit::de::from_str(text)
        .map_err(|error| Error::parse(path, text, error.span(), error.message()))?;
    let host_lines = host_line_numbers(&document, text);
    let defaults_line = document
        .get("defaults")
        .and_then(Item::span)
        .map(|span| line_number(text, span.start))
        .or_else(|| header_line(text, "[defaults]"));
    Ok(LoadedInventory {
        path: path.to_path_buf(),
        inventory,
        document,
        host_lines,
        defaults_line,
    })
}

/// Copy source lines from `loaded` onto validation problems.
pub fn attach_lines(problems: &mut [Problem], loaded: &LoadedInventory) {
    for problem in problems {
        problem.line = match problem.site {
            ProblemSite::Host(index) => loaded.host_lines.get(index).copied().flatten(),
            ProblemSite::Defaults => loaded.defaults_line,
        };
    }
}

/// Prefer spans from the TOML parser. When a span is missing, fall back to
/// scanning for `[[hosts]]` headers, which is where the tables start in a hand-written file.
fn host_line_numbers(document: &DocumentMut, text: &str) -> Vec<Option<usize>> {
    let spanned = document
        .get("hosts")
        .and_then(Item::as_array_of_tables)
        .map(|tables| {
            tables
                .iter()
                .map(|table| table.span().map(|span| line_number(text, span.start)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if spanned.iter().any(Option::is_some) {
        return spanned;
    }
    text.lines()
        .enumerate()
        .filter(|(_, line)| line.trim() == "[[hosts]]")
        .map(|(index, _)| Some(index + 1))
        .collect()
}

fn header_line(text: &str, header: &str) -> Option<usize> {
    text.lines()
        .position(|line| line.trim() == header)
        .map(|index| index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::host::validate_inventory;

    #[test]
    fn missing_file_is_an_empty_inventory() {
        let path =
            std::env::temp_dir().join(format!("vcon-missing-{}-hosts.toml", std::process::id()));
        let _ = fs::remove_file(&path);
        let loaded = load(&path).unwrap();
        assert!(loaded.inventory.hosts.is_empty());
        assert!(validate_inventory(&loaded.inventory).is_empty());
    }

    #[test]
    fn parse_error_includes_path_and_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        fs::write(&path, "[[hosts]\nname = \"web-1\"\n").unwrap();
        let error = load(&path).unwrap_err();
        let message = error.to_string();
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains(":1:"), "{message}");
    }

    #[test]
    fn type_error_includes_the_offending_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        fs::write(
            &path,
            "[[hosts]]\nname = \"web-1\"\naddress = \"10.0.0.1\"\nport = \"nope\"\n",
        )
        .unwrap();
        let error = load(&path).unwrap_err();
        let message = error.to_string();
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains(":4:"), "{message}");
    }

    #[test]
    fn comments_survive_in_the_document_and_validation_gets_a_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        let text = "# keep me\n[[hosts]]\nname = \"Web-1\"\naddress = \"10.0.0.1\"\n";
        fs::write(&path, text).unwrap();
        let loaded = load(&path).unwrap();
        assert!(loaded.document.to_string().contains("# keep me"));
        let mut problems = validate_inventory(&loaded.inventory);
        attach_lines(&mut problems, &loaded);
        assert!(
            problems.iter().any(|problem| problem.line == Some(2)),
            "{problems:?}"
        );
    }
}
