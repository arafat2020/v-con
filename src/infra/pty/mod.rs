//! Pseudo-terminal helpers for embedded SSH sessions.
//!
//! Key encoding is pure and tested here. Spawning and parsing live in
//! [`crate::services::sessions`].

mod keys;

pub use keys::{KeyModes, encode_key, encode_paste};
