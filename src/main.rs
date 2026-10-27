// FlashLin — Fast, Reliable, Cached, Powerful Rust Web Server
// v3.0.0 — supervised Node.js/Python apps · per-client limits · faster vhosts

mod apps;
mod cache;
mod cli;
mod config;
mod error;
mod error_pages;
mod flashweb;
mod http;
mod logging;
mod metrics;
mod middleware;
mod php;
mod proxy;
mod security;
mod server;
mod static_files;
#[cfg(feature = "tls")]
mod tls;
mod version;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::info;

fn main() -> anyhow::Result<()> {
    if handle_metadata_command() {
        return Ok(());
    }
    let cli_args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(exit_code) = cli::try_dispatch(&cli_args) {
        std::process::exit(exit_code);
    }

    let banner = r#"
  ______ _           _     _     _
 |  ____| |         | |   | |   (_)
 | |__  | | __ _ ___| |__ | |    _ _ __
 |  __| | |/ _` / __| '_ \| |   | | '_ \
 | |    | | (_| \__ \ | | | |___| | | | |
 |_|    |_|\__,_|___/_| |_|_____|_|_| |_|
    Fast · Reliable · Cached · Powerful
    "#;
    // Determine config file path (default: server.toml in current directory).
    let args: Vec<String> = std::env::args().skip(1).collect();
    let check_only = args
        .first()
        .is_some_and(|arg| arg == "--check" || arg == "-t");
    let watch = args.first().is_some_and(|arg| arg == "--watch");
    let config_path = args
        .get(usize::from(check_only || watch))
        .cloned()
        .unwrap_or_else(|| "server.toml".into());
    let raw_config = server::reload::load_config(&config_path)?;
    logging::init(raw_config.logging.as_ref())?;
    if !check_only && !raw_config.logging.as_ref().is_some_and(|l| l.json) {
        println!("{banner}");
    }

    if check_only {
        println!("Configuration OK: {config_path}");
        return Ok(());
    }

    let config = Arc::new(raw_config);

    info!(
        server_name  = %config.server.server_name,
        host         = %config.server.host,
        port         = config.server.port,
        workers      = config.server.workers.unwrap_or_else(num_cpus),
        tls_enabled  = config.tls.as_ref().map(|t| t.enabled).unwrap_or(false),
        php_enabled  = config.php.as_ref().map(|p| p.enabled).unwrap_or(false),
        "FlashLin starting"
    );

    // Build the runtime only after configuration has been read.  This makes the
    // documented worker setting real instead of merely displaying it at startup.
    let workers = config.server.workers.unwrap_or_else(num_cpus);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()?;
    let server = server::FlashLinServer::new(config, watch.then_some(config_path))?;
    runtime.block_on(server.run())
}

fn handle_metadata_command() -> bool {
    let Some(arg) = std::env::args().nth(1) else {
        return false;
    };

    match arg.as_str() {
        "--version" | "-V" | "version" => {
            version::print_version();
            true
        }
        "--license" | "license" | "--show-license" => {
            version::print_license();
            true
        }
        "--help" | "-h" | "help" => {
            print_help();
            true
        }
        _ => false,
    }
}

fn print_help() {
    let lines = [
        "FlashLin — Fast, Reliable, Cached, Powerful Rust web server",
        "",
        "Run the server:",
        "  flashlin [server.toml]         Run in the foreground (default: ./server.toml)",
        "  flashlin --watch [server.toml] Run with file-polled hot config reload",
        "  flashlin --check [server.toml] Validate configuration without binding a port (-t)",
        "",
        "Manage an installed service (requires production/install.sh's systemd unit):",
        "  flashlin start                 Start the flashlin systemd service",
        "  flashlin stop                  Stop it",
        "  flashlin restart               Restart it",
        "  flashlin status                Show whether it's running (systemctl status)",
        "  flashlin purge cache [config]  Clear the running server's in-memory response cache",
        "  flashlin remove                Stop it and remove the binary + systemd unit",
        "                                 (config, web root and certs are left in place)",
        "",
        "Manage background apps ([[apps]] in server.toml) of a running server:",
        "  flashlin apps [config]         List apps with state, pid, port, restarts, health",
        "  flashlin apps restart <name> [config]",
        "                                 Gracefully restart every instance of an app",
        "",
        "Other:",
        "  flashlin --version             Show version and build info (-V)",
        "  flashlin --license             Show the license text",
        "  flashlin --help                Show this message (-h)",
        "",
        "Full docs: docs/PRODUCTION.md and production/README.md in the repository.",
    ];
    for line in lines {
        println!("{line}");
    }
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

fn resolve_config_relative_path(config_path: &str, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return path;
    }

    Path::new(config_path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.join(&path))
        .unwrap_or(path)
}

fn promote_domain_tls(config: &mut config::ServerConfig) {
    if config.tls.as_ref().map(|tls| tls.enabled).unwrap_or(false) {
        return;
    }

    if let Some((domain, tls)) = config.virtual_hosts.iter().find_map(|vhost| {
        vhost
            .tls
            .as_ref()
            .filter(|tls| tls.enabled)
            .map(|tls| (vhost.domain.name.as_str(), tls.clone()))
    }) {
        info!(
            domain = domain,
            "Using TLS certificate from virtual host for HTTPS listener"
        );
        if config.server.port == tls.http_port {
            info!(
                old_port = config.server.port,
                https_port = 443,
                http_port = tls.http_port,
                "Promoting HTTPS listener to port 443 because TLS came from a virtual host"
            );
            config.server.port = 443;
        }
        config.tls = Some(tls);
    }
}

/// Build a safe default config when no server.toml is found.
fn default_config() -> config::ServerConfig {
    config::ServerConfig {
        server: config::parser::ServerSection {
            host: "0.0.0.0".into(),
            port: 8080,
            workers: None,
            server_name: "FlashLin".into(),
            read_timeout: "30s".into(),
            write_timeout: "60s".into(),
            keep_alive: true,
            keep_alive_timeout: "75s".into(),
            root: Some("./public".into()),
            index: Some(vec!["index.html".into(), "index.php".into()]),
            max_connections: 0,
            domains_dir: None,
            direct_static_io: false,
            sendfile: false,
            max_connections_per_ip: 0,
            control_token: None,
        },
        http3: None,
        tls: None,
        security: Some(config::SecurityConfig {
            max_body_size: "20MB".into(),
            max_header_size: "32KB".into(),
            rate_limit: None,
            hide_server_header: true,
            block_dotfiles: true,
            block_sensitive_files: true,
            allow_ips: None,
            deny_ips: None,
            security_headers: true,
            csp: None,
        }),
        compression: Some(config::CompressionConfig {
            gzip: true,
            brotli: true,
            min_size: 1024,
        }),
        cache: Some(config::CacheConfig {
            enabled: true,
            max_size: "128MB".into(),
            ttl: "300s".into(),
            revalidate_after: "0s".into(),
            max_file_size: "10MB".into(),
        }),
        php: None,
        logging: Some(config::LoggingConfig {
            level: "info".into(),
            json: false,
            access_log: None,
            error_log: None,
        }),
        routes: Some(vec![config::RouteConfig {
            path: "/".into(),
            root: Some("./public".into()),
            index: Some("index.html".into()),
            proxy_pass: None,
            proxy_upstreams: vec![],
            php_handler: None,
            php_enabled: false,
            cache_control: None,
            methods: None,
            directory_listing: false,
            app: None,
        }]),
        static_dirs: None,
        virtual_hosts: vec![],
        vhost_index: Default::default(),
        apps: vec![],
        state: Default::default(),
    }
}
