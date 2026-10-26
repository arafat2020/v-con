use crate::domain::host::{Inventory, validate_inventory};
use crate::domain::selector::{evaluate_selector, parse_selector};
use std::fs::{self, create_dir_all};
use std::path::Path;

pub fn handle_init(config_dir: &Path) -> Result<(), String> {
    create_dir_all(config_dir).map_err(|e| e.to_string())?;
    let host_path = config_dir.join("hosts.toml");

    if host_path.exists() {
        return Err(format!("File already exists: {}", host_path.display()));
    }

    let template = r#"# vcon inventory template
[defaults]
user = "root"
port = 22

[[hosts]]
name = "web-1"
address = "192.168.1.10"
tags = ["web", "prod"]
"#;

    fs::write(&host_path, template).map_err(|e| e.to_string())?;
    println!("Created Inventory template at {}", host_path.display());
    Ok(())
}

pub fn handle_check(inventory: &Inventory) -> Result<(), String>{
    let errors = validate_inventory(inventory);

    if errors.is_empty() {
        println!("✔ Inventory is valid ({} hosts)", inventory.hosts.len());
        Ok(())
    } else {
        eprintln!("Found {} validation problems:", errors.len());
        for err in &errors {
            eprintln!("  – {}", err);
        }
        Err("Inventory validation failed".to_string())
    }
}