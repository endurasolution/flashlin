use crate::error::{FlashError, Result};
use dashmap::DashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

static ROOT_CACHE: OnceLock<DashMap<String, PathBuf>> = OnceLock::new();

/// Canonical document roots, resolved once per root string.
///
/// `canonicalize` is a `realpath` walk over every path component, and the
/// request path calls `safe_join` several times per request, so resolving it
/// each time dominated the static-file syscall count. Roots come from
/// configuration only, never from request input, so this map stays small.
/// Failures are not cached: a root created after startup still resolves later.
pub fn canonical_root(root: &str) -> PathBuf {
    canonical_root_with_capacity(root, 0)
}

fn canonical_root_with_capacity(root: &str, additional: usize) -> PathBuf {
    let cache = ROOT_CACHE.get_or_init(DashMap::new);
    if let Some(cached) = cache.get(root) {
        // A PathBuf clone reserves only the root's length. Reserve the final
        // URL path here, so safe_join does not immediately reallocate it.
        let mut path = PathBuf::with_capacity(cached.as_os_str().len() + additional);
        path.push(cached.as_path());
        return path;
    }
    let mut path = match Path::new(root).canonicalize() {
        Ok(resolved) => {
            cache.insert(root.to_owned(), resolved.clone());
            resolved
        }
        Err(_) => PathBuf::from(root),
    };
    path.reserve(additional);
    path
}

/// Resolve `url_path` relative to `root`, rejecting any path-traversal attempts.
///
/// This is security-critical: we must never allow `../../etc/passwd` to escape
/// the configured root directory.
pub fn safe_join(root: &str, url_path: &str) -> Result<PathBuf> {
    // Strip leading slash from URL path.
    let rel = url_path.trim_start_matches('/');

    // If the root doesn't exist yet we still allow it (files may not exist —
    // we will 404 rather than panic).
    let mut canonical_root = canonical_root_with_capacity(root, rel.len() + 1);

    // For ordinary relative names, append components to the already-owned
    // root instead of allocating a joined path and rebuilding every ancestor.
    // Prefixes, rooted paths and parent traversal keep the full checks below.
    let relative = Path::new(rel);
    if canonical_root.is_absolute()
        && !rel.contains(':')
        && !rel.contains('\\')
        && !canonical_root
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        && relative
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        for component in relative.components() {
            canonical_root.push(component.as_os_str());
        }
        return Ok(canonical_root);
    }

    // Build the candidate path and normalise it without filesystem access.
    let candidate = canonical_root.join(rel);
    let normalised = normalise(&candidate);

    // After normalisation, the path must still be under the root.
    if !normalised.starts_with(&canonical_root) {
        return Err(FlashError::PathTraversal);
    }

    Ok(normalised)
}

/// Normalise a path by resolving `.` and `..` components without hitting the
/// filesystem (so it works for paths that do not exist yet).
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(seg) => out.push(seg),
        }
    }
    out
}

/// Return true if the path segment looks like a Windows device name that could
/// cause problems on Windows (CON, PRN, AUX, NUL, COM1…COM9, LPT1…LPT9).
pub fn is_windows_device_name(name: &str) -> bool {
    // Most requests have ASCII paths. Compare those in place instead of
    // allocating an uppercase copy for every path segment on every check.
    if name.is_ascii() {
        let base = name.split('.').next().unwrap_or("").as_bytes();
        return match base.len() {
            3 => [b"CON", b"PRN", b"AUX", b"NUL"]
                .iter()
                .any(|device| base.eq_ignore_ascii_case(*device)),
            4 => {
                (b'1'..=b'9').contains(&base[3])
                    && (base[..3].eq_ignore_ascii_case(b"COM")
                        || base[..3].eq_ignore_ascii_case(b"LPT"))
            }
            _ => false,
        };
    }
    // Preserve the existing Unicode case-mapping behavior for other paths.
    let upper = name.to_uppercase();
    let base = upper.split('.').next().unwrap_or("");
    matches!(
        base,
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Check every segment of a URL path for problematic patterns.
pub fn validate_url_path(url_path: &str) -> Result<()> {
    if url_path.contains('\\') || url_path.contains(':') || url_path.chars().any(char::is_control) {
        return Err(FlashError::PathTraversal);
    }
    for segment in url_path.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        // `..` should have been blocked by the percent-decode step already,
        // but we double-check here.
        if segment == ".." {
            return Err(FlashError::PathTraversal);
        }
        // Null bytes are never valid.
        if segment.contains('\0') {
            return Err(FlashError::PathTraversal);
        }
        if is_windows_device_name(segment) {
            return Err(FlashError::AccessDenied);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_paths_normalize_without_changing_root_containment() {
        let root = canonical_root("./public");
        for path in ["assets/app.js", "/assets/app.js", "assets//./app.js"] {
            assert_eq!(
                safe_join("./public", path).unwrap(),
                root.join("assets/app.js")
            );
        }
        assert_eq!(safe_join("./public", "").unwrap(), root);
        assert_eq!(
            safe_join("./public", "assets/../index.html").unwrap(),
            root.join("index.html")
        );
        assert!(safe_join("./public", "assets/../../outside.txt").is_err());
        #[cfg(windows)]
        for path in [
            r"C:\outside.txt",
            r"\\server\share\outside.txt",
            r"\outside.txt",
            "assets/C:outside.txt",
        ] {
            assert!(safe_join("./public", path).is_err(), "{path}");
        }
    }

    #[test]
    fn device_names_keep_case_extension_and_segment_boundaries() {
        for name in ["con", "PrN.txt", "AUX", "nul.tar.gz", "CoM1", "lpt9.dat"] {
            assert!(is_windows_device_name(name), "{name}");
        }
        for name in [
            "console",
            "com0",
            "com10",
            "lpt0",
            "nulled",
            "index.html",
            "cön",
        ] {
            assert!(!is_windows_device_name(name), "{name}");
        }
        assert!(validate_url_path("/assets/CoM9.txt").is_err());
        assert!(validate_url_path("/assets/company.txt").is_ok());
    }

    #[test]
    fn blocks_parent_traversal() {
        let result = safe_join("./public", "../../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn allows_normal_path() {
        // This test only checks the logic, not filesystem presence.
        let result = safe_join("/tmp", "index.html");
        // The path should not be an error (even if the file doesn't exist).
        // On Windows /tmp won't canonicalize; we simply ensure no traversal error.
        match result {
            Err(FlashError::PathTraversal) => panic!("Should not be traversal error"),
            _ => {}
        }
    }

    #[test]
    fn validates_null_byte() {
        assert!(validate_url_path("/foo\0bar").is_err());
    }

    #[test]
    fn validates_parent_segment() {
        assert!(validate_url_path("/foo/../etc").is_err());
    }
}
