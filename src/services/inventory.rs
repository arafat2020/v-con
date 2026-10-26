//! Load the inventory and resolve it for both the CLI and the TUI.
//!
//! The service owns the use case. Callers pass a path and the local user name
//! and get back effective hosts plus every validation problem. Writing hosts
//! is phase 3; the [`toml_edit`] document is kept so that write can preserve comments.

use crate::config::{self, Config};
use crate::domain::host::{EffectiveHost, Inventory, Problem, validate_inventory};
use crate::error::Error;
use crate::infra::inventory_store::{self, LoadedInventory};
use crate::paths::Paths;
use std::path::Path;
use toml_edit::DocumentMut;

/// Resolved inventory ready to list, check, or browse.
pub struct InventoryBundle {
    /// Parsed file, including hosts that failed validation.
    pub inventory: Inventory,
    /// Hosts after [`crate::domain::host::resolve`]. Disabled hosts are included.
    pub hosts: Vec<EffectiveHost>,
    /// Every validation problem, with line numbers when the source had them.
    pub problems: Vec<Problem>,
    /// Original document. Phase 3 edits this instead of reserializing.
    #[allow(dead_code)]
    pub document: DocumentMut,
}

/// Load `hosts.toml` and `config.toml` from `paths`.
pub fn load(paths: &Paths, local_user: &str) -> Result<(InventoryBundle, Config), Error> {
    let loaded = inventory_store::load(&paths.hosts_file())?;
    let config = config::load(&paths.config_file())?;
    Ok((bundle(loaded, local_user), config))
}

/// Resolve `loaded` with `local_user` and attach source lines to any problems.
pub fn bundle(loaded: LoadedInventory, local_user: &str) -> InventoryBundle {
    let mut problems = validate_inventory(&loaded.inventory);
    inventory_store::attach_lines(&mut problems, &loaded);
    let hosts = loaded
        .inventory
        .hosts
        .iter()
        .map(|host| crate::domain::host::resolve(host, &loaded.inventory.defaults, local_user))
        .collect();
    InventoryBundle {
        inventory: loaded.inventory,
        hosts,
        problems,
        document: loaded.document,
    }
}

/// Load one `hosts.toml`. Used by `vcon ls` and `vcon check`.
pub fn load_hosts(path: &Path, local_user: &str) -> Result<InventoryBundle, Error> {
    let loaded = inventory_store::load(path)?;
    Ok(bundle(loaded, local_user))
}
