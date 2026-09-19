/// Returns true if the URL path should be blocked because it refers to a
/// sensitive file that must never be served directly.
fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    let (text, prefix) = (text.as_bytes(), prefix.as_bytes());
    text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn ends_with_ignore_case(text: &str, suffix: &str) -> bool {
    let (text, suffix) = (text.as_bytes(), suffix.as_bytes());
    text.len() >= suffix.len() && text[text.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

pub fn is_sensitive_path(path: &str) -> bool {
    // Compared case-insensitively in place: this runs on every request, and
    // lower-casing the whole path first allocated a string each time.

    // Exact file names (anywhere in the path).
    const BLOCKED_NAMES: &[&str] = &[
        ".env",
        ".env.local",
        ".env.production",
        ".env.staging",
        ".htaccess",
        ".htpasswd",
        ".gitignore",
        ".git",
        "composer.json",
        "composer.lock",
        "package.json",
        "package-lock.json",
        "yarn.lock",
        "phpinfo.php", // common info leak
        "info.php",
        "web.config",
        "wp-config.php",
        "config.php",
        "database.php",
        "settings.php",
    ];

    // Blocked extensions.
    const BLOCKED_EXTENSIONS: &[&str] = &[
        ".bak", ".backup", ".old", ".orig", ".save", ".swp", ".swo", ".sql", ".db", ".sqlite",
        ".sqlite3", ".log", ".key", ".pem", ".p12", ".pfx", ".crt", ".cer", ".conf", ".config",
        ".cfg", ".ini", ".yml", ".yaml", ".toml", ".json",
    ];

    // Blocked path prefixes.
    const BLOCKED_PREFIXES: &[&str] = &[
        "/.git/",
        "/.svn/",
        "/.hg/",
        "/vendor/",
        "/node_modules/",
        "/.idea/",
        "/.vscode/",
    ];

    for prefix in BLOCKED_PREFIXES {
        if starts_with_ignore_case(path, prefix)
            || path.eq_ignore_ascii_case(prefix.trim_end_matches('/'))
        {
            return true;
        }
    }

    // Check the final path segment (filename).
    let filename = path.rsplit('/').next().unwrap_or(path);

    for name in BLOCKED_NAMES {
        if filename.eq_ignore_ascii_case(name) {
            return true;
        }
    }

    for ext in BLOCKED_EXTENSIONS {
        if ends_with_ignore_case(filename, ext) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_env_file() {
        assert!(is_sensitive_path("/.env"));
        assert!(is_sensitive_path("/app/.env.local"));
    }

    #[test]
    fn blocks_git_dir() {
        assert!(is_sensitive_path("/.git/config"));
        assert!(is_sensitive_path("/.git/HEAD"));
    }

    #[test]
    fn blocks_composer() {
        assert!(is_sensitive_path("/composer.json"));
        assert!(is_sensitive_path("/composer.lock"));
    }

    #[test]
    fn blocks_backup_files() {
        assert!(is_sensitive_path("/index.php.bak"));
        assert!(is_sensitive_path("/db.sql"));
    }

    #[test]
    fn allows_normal_files() {
        assert!(!is_sensitive_path("/index.html"));
        assert!(!is_sensitive_path("/assets/app.js"));
        assert!(!is_sensitive_path("/images/logo.png"));
        assert!(!is_sensitive_path("/wordpress/wp-login.php"));
        assert!(!is_sensitive_path("/wordpress/xmlrpc.php"));
    }
}
