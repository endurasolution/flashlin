#[cfg(test)]
mod tests {
    use crate::security::sensitive_files::is_sensitive_path;
    use crate::static_files::path_security::{safe_join, validate_url_path};

    // ─── Sensitive file detection ─────────────────────────────────────────────

    #[test]
    fn blocks_dotenv() {
        assert!(is_sensitive_path("/.env"));
        assert!(is_sensitive_path("/app/.env.production"));
    }

    #[test]
    fn blocks_git_internals() {
        assert!(is_sensitive_path("/.git/config"));
        assert!(is_sensitive_path("/.git/HEAD"));
    }

    #[test]
    fn blocks_composer_files() {
        assert!(is_sensitive_path("/composer.json"));
        assert!(is_sensitive_path("/composer.lock"));
    }

    #[test]
    fn blocks_sql_dumps() {
        assert!(is_sensitive_path("/dump.sql"));
        assert!(is_sensitive_path("/schema.db"));
        assert!(is_sensitive_path("/app.sqlite3"));
    }

    #[test]
    fn blocks_backup_extensions() {
        assert!(is_sensitive_path("/index.php.bak"));
        assert!(is_sensitive_path("/config.old"));
    }

    #[test]
    fn allows_normal_files() {
        assert!(!is_sensitive_path("/index.html"));
        assert!(!is_sensitive_path("/assets/app.js"));
        assert!(!is_sensitive_path("/images/photo.jpg"));
        assert!(!is_sensitive_path("/about.html"));
    }

    // ─── Path traversal prevention ────────────────────────────────────────────

    #[test]
    fn blocks_parent_traversal() {
        let r = safe_join("./public", "../../etc/passwd");
        assert!(r.is_err(), "Path traversal must be rejected");
    }

    #[test]
    fn blocks_null_byte() {
        assert!(validate_url_path("/foo\0bar").is_err());
    }

    #[test]
    fn blocks_dotdot_segment() {
        assert!(validate_url_path("/foo/../etc").is_err());
    }

    #[test]
    fn allows_valid_paths() {
        assert!(validate_url_path("/index.html").is_ok());
        assert!(validate_url_path("/assets/style.css").is_ok());
        assert!(validate_url_path("/api/v1/users").is_ok());
    }

    // ─── Rate limiting ─────────────────────────────────────────────────────────

    #[test]
    fn rate_limiter_blocks_after_limit() {
        use crate::security::rate_limit::RateLimiter;

        // capacity=3, refill=0 (no refill during test), burst=0
        let limiter = RateLimiter::new(3, 0.0, 0);

        assert!(limiter.check("1.2.3.4"));
        assert!(limiter.check("1.2.3.4"));
        assert!(limiter.check("1.2.3.4"));
        assert!(!limiter.check("1.2.3.4"), "4th request must be blocked");
    }

    #[test]
    fn rate_limiter_separate_keys() {
        use crate::security::rate_limit::RateLimiter;

        let limiter = RateLimiter::new(1, 0.0, 0);

        assert!(limiter.check("1.0.0.1"));
        assert!(!limiter.check("1.0.0.1")); // blocked

        // Different IP should still pass.
        assert!(
            limiter.check("1.0.0.2"),
            "Different IP must not be rate-limited"
        );
    }
}
