//! `vcon check`: print every validation problem and fail when any exist.
//!
//! A file that does not parse is also a failure. The message includes the
//! path and line from [`crate::error::Error::Parse`]. An empty inventory is valid.

use crate::error::Error;
use crate::services::inventory::InventoryBundle;
use std::process::ExitCode;

/// Message and exit code for a parsed inventory.
pub struct CheckResult {
    /// Either "Inventory is valid…" or the full problem list.
    pub text: String,
    /// `SUCCESS` when `problems` is empty, otherwise exit code 1.
    pub code: ExitCode,
}

/// Format validation problems. Does not load the file; the caller does that.
pub fn run(bundle: &InventoryBundle) -> CheckResult {
    if bundle.problems.is_empty() {
        CheckResult {
            text: format!(
                "Inventory is valid ({} hosts)",
                bundle.inventory.hosts.len()
            ),
            code: ExitCode::SUCCESS,
        }
    } else {
        let mut lines = vec![format!(
            "Found {} validation problems:",
            bundle.problems.len()
        )];
        for problem in &bundle.problems {
            lines.push(format!("  - {problem}"));
        }
        CheckResult {
            text: lines.join("\n"),
            code: ExitCode::from(1),
        }
    }
}

/// Format a load or parse failure as a failing check result.
pub fn load_error(error: &Error) -> CheckResult {
    CheckResult {
        text: error.to_string(),
        code: ExitCode::from(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::inventory;

    #[test]
    fn reports_every_problem_and_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        std::fs::write(
            &path,
            "[[hosts]]\nname = \"Web-1\"\naddress = \"\"\njump = \"missing\"\n",
        )
        .unwrap();
        let bundle = inventory::load_hosts(&path, "ada").unwrap();
        let result = run(&bundle);
        assert_eq!(result.code, ExitCode::from(1));
        assert!(result.text.contains("invalid characters"));
        assert!(result.text.contains("address is empty"));
        assert!(result.text.contains("does not exist"));
        assert!(result.text.contains("line "));
    }

    #[test]
    fn valid_inventory_exits_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        std::fs::write(
            &path,
            "[[hosts]]\nname = \"web-1\"\naddress = \"10.0.0.1\"\n",
        )
        .unwrap();
        let bundle = inventory::load_hosts(&path, "ada").unwrap();
        let result = run(&bundle);
        assert_eq!(result.code, ExitCode::SUCCESS);
        assert!(result.text.contains("valid"));
    }
}
