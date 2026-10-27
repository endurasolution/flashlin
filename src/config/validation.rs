use super::parser::{parse_duration_secs, parse_size, ServerConfig};
use crate::error::{FlashError, Result};
use std::path::Path;

pub fn validate(cfg: &ServerConfig) -> Result<()> {
    validate_server(cfg)?;
    validate_http3(cfg)?;
    validate_tls(cfg)?;
    validate_security(cfg)?;
    if let Some(cache) = &cfg.cache {
        if parse_duration_secs(&cache.revalidate_after).is_none() {
            return Err(FlashError::Config(
                "Invalid cache.revalidate_after duration".into(),
            ));
        }
    }
    validate_php(cfg)?;
    validate_routes(cfg)?;
    validate_apps(cfg)?;
    Ok(())
}

fn validate_http3(cfg: &ServerConfig) -> Result<()> {
    if let Some(h3) = cfg.http3.as_ref().filter(|h3| h3.enabled) {
        if !cfg!(feature = "http3") {
            return Err(FlashError::Config(
                "HTTP/3 requires --features http3".into(),
            ));
        }
        if !cfg.tls.as_ref().is_some_and(|t| t.enabled) {
            return Err(FlashError::Config(
                "HTTP/3 requires enabled TLS (manual certificates or ACME)".into(),
            ));
        }
        if h3.port == Some(0) || !(1..=128).contains(&h3.max_concurrent_streams) {
            return Err(FlashError::Config(
                "HTTP/3 port must be nonzero and max_concurrent_streams must be 1..=128".into(),
            ));
        }
    }
    Ok(())
}

fn validate_server(cfg: &ServerConfig) -> Result<()> {
    if cfg.server.port == 0 {
        return Err(FlashError::Config("server.port cannot be 0".into()));
    }
    if cfg
        .server
        .control_token
        .as_deref()
        .is_some_and(|t| t.len() < 16 || !t.bytes().all(|b| b.is_ascii_graphic()))
    {
        return Err(FlashError::Config(
            "server.control_token must be at least 16 printable characters without spaces".into(),
        ));
    }
    if let Some(workers) = cfg.server.workers {
        if workers == 0 {
            return Err(FlashError::Config("server.workers cannot be 0".into()));
        }
    }
    for (name, value) in [
        ("read_timeout", &cfg.server.read_timeout),
        ("write_timeout", &cfg.server.write_timeout),
        ("keep_alive_timeout", &cfg.server.keep_alive_timeout),
    ] {
        if !parse_duration_secs(value).is_some_and(|s| s > 0) {
            return Err(FlashError::Config(format!("Invalid {name}: '{value}'")));
        }
    }
    Ok(())
}

fn validate_tls(cfg: &ServerConfig) -> Result<()> {
    if let Some(tls) = &cfg.tls {
        if tls.enabled {
            if let Some(_acme) = &tls.acme {
                #[cfg(not(feature = "acme"))]
                return Err(FlashError::Config(
                    "Automatic certificates require --features acme".into(),
                ));
                #[cfg(feature = "acme")]
                {
                    let acme = _acme;
                    if tls.cert.is_some() || tls.key.is_some() {
                        return Err(FlashError::Config(
                            "Use tls.acme or manual cert/key, not both".into(),
                        ));
                    }
                    if !acme.accept_terms
                        || acme.domains.is_empty()
                        || acme.domains.len() > 100
                        || acme.domains.iter().any(|d| !valid_acme_domain(d))
                        || !acme.email.contains('@')
                        || acme.email.chars().any(char::is_whitespace)
                        || acme.cache_dir.trim().is_empty()
                    {
                        return Err(FlashError::Config("ACME requires valid DNS domains, email, a private cache_dir and explicit accept_terms = true".into()));
                    }
                    validate_acme_cache(cfg, &acme.cache_dir)?;
                    return Ok(());
                }
            }
            let cert = tls.cert.as_deref().unwrap_or("");
            let key = tls.key.as_deref().unwrap_or("");
            if cert.is_empty() || key.is_empty() {
                return Err(FlashError::Config(
                    "tls.cert and tls.key are required when TLS is enabled".into(),
                ));
            }
            if !Path::new(cert).exists() {
                return Err(FlashError::Config(format!(
                    "TLS cert file not found: {cert}"
                )));
            }
            if !Path::new(key).exists() {
                return Err(FlashError::Config(format!("TLS key file not found: {key}")));
            }
        }
    }
    Ok(())
}

fn validate_security(cfg: &ServerConfig) -> Result<()> {
    if let Some(sec) = &cfg.security {
        if let Some(rate) = &sec.rate_limit {
            if !super::parser::parse_rate_limit(&rate.limit).is_some_and(|(count, window)| {
                count > 0
                    && window > 0
                    && count
                        .checked_add(rate.burst.unwrap_or(count / 5 + 1))
                        .is_some()
            }) {
                return Err(FlashError::Config("Invalid rate limit or burst".into()));
            }
        }
        if parse_size(&sec.max_body_size).is_none() {
            return Err(FlashError::Config(format!(
                "Invalid max_body_size: '{}'",
                sec.max_body_size
            )));
        }
        if parse_size(&sec.max_header_size).is_none() {
            return Err(FlashError::Config(format!(
                "Invalid max_header_size: '{}'",
                sec.max_header_size
            )));
        }
    }
    Ok(())
}

#[cfg(feature = "acme")]
fn valid_acme_domain(domain: &str) -> bool {
    domain.len() <= 253
        && domain.contains('.')
        && domain.parse::<std::net::IpAddr>().is_err()
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Resolve existing ancestors as well as .. components before comparing private/public paths.
#[cfg(feature = "acme")]
fn resolved_path(path: &Path) -> std::io::Result<std::path::PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut resolved = std::path::PathBuf::new();
    for component in absolute.components() {
        if component == std::path::Component::ParentDir {
            resolved.pop();
        } else if component != std::path::Component::CurDir {
            resolved.push(component);
        }
        if resolved.exists() {
            resolved = resolved.canonicalize()?;
        }
    }
    Ok(resolved)
}

#[cfg(feature = "acme")]
pub(crate) fn validate_acme_cache(cfg: &ServerConfig, cache: &str) -> Result<()> {
    let private = resolved_path(Path::new(cache)).map_err(|e| FlashError::Config(e.to_string()))?;
    let roots = cfg
        .server
        .root
        .iter()
        .map(Path::new)
        .chain(
            cfg.routes
                .iter()
                .flatten()
                .filter_map(|r| r.root.as_deref())
                .map(Path::new),
        )
        .chain(cfg.static_dirs.iter().flatten().map(|r| r.root.as_path()));
    for root in roots {
        let public = resolved_path(root).map_err(|e| FlashError::Config(e.to_string()))?;
        if private.starts_with(public) {
            return Err(FlashError::Config(
                "ACME cache_dir must be outside every public document root".into(),
            ));
        }
    }
    Ok(())
}

fn validate_php(cfg: &ServerConfig) -> Result<()> {
    if let Some(php) = &cfg.php {
        if php.enabled {
            for handler in &php.handlers {
                if handler.name.is_empty() {
                    return Err(FlashError::Config(
                        "PHP handler name cannot be empty".into(),
                    ));
                }
                let has_socket = handler.socket.is_some();
                let has_address = handler.address.is_some();
                if !has_socket && !has_address {
                    return Err(FlashError::Config(format!(
                        "PHP handler '{}': must specify either 'socket' or 'address'",
                        handler.name
                    )));
                }
                if parse_duration_secs(&handler.timeout).is_none() {
                    return Err(FlashError::Config(format!(
                        "PHP handler '{}': invalid timeout '{}'",
                        handler.name, handler.timeout
                    )));
                }
                if parse_size(&handler.max_body_size).is_none() {
                    return Err(FlashError::Config(format!(
                        "PHP handler '{}': invalid max_body_size '{}'",
                        handler.name, handler.max_body_size
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_routes(cfg: &ServerConfig) -> Result<()> {
    if let Some(routes) = &cfg.routes {
        for route in routes {
            for method in route.methods.iter().flatten() {
                if hyper::Method::from_bytes(method.as_bytes()).is_err() {
                    return Err(FlashError::Config(format!(
                        "Route '{}': invalid HTTP method",
                        route.path
                    )));
                }
            }
            let has_root = route.root.is_some();
            let has_proxy = route.proxy_target().is_some();
            if route.proxy_pass.is_some() && !route.proxy_upstreams.is_empty() {
                return Err(FlashError::Config(
                    "Use proxy_pass or proxy_upstreams, not both".into(),
                ));
            }
            for upstream in &route.proxy_upstreams {
                crate::proxy::reverse_proxy::validate_upstream(upstream).map_err(|reason| {
                    FlashError::Config(format!("Route '{}': {reason}", route.path))
                })?;
            }
            if let Some(upstream) = &route.proxy_pass {
                crate::proxy::reverse_proxy::validate_upstream(upstream).map_err(|reason| {
                    FlashError::Config(format!("Route '{}': {reason}", route.path))
                })?;
            }
            if let Some(app) = &route.app {
                if !cfg.apps.iter().any(|a| &a.name == app) {
                    return Err(FlashError::Config(format!(
                        "Route '{}': app '{app}' is not defined in [[apps]]",
                        route.path
                    )));
                }
            }
            if has_root && has_proxy {
                return Err(FlashError::Config(format!(
                    "Route '{}': cannot set both 'root' and 'proxy_pass'",
                    route.path
                )));
            }
            if has_root {
                let root = route.root.as_deref().unwrap();
                if !Path::new(root).exists() {
                    tracing::warn!(
                        "Route '{}': root directory '{}' does not exist",
                        route.path,
                        root
                    );
                }
            }
        }
    }
    Ok(())
}

fn validate_apps(cfg: &ServerConfig) -> Result<()> {
    let mut claimed: Vec<(u16, u16, &str)> = Vec::new();
    for app in &cfg.apps {
        let err = |msg: String| Err(FlashError::Config(format!("App '{}': {msg}", app.name)));
        if app.name.is_empty()
            || app.name.len() > 64
            || !app
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(FlashError::Config(format!(
                "App name '{}' must be 1-64 characters of letters, digits, '-' or '_'",
                app.name
            )));
        }
        if claimed.iter().any(|(_, _, name)| *name == app.name) {
            return err("duplicate app name".into());
        }
        if app.command.as_deref().is_none_or(str::is_empty) && app.runtime.is_none() {
            return err("set 'runtime' (node or python) or 'command'".into());
        }
        if !(1..=64).contains(&app.instances) {
            return err("instances must be 1..=64".into());
        }
        let Some(last) = app
            .port
            .checked_add(app.instances - 1)
            .filter(|_| app.port != 0)
        else {
            return err("port must be nonzero and port + instances - 1 must fit in 65535".into());
        };
        let mut reserved = vec![cfg.server.port];
        if let Some(tls) = cfg.tls.as_ref().filter(|t| t.enabled) {
            reserved.push(tls.http_port);
        }
        if let Some(h3) = cfg.http3.as_ref().filter(|h| h.enabled) {
            reserved.push(h3.port.unwrap_or(cfg.server.port));
        }
        if reserved.iter().any(|p| (app.port..=last).contains(p)) {
            return err(format!(
                "ports {}..={last} overlap a port FlashLin itself listens on",
                app.port
            ));
        }
        if let Some((_, _, other)) = claimed
            .iter()
            .find(|(first, end, _)| app.port <= *end && *first <= last)
        {
            return err(format!("ports overlap app '{other}'"));
        }
        claimed.push((app.port, last, &app.name));
        for (field, value) in [
            ("health_interval", &app.health_interval),
            ("start_timeout", &app.start_timeout),
            ("stop_timeout", &app.stop_timeout),
        ] {
            if !parse_duration_secs(value).is_some_and(|s| s > 0) {
                return err(format!("invalid {field} '{value}'"));
            }
        }
        // The path is written into the probe's request line verbatim.
        if let Some(path) = &app.health_check {
            if !path.starts_with('/') || path.bytes().any(|b| !b.is_ascii_graphic()) {
                return err("health_check must be a path like \"/health\" with no spaces".into());
            }
        }
        for key in app.env.keys() {
            if key.is_empty() || key.contains(['=', ' ']) {
                return err(format!("invalid environment variable name '{key}'"));
            }
        }
        if app.user.is_some() && !cfg!(unix) {
            return err("'user' is only supported on Unix".into());
        }
        if let Some(cwd) = &app.cwd {
            if !Path::new(cwd).is_dir() {
                tracing::warn!(app = %app.name, cwd, "App working directory does not exist");
            }
        }
    }
    Ok(())
}
