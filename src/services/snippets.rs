//! Load and save `snippets.toml`.
//!
//! Missing files are an empty list. Writes use `toml_edit` so comments and key
//! order are preserved when possible, matching the inventory store style.

use crate::domain::snippet::Snippet;
use crate::error::Error;
use crate::infra::inventory_store::write_atomic;
use serde::Deserialize;
use std::path::Path;
use std::str::FromStr;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

/// On-disk document plus the deserialized snippet list.
#[derive(Debug, Clone)]
pub struct SnippetStore {
    /// Path that was loaded (or would be written).
    pub path: std::path::PathBuf,
    /// Current snippets.
    pub snippets: Vec<Snippet>,
    /// Editable document for style-preserving saves.
    document: DocumentMut,
}

#[derive(Debug, Deserialize)]
struct FileShape {
    #[serde(default)]
    snippets: Vec<Snippet>,
}

impl SnippetStore {
    /// Load `path`. A missing file yields an empty store.
    pub fn load(path: &Path) -> Result<Self, Error> {
        if !path.exists() {
            return Ok(Self {
                path: path.to_path_buf(),
                snippets: Vec::new(),
                document: DocumentMut::new(),
            });
        }
        let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let document = DocumentMut::from_str(&text)
            .map_err(|error| Error::parse(path, &text, error.span(), error.message()))?;
        let shape: FileShape = toml_edit::de::from_str(&text)
            .map_err(|error| Error::parse(path, &text, error.span(), error.message()))?;
        Ok(Self {
            path: path.to_path_buf(),
            snippets: shape.snippets,
            document,
        })
    }

    /// All snippets in file order.
    pub fn list(&self) -> &[Snippet] {
        &self.snippets
    }

    /// Find a snippet by exact name.
    pub fn get(&self, name: &str) -> Option<&Snippet> {
        self.snippets.iter().find(|snippet| snippet.name == name)
    }

    /// Insert or replace by name, then write the file.
    pub fn upsert(&mut self, snippet: Snippet) -> Result<(), Error> {
        if let Some(existing) = self
            .snippets
            .iter_mut()
            .find(|candidate| candidate.name == snippet.name)
        {
            *existing = snippet;
        } else {
            self.snippets.push(snippet);
        }
        self.save()
    }

    /// Remove by name. Returns whether anything was removed.
    pub fn delete(&mut self, name: &str) -> Result<bool, Error> {
        let before = self.snippets.len();
        self.snippets.retain(|snippet| snippet.name != name);
        if self.snippets.len() == before {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    /// Rewrite the document from `snippets` and write atomically.
    pub fn save(&mut self) -> Result<(), Error> {
        sync_document(&mut self.document, &self.snippets);
        write_atomic(&self.path, &self.document.to_string())
    }
}

fn sync_document(document: &mut DocumentMut, snippets: &[Snippet]) {
    let mut array = ArrayOfTables::new();
    for snippet in snippets {
        let mut table = Table::new();
        table.insert("name", Item::Value(Value::from(snippet.name.as_str())));
        if let Some(description) = &snippet.description {
            table.insert(
                "description",
                Item::Value(Value::from(description.as_str())),
            );
        }
        table.insert(
            "command",
            Item::Value(Value::from(snippet.command.as_str())),
        );
        if snippet.confirm {
            table.insert("confirm", Item::Value(Value::from(true)));
        }
        if let Some(selector) = &snippet.selector {
            table.insert("selector", Item::Value(Value::from(selector.as_str())));
        }
        array.push(table);
    }
    document.insert("snippets", Item::ArrayOfTables(array));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn missing_file_is_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snippets.toml");
        let store = SnippetStore::load(&path).unwrap();
        assert!(store.list().is_empty());
    }

    #[test]
    fn upsert_and_get_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snippets.toml");
        let mut store = SnippetStore::load(&path).unwrap();
        store
            .upsert(Snippet {
                name: "disk".into(),
                description: Some("df".into()),
                command: "df -h /".into(),
                confirm: false,
                selector: None,
            })
            .unwrap();
        let reloaded = SnippetStore::load(&path).unwrap();
        assert_eq!(reloaded.get("disk").unwrap().command, "df -h /");
    }

    #[test]
    fn delete_removes_snippet() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snippets.toml");
        let mut store = SnippetStore::load(&path).unwrap();
        store
            .upsert(Snippet {
                name: "x".into(),
                description: None,
                command: "true".into(),
                confirm: true,
                selector: Some("@web".into()),
            })
            .unwrap();
        assert!(store.delete("x").unwrap());
        assert!(SnippetStore::load(&path).unwrap().list().is_empty());
    }
}
