//! Errors that cross the CLI, the loader, and the TUI.
//!
//! Parse failures include the file path and a 1-based line so `vcon check`
//! can print them directly.

use std::path::PathBuf;

/// A failure the user can be shown without a stack trace.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// TOML or config failed to parse. `line` is 1-based.
    #[error("{path}:{line}: {message}")]
    Parse {
        /// File that failed.
        path: PathBuf,
        /// 1-based line, or 1 when the parser gave no span.
        line: usize,
        /// Parser message.
        message: String,
    },
    /// The file could not be read.
    #[error("failed to read {path}: {source}")]
    Read {
        /// File that could not be read.
        path: PathBuf,
        /// OS error.
        #[source]
        source: std::io::Error,
    },
    /// The file could not be written. `vcon init` uses this when the directory cannot be created.
    #[error("failed to write {path}: {source}")]
    Write {
        /// Path that could not be written.
        path: PathBuf,
        /// OS error.
        #[source]
        source: std::io::Error,
    },
    /// A problem that is already a complete sentence, such as "hosts.toml already exists".
    #[error("{0}")]
    Message(String),
}

impl Error {
    /// Build a parse error, converting a byte span into a 1-based line.
    pub fn parse(
        path: impl Into<PathBuf>,
        text: &str,
        span: Option<std::ops::Range<usize>>,
        message: impl Into<String>,
    ) -> Self {
        let message = message.into();
        let line = span
            .map(|range| line_number(text, range.start))
            .unwrap_or(1);
        Self::Parse {
            path: path.into(),
            line,
            message,
        }
    }
}

/// 1-based line containing `byte_offset`. An offset past the end uses the last line.
pub fn line_number(text: &str, byte_offset: usize) -> usize {
    let end = byte_offset.min(text.len());
    text[..end].bytes().filter(|byte| *byte == b'\n').count() + 1
}
