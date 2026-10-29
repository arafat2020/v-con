//! Read and write `hosts.toml`.
//!
//! The parsed [`toml_edit::DocumentMut`] is kept beside the serde value so an
//! edit can change one host without rewriting comments or key order.
//!
//! A missing file is an empty inventory, not an error. A syntax or type error
//! includes the file path and a 1-based line. Writes validate the whole
//! inventory first, then replace the file by renaming a temporary file in the
//! same directory. The previous contents are kept as `hosts.toml.bak`.

use crate::domain::host::{Host, Inventory, Problem, ProblemSite, validate_inventory};
use crate::error::{Error, line_number};
use crate::infra::fsutil::{self, FILE_MODE};
use fs4::fs_std::FileExt;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value};

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

/// Add `host` and write the file when the whole inventory is still valid.
pub fn add_host(path: &Path, host: Host) -> Result<LoadedInventory, Error> {
    mutate(path, |loaded| {
        insert_host(&mut loaded.document, None, &host)
    })
}

/// Replace the host named `old_name`. A new name also rewrites jump references.
pub fn update_host(path: &Path, old_name: &str, host: Host) -> Result<LoadedInventory, Error> {
    let old_name = old_name.to_string();
    mutate(path, |loaded| {
        insert_host(&mut loaded.document, Some(old_name.as_str()), &host)
    })
}

/// Remove `name`. The caller must already have refused hosts that jump through it.
pub fn remove_host(path: &Path, name: &str) -> Result<LoadedInventory, Error> {
    let name = name.to_string();
    mutate(path, |loaded| {
        let array = hosts_array(&mut loaded.document)?;
        let index = array
            .iter()
            .position(|table| table_name(table) == Some(name.as_str()))
            .ok_or_else(|| Error::Message(format!("no host `{name}`")))?;
        array.remove(index);
        Ok(())
    })
}

/// Add or replace each host, then validate once.
///
/// Hosts whose names already exist are left unchanged unless `overwrite` is set.
pub fn import_hosts(
    path: &Path,
    hosts: &[Host],
    overwrite: bool,
) -> Result<LoadedInventory, Error> {
    let hosts = hosts.to_vec();
    mutate(path, move |loaded| {
        for host in &hosts {
            let exists = document_has_host(&loaded.document, &host.name);
            if exists && !overwrite {
                continue;
            }
            let old_name = exists.then_some(host.name.as_str());
            insert_host(&mut loaded.document, old_name, host)?;
        }
        Ok(())
    })
}

fn document_has_host(document: &DocumentMut, name: &str) -> bool {
    document
        .get("hosts")
        .and_then(Item::as_array_of_tables)
        .is_some_and(|array| array.iter().any(|table| table_name(table) == Some(name)))
}

fn mutate(
    path: &Path,
    edit: impl FnOnce(&mut LoadedInventory) -> Result<(), Error>,
) -> Result<LoadedInventory, Error> {
    with_inventory_lock(path, || {
        let mut loaded = load(path)?;
        edit(&mut loaded)?;
        let text = loaded.document.to_string();
        let reparsed = parse_text(path, &text)?;
        let problems = validate_inventory(&reparsed.inventory);
        if !problems.is_empty() {
            return Err(Error::Message(format_problems(&problems)));
        }
        write_atomic(path, &text)?;
        parse_text(path, &text)
    })
}

/// Exclusive lock around inventory read-modify-write so two processes cannot clobber the file.
fn with_inventory_lock<T>(
    path: &Path,
    body: impl FnOnce() -> Result<T, Error>,
) -> Result<T, Error> {
    let lock_path = inventory_lock_path(path);
    if let Some(parent) = lock_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fsutil::create_dir_secure(parent).map_err(|source| Error::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| Error::Write {
            path: lock_path.clone(),
            source,
        })?;
    file.lock_exclusive().map_err(|source| Error::Write {
        path: lock_path.clone(),
        source,
    })?;
    let result = body();
    let _ = FileExt::unlock(&file);
    let _ = file;
    result
}

fn inventory_lock_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "hosts.toml".to_string());
    parent.join(format!(".{name}.lock"))
}

fn format_problems(problems: &[Problem]) -> String {
    problems
        .iter()
        .map(|problem| problem.message.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

fn insert_host(
    document: &mut DocumentMut,
    old_name: Option<&str>,
    host: &Host,
) -> Result<(), Error> {
    if let Some(old_name) = old_name {
        let index = hosts_array(document)?
            .iter()
            .position(|table| table_name(table) == Some(old_name))
            .ok_or_else(|| Error::Message(format!("no host `{old_name}`")))?;
        write_owned_fields(
            hosts_array(document)?.get_mut(index).expect("index exists"),
            host,
        );
        if old_name != host.name {
            retarget_jumps(document, old_name, &host.name);
        }
    } else {
        let mut table = Table::new();
        write_owned_fields(&mut table, host);
        write_carried(&mut table, host);
        hosts_array(document)?.push(table);
    }
    Ok(())
}

fn hosts_array(document: &mut DocumentMut) -> Result<&mut ArrayOfTables, Error> {
    if document.get("hosts").is_none() {
        document.insert("hosts", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    document
        .get_mut("hosts")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| Error::Message("hosts must be an array of [[hosts]] tables".to_string()))
}

fn retarget_jumps(document: &mut DocumentMut, old_name: &str, new_name: &str) {
    if let Some(array) = document
        .get_mut("hosts")
        .and_then(Item::as_array_of_tables_mut)
    {
        for table in array.iter_mut() {
            if table_str(table, "jump") == Some(old_name) {
                set_string(table, "jump", new_name);
            }
        }
    }
    if let Some(defaults) = document.get_mut("defaults").and_then(Item::as_table_mut)
        && table_str(defaults, "jump") == Some(old_name)
    {
        set_string(defaults, "jump", new_name);
    }
}

fn write_owned_fields(table: &mut Table, host: &Host) {
    set_string(table, "name", &host.name);
    set_string(table, "address", &host.address);
    set_optional_string(table, "user", host.user.as_deref());
    match host.port {
        Some(port) => set_integer(table, "port", i64::from(port)),
        None => {
            table.remove("port");
        }
    }
    set_optional_string(table, "identity_file", host.identity_file.as_deref());
    set_optional_string(table, "jump", host.jump.as_deref());
    if host.tags.is_empty() {
        table.remove("tags");
    } else {
        let mut tags = Array::new();
        for tag in &host.tags {
            tags.push(tag.as_str());
        }
        table.insert("tags", Item::Value(Value::Array(tags)));
    }
    set_optional_string(table, "notes", host.notes.as_deref());
}

fn set_string(table: &mut Table, key: &str, value: &str) {
    set_value(table, key, Value::from(value));
}

fn set_integer(table: &mut Table, key: &str, value: i64) {
    set_value(table, key, Value::from(value));
}

fn set_optional_string(table: &mut Table, key: &str, value: Option<&str>) {
    match value {
        Some(value) => set_string(table, key, value),
        None => {
            table.remove(key);
        }
    }
}

fn set_value(table: &mut Table, key: &str, value: Value) {
    if let Some(Item::Value(existing)) = table.get_mut(key) {
        let decor = existing.decor().clone();
        *existing = value;
        *existing.decor_mut() = decor;
    } else {
        table.insert(key, Item::Value(value));
    }
}

fn write_carried(table: &mut Table, host: &Host) {
    if host.disabled {
        set_value(table, "disabled", Value::from(true));
    }
    if !host.ssh_options.is_empty() {
        let mut inline = toml_edit::InlineTable::new();
        for (key, value) in &host.ssh_options {
            inline.insert(key, Value::from(value.as_str()));
        }
        set_value(table, "ssh_options", Value::InlineTable(inline));
    }
    if !host.tunnels.is_empty() {
        let mut tunnels = ArrayOfTables::new();
        for tunnel in &host.tunnels {
            let mut row = Table::new();
            row.insert("name", Item::Value(Value::from(tunnel.name.as_str())));
            row.insert("spec", Item::Value(Value::from(tunnel.spec.as_str())));
            tunnels.push(row);
        }
        table.insert("tunnels", Item::ArrayOfTables(tunnels));
    }
}

fn table_name(table: &Table) -> Option<&str> {
    table_str(table, "name")
}

fn table_str<'a>(table: &'a Table, key: &str) -> Option<&'a str> {
    table.get(key).and_then(Item::as_str)
}

/// Write `text` by renaming a temporary file. The previous file becomes `*.bak`.
///
/// Parent directories are created `0700`. The final file is set to `0600`.
pub fn write_atomic(path: &Path, text: &str) -> Result<(), Error> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fsutil::create_dir_secure(parent).map_err(|source| Error::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "hosts.toml".to_string());
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(".{file_name}.tmp"));
    let backup = parent.join(format!("{file_name}.bak"));
    if path.exists() {
        fs::copy(path, &backup).map_err(|source| Error::Write {
            path: backup.clone(),
            source,
        })?;
    }
    fs::write(&temporary, text).map_err(|source| Error::Write {
        path: temporary.clone(),
        source,
    })?;
    fs::rename(&temporary, path).map_err(|source| Error::Write {
        path: path.to_path_buf(),
        source,
    })?;
    fsutil::set_mode(path, FILE_MODE).map_err(|source| Error::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
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

    fn sample() -> &'static str {
        r#"# keep me
[[hosts]]
name = "web-1" # the first
address = "203.0.113.10"

# second stays
[[hosts]]
name = "db-1"
address = "10.0.0.20"
jump = "web-1"
"#
    }

    #[test]
    fn add_edit_and_remove_keep_comments_and_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        fs::write(&path, sample()).unwrap();

        let added = Host {
            name: "cache-1".into(),
            address: "10.0.0.30".into(),
            tags: vec!["cache".into()],
            ..Host::default()
        };
        add_host(&path, added).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep me"), "{text}");
        assert!(text.contains("# the first"), "{text}");
        assert!(text.contains("# second stays"), "{text}");
        assert!(text.contains("cache-1"), "{text}");
        assert!(
            fs::read_to_string(dir.path().join("hosts.toml.bak"))
                .unwrap()
                .contains("web-1")
        );

        let mut web = load(&path).unwrap().inventory.hosts.remove(0);
        web.address = "203.0.113.99".into();
        web.name = "edge".into();
        update_host(&path, "web-1", web).unwrap();
        let loaded = load(&path).unwrap();
        let text = loaded.document.to_string();
        assert!(text.contains("# the first"), "{text}");
        assert_eq!(
            loaded
                .inventory
                .hosts
                .iter()
                .find(|host| host.name == "db-1")
                .unwrap()
                .jump
                .as_deref(),
            Some("edge")
        );

        remove_host(&path, "db-1").unwrap();
        let loaded = load(&path).unwrap();
        assert!(loaded.document.to_string().contains("# keep me"));
        assert_eq!(loaded.inventory.hosts.len(), 2);
        assert!(
            loaded
                .inventory
                .hosts
                .iter()
                .all(|host| host.name != "db-1")
        );
    }

    #[test]
    fn invalid_edit_does_not_change_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        fs::write(&path, sample()).unwrap();
        let before = fs::read_to_string(&path).unwrap();
        let error = add_host(
            &path,
            Host {
                name: "web-1".into(),
                address: "10.0.0.9".into(),
                ..Host::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("duplicated"), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        assert!(!dir.path().join("hosts.toml.bak").exists());
    }

    #[test]
    fn a_new_host_keeps_options_tunnels_and_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        let mut ssh_options = std::collections::BTreeMap::new();
        ssh_options.insert("ForwardAgent".to_string(), "yes".to_string());
        add_host(
            &path,
            Host {
                name: "web-1".into(),
                address: "203.0.113.10".into(),
                ssh_options,
                disabled: true,
                tunnels: vec![crate::domain::host::HostTunnel {
                    name: "grafana".into(),
                    spec: "L:3000:localhost:3000".into(),
                }],
                ..Host::default()
            },
        )
        .unwrap();
        let loaded = load(&path).unwrap();
        let host = &loaded.inventory.hosts[0];
        assert!(host.disabled);
        assert_eq!(
            host.ssh_options.get("ForwardAgent").map(String::as_str),
            Some("yes")
        );
        assert_eq!(host.tunnels[0].spec, "L:3000:localhost:3000");
    }
}
