//! Remote path helpers. Always `/`-separated; never use [`std::path::Path`] for remote strings.

/// Join `parent` and `name` with a single `/`.
pub fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() || parent == "/" {
        if name.starts_with('/') {
            return name.to_string();
        }
        return format!("/{name}");
    }
    let parent = parent.trim_end_matches('/');
    if name.is_empty() || name == "." {
        return parent.to_string();
    }
    if name == ".." {
        return parent_of(parent).unwrap_or_else(|| "/".to_string());
    }
    format!("{parent}/{name}")
}

/// Parent directory, or `None` at `/`.
pub fn parent_of(path: &str) -> Option<String> {
    let path = normalize(path);
    if path == "/" {
        return None;
    }
    match path.rsplit_once('/') {
        Some(("", _)) => Some("/".to_string()),
        Some((parent, _)) => Some(parent.to_string()),
        None => Some("/".to_string()),
    }
}

/// Final component of `path`.
pub fn file_name(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    if path.is_empty() || path == "/" {
        return "/";
    }
    path.rsplit('/').next().unwrap_or(path)
}

/// Collapse repeated slashes and resolve `.` segments. Does not resolve `..`
/// beyond the string (use [`join`] for that).
pub fn normalize(path: &str) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    let absolute = path.starts_with('/');
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return if absolute {
            "/".to_string()
        } else {
            ".".to_string()
        };
    }
    let body = parts.join("/");
    if absolute { format!("/{body}") } else { body }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_and_parents() {
        assert_eq!(join("/etc", "nginx"), "/etc/nginx");
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(join("/etc/nginx", ".."), "/etc");
        assert_eq!(parent_of("/etc/nginx.conf").as_deref(), Some("/etc"));
        assert_eq!(parent_of("/").as_deref(), None);
        assert_eq!(file_name("/etc/nginx.conf"), "nginx.conf");
        assert_eq!(normalize("/a//b/./c/../d"), "/a/b/d");
    }
}
