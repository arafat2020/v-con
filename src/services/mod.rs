//! Use cases. Orchestrates domain logic and infrastructure.
//!
//! These modules do not know about ratatui.

mod audit;
pub mod connect;
pub mod exec;
pub mod files;
pub mod health;
pub mod inventory;
mod sessions;
mod snippets;
mod tunnels;
