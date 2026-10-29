//! Pure data types and logic. No I/O.
//!
//! Host records, selector evaluation, list filtering, and health parsing live
//! here so the CLI and the TUI share one implementation. Later phases add
//! tunnels and snippets beside them.

pub mod activity;
pub mod filter;
pub mod health;
pub mod host;
pub mod host_form;
pub mod selector;
pub mod snippet;
pub mod tunnel;
