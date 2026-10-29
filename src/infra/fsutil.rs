//! Secure directory and file modes for config and state.
//!
//! Directories are created `0700`. Sensitive files are set to `0600` after
//! write. Non-unix builds create with default modes.

use std::fs;
use std::io;
use std::path::Path;

/// Directory mode for config, state, cache, and control sockets.
pub const DIR_MODE: u32 = 0o700;
/// File mode for hosts, config, state, audit, and snippets.
pub const FILE_MODE: u32 = 0o600;

/// Create `path` and parents with [`DIR_MODE`] on unix.
pub fn create_dir_secure(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// Set `path` permissions to `mode` on unix. No-op elsewhere.
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// Create parent dirs securely, write `text`, then set [`FILE_MODE`].
pub fn write_secure(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        create_dir_secure(parent)?;
    }
    fs::write(path, text)?;
    set_mode(path, FILE_MODE)?;
    Ok(())
}

/// Whether `path` is writable by creating a temporary probe file.
pub fn dir_is_writable(path: &Path) -> bool {
    if create_dir_secure(path).is_err() {
        return false;
    }
    let probe = path.join(format!(".vcon-write-probe-{}", std::process::id()));
    match fs::write(&probe, b"") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn write_secure_sets_file_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.toml");
        write_secure(&path, "x = 1\n").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, FILE_MODE);
        let dir_mode = fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        // tempfile may already exist with a looser mode; only assert our nested dirs.
        let nested = dir.path().join("nested");
        create_dir_secure(&nested).unwrap();
        let nested_mode = fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
        assert_eq!(nested_mode, DIR_MODE);
        let _ = dir_mode;
    }
}
