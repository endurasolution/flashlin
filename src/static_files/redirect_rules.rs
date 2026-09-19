use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;

use super::path_security::{canonical_root, safe_join, validate_url_path};
use crate::error::{FlashError, Result};

static RULE_FILE_CACHE: OnceLock<DashMap<PathBuf, (Instant, Option<Arc<String>>)>> =
    OnceLock::new();

const RULE_CACHE_TTL: Duration = Duration::from_secs(60);

/// Read a compatibility rule file, caching *absence* as well as content.
///
/// Every request consults `.htaccess`, `nginx.conf` and `redirects.conf` in each
/// directory. Almost all deployments have none of them, so without negative
/// caching this cost three failed `open` syscalls per request. Mirrors the
/// existing `.flashweb` cache behaviour, including its 60-second TTL.
fn read_rule_file(path: &Path) -> Result<Option<Arc<String>>> {
    let cache = RULE_FILE_CACHE.get_or_init(DashMap::new);
    if let Some(entry) = cache.get(path) {
        let (created, cached) = &*entry;
        if created.elapsed() < RULE_CACHE_TTL {
            return Ok(cached.clone());
        }
    }

    let content = match std::fs::read_to_string(path) {
        Ok(content) => Some(Arc::new(content)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        // Permission errors stay uncached so a corrected mode takes effect at once.
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(FlashError::AccessDenied);
        }
        Err(e) => return Err(FlashError::Io(e)),
    };

    if cache.len() >= 4096 && !cache.contains_key(path) {
        cache.retain(|_, (created, _)| created.elapsed() < RULE_CACHE_TTL);
        if cache.len() >= 4096 {
            return Ok(content);
        }
    }
    cache.insert(path.to_path_buf(), (Instant::now(), content.clone()));
    Ok(content)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectRule {
    pub location: String,
    pub permanent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewriteAction {
    Redirect(RedirectRule),
    Rewrite(String),
}

#[derive(Debug, Clone)]
struct RewriteCond {
    variable: String,
    pattern: String,
    negate: bool,
}

pub fn find_rewrite_action(url_path: &str, root: &str) -> Result<Option<RewriteAction>> {
    validate_url_path(url_path)?;
    let fs_path = safe_join(root, url_path)?;
    let root_path = canonical_root(root);

    // A path already known to be a regular file skips the stat; rules are then
    // collected from its parent directory, exactly as the stat would decide.
    let known_file = super::file_server::is_verified_file(&fs_path);
    let mut current_dir = if !known_file && fs_path.is_dir() {
        fs_path
    } else {
        fs_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root_path.clone())
    };

    if !current_dir.starts_with(&root_path) {
        current_dir.clone_from(&root_path);
    }

    // Walk toward the root in place. Rules use nearest-directory precedence,
    // so constructing and then reversing a vector of every ancestor is wasted.
    loop {
        if let Some(action) = parse_htaccess(&current_dir.join(".htaccess"), url_path)? {
            return Ok(Some(action));
        }
        if let Some(rule) = parse_nginx_redirects(&current_dir.join("nginx.conf"), url_path)? {
            return Ok(Some(RewriteAction::Redirect(rule)));
        }
        if let Some(rule) = parse_nginx_redirects(&current_dir.join("redirects.conf"), url_path)? {
            return Ok(Some(RewriteAction::Redirect(rule)));
        }
        if current_dir == root_path || !current_dir.pop() {
            break;
        }
    }

    Ok(None)
}

fn parse_htaccess(path: &Path, url_path: &str) -> Result<Option<RewriteAction>> {
    let Some(content) = read_rule_file(path)? else {
        return Ok(None);
    };

    let mut conditions = Vec::new();
    for line in content.lines().map(clean_line).filter(|l| !l.is_empty()) {
        let parts = line.split_whitespace().collect::<Vec<_>>();
        if parts[0].eq_ignore_ascii_case("rewriteengine") {
            continue;
        }

        if parts.len() >= 3 && parts[0].eq_ignore_ascii_case("rewritecond") {
            conditions.push(parse_rewrite_cond(parts[1], parts[2]));
            continue;
        }

        if parts.len() >= 3 && parts[0].eq_ignore_ascii_case("redirect") {
            let has_status = parts[1].parse::<u16>().is_ok();
            let status = if has_status {
                parts[1].parse::<u16>().unwrap_or(302)
            } else {
                302
            };
            let source_idx = if has_status { 2 } else { 1 };
            let target_idx = source_idx + 1;
            if parts.get(source_idx) == Some(&url_path) {
                return Ok(Some(RewriteAction::Redirect(RedirectRule {
                    location: parts.get(target_idx).unwrap_or(&"/").to_string(),
                    permanent: status == 301,
                })));
            }
        }

        if parts.len() >= 3 && parts[0].eq_ignore_ascii_case("redirectpermanent") {
            if parts[1] == url_path {
                return Ok(Some(RewriteAction::Redirect(RedirectRule {
                    location: parts[2].to_string(),
                    permanent: true,
                })));
            }
        }

        if parts.len() >= 3 && parts[0].eq_ignore_ascii_case("redirecttemp") {
            if parts[1] == url_path {
                return Ok(Some(RewriteAction::Redirect(RedirectRule {
                    location: parts[2].to_string(),
                    permanent: false,
                })));
            }
        }

        if parts.len() >= 3 && parts[0].eq_ignore_ascii_case("rewriterule") {
            let conds_match = conditions
                .iter()
                .all(|cond| rewrite_cond_matches(cond, url_path));
            conditions.clear();
            if conds_match {
                if let Some(rewritten) = rewrite_substitution(parts[1], parts[2], url_path) {
                    let flags = parts.get(3).copied().unwrap_or("");
                    if rewrite_flags_redirect(flags) {
                        return Ok(Some(RewriteAction::Redirect(RedirectRule {
                            location: rewritten,
                            permanent: rewrite_flags_permanent(flags),
                        })));
                    }
                    return Ok(Some(RewriteAction::Rewrite(rewritten)));
                }
            }
        }
    }

    Ok(None)
}

fn parse_nginx_redirects(path: &Path, url_path: &str) -> Result<Option<RedirectRule>> {
    let Some(content) = read_rule_file(path)? else {
        return Ok(None);
    };

    let mut current_location: Option<String> = None;
    for line in content.lines().map(clean_line).filter(|l| !l.is_empty()) {
        let parts = line.split_whitespace().collect::<Vec<_>>();
        if parts.len() >= 3 && parts[0] == "location" {
            let path_idx = if parts.get(1) == Some(&"=") { 2 } else { 1 };
            current_location = parts
                .get(path_idx)
                .map(|path| path.trim_end_matches('{').to_string());
            continue;
        }

        if parts.len() >= 4 && parts[0] == "rewrite" {
            if rewrite_pattern_matches(parts[1], url_path) {
                let permanent = parts
                    .get(3)
                    .map(|flag| flag.trim_end_matches(';') == "permanent")
                    .unwrap_or(false);
                return Ok(Some(RedirectRule {
                    location: parts[2].trim_end_matches(';').to_string(),
                    permanent,
                }));
            }
        }

        if parts.len() >= 3 && parts[0] == "return" {
            let status = parts[1].parse::<u16>().unwrap_or(302);
            let location = parts[2].trim_end_matches(';');
            if current_location.as_deref() == Some(url_path) || current_location.is_none() {
                return Ok(Some(RedirectRule {
                    location: location.to_string(),
                    permanent: status == 301,
                }));
            }
        }

        if line.contains('}') {
            current_location = None;
        }
    }

    Ok(None)
}

fn clean_line(line: &str) -> &str {
    line.split('#').next().unwrap_or("").trim()
}

fn rewrite_pattern_matches(pattern: &str, url_path: &str) -> bool {
    let path = url_path.trim_start_matches('/');
    let pattern = pattern
        .trim_start_matches('^')
        .trim_end_matches('$')
        .trim_start_matches('/');
    pattern == path || pattern == url_path
}

fn parse_rewrite_cond(variable: &str, pattern: &str) -> RewriteCond {
    let pattern = pattern.trim();
    RewriteCond {
        variable: variable
            .trim_start_matches("%{")
            .trim_end_matches('}')
            .to_ascii_uppercase(),
        pattern: pattern.trim_start_matches('!').to_string(),
        negate: pattern.starts_with('!'),
    }
}

fn rewrite_cond_matches(cond: &RewriteCond, url_path: &str) -> bool {
    let value = match cond.variable.as_str() {
        "REQUEST_URI" => url_path,
        _ => "",
    };
    let matched = simple_pattern_matches(&cond.pattern, value);
    if cond.negate {
        !matched
    } else {
        matched
    }
}

fn simple_pattern_matches(pattern: &str, value: &str) -> bool {
    if let Some(prefix) = pattern.strip_prefix('^') {
        return value.starts_with(prefix.trim_end_matches('$'));
    }
    value == pattern
}

fn rewrite_substitution(pattern: &str, substitution: &str, url_path: &str) -> Option<String> {
    if !rewrite_pattern_matches(pattern, url_path) && !pattern_supports_whole_path(pattern) {
        return None;
    }

    let path = url_path.trim_start_matches('/');
    let rewritten = if pattern_supports_whole_path(pattern) {
        substitution.replace("$1", path)
    } else {
        substitution.to_string()
    };

    Some(if rewritten.starts_with('/') {
        rewritten
    } else {
        format!("/{rewritten}")
    })
}

fn pattern_supports_whole_path(pattern: &str) -> bool {
    matches!(pattern, "^(.*)$" | "(.*)" | "^(.+)$" | "(.+)")
}

fn rewrite_flags_redirect(flags: &str) -> bool {
    flags
        .trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .any(|flag| {
            flag.trim().eq_ignore_ascii_case("r")
                || flag.trim().to_ascii_lowercase().starts_with("r=")
        })
}

fn rewrite_flags_permanent(flags: &str) -> bool {
    flags
        .trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .any(|flag| {
            let flag = flag.trim().to_ascii_lowercase();
            flag == "permanent" || flag == "r=301"
        })
}

#[cfg(test)]
mod tests {
    use super::{
        parse_rewrite_cond, rewrite_cond_matches, rewrite_pattern_matches, rewrite_substitution,
    };

    #[test]
    fn rewrite_patterns_match_common_forms() {
        assert!(rewrite_pattern_matches("^old$", "/old"));
        assert!(rewrite_pattern_matches("^/old$", "/old"));
        assert!(!rewrite_pattern_matches("^other$", "/old"));
    }

    #[test]
    fn rewrite_cond_supports_negative_request_uri_prefix() {
        let cond = parse_rewrite_cond("%{REQUEST_URI}", "!^/real/");
        assert!(rewrite_cond_matches(&cond, "/about"));
        assert!(!rewrite_cond_matches(&cond, "/real/style.css"));
    }

    #[test]
    fn rewrite_rule_substitutes_whole_path_capture() {
        assert_eq!(
            rewrite_substitution("^(.*)$", "/real/$1", "/css/app.css"),
            Some("/real/css/app.css".to_string())
        );
        assert_eq!(
            rewrite_substitution("^(.*)$", "/real/$1", "/"),
            Some("/real/".to_string())
        );
    }
}
