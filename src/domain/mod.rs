//! Pure data types and logic. No I/O.
//!
//! Host records, selector evaluation, and list filtering live here so the CLI
//! and the TUI share one implementation. Later phases add health, tunnels, and
//! snippets beside them.

pub mod activity;
pub mod filter;
mod health;
pub mod host;
pub mod host_form;
pub mod selector;
mod snippet;
mod tunnel;
