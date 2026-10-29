//! Embedded health script run over ssh.

/// POSIX `sh` body that prints `key=value` lines. Missing tools omit keys.
pub const SCRIPT: &str = include_str!("script.sh");
