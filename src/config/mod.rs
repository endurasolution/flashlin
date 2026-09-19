pub mod parser;
pub mod validation;

pub use parser::{
    load, load_virtual_host, parse_duration_secs, parse_rate_limit, parse_size, CacheConfig,
    CompressionConfig, LoggingConfig, PhpHandler, PhpMode, RouteConfig, SecurityConfig,
    ServerConfig, TlsConfig, VirtualHostConfig,
};
pub use validation::validate;

/// Load every `*.toml` file in `dir` as a `VirtualHostConfig`.
/// Any unreadable or malformed entry rejects the configuration.
pub fn load_domains(dir: &str) -> crate::error::Result<Vec<VirtualHostConfig>> {
    use std::fs;
    let entries = fs::read_dir(dir).map_err(|e| {
        crate::error::FlashError::Config(format!("Cannot read domains dir '{dir}': {e}"))
    })?;
    let mut vhosts = Vec::new();
    let mut paths = entries
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| crate::error::FlashError::Config(e.to_string()))?;
    paths.sort();
    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let path_str = path.to_string_lossy().into_owned();
        match load_virtual_host(&path_str) {
            Ok(vh) => {
                tracing::info!(domain = %vh.domain.name, file = %path_str, "Virtual host loaded");
                vhosts.push(vh);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(vhosts)
}
