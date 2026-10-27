#[cfg(test)]
mod tests {
    use crate::config::parser::{parse_duration_secs, parse_rate_limit, parse_size};

    #[test]
    fn parse_sizes() {
        assert_eq!(parse_size("20MB"), Some(20 * 1024 * 1024));
        assert_eq!(parse_size("32KB"), Some(32 * 1024));
        assert_eq!(parse_size("1GB"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size("512B"), Some(512));
        assert_eq!(parse_size("1024"), Some(1024));
        assert_eq!(parse_size("bad"), None);
    }

    #[test]
    fn parse_durations() {
        assert_eq!(parse_duration_secs("30s"), Some(30));
        assert_eq!(parse_duration_secs("2m"), Some(120));
        assert_eq!(parse_duration_secs("1h"), Some(3600));
        assert_eq!(parse_duration_secs("45"), Some(45));
    }

    #[test]
    fn parse_rate_limits() {
        assert_eq!(parse_rate_limit("100/min"), Some((100, 60)));
        assert_eq!(parse_rate_limit("500/hour"), Some((500, 3600)));
        assert_eq!(parse_rate_limit("10/sec"), Some((10, 1)));
        assert_eq!(parse_rate_limit("bad"), None);
    }

    #[test]
    fn default_config_is_valid() {
        // Build the default config and validate it — must not fail.
        use crate::config::{
            parser::ServerSection, CompressionConfig, LoggingConfig, RouteConfig, SecurityConfig,
            ServerConfig,
        };

        let cfg = ServerConfig {
            server: ServerSection {
                host: "0.0.0.0".into(),
                port: 8080,
                workers: None,
                server_name: "FlashLin".into(),
                read_timeout: "30s".into(),
                write_timeout: "60s".into(),
                keep_alive: true,
                keep_alive_timeout: "75s".into(),
                root: Some("./public".into()),
                index: Some(vec!["index.html".into()]),
                max_connections: 0,
                domains_dir: None,
                direct_static_io: false,
                sendfile: false,
                max_connections_per_ip: 0,
                control_token: None,
                directory_listing_css: None,
            },
            http3: None,
            tls: None,
            security: Some(SecurityConfig {
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
            compression: None,
            cache: None,
            php: None,
            logging: None,
            routes: None,
            static_dirs: None,
            virtual_hosts: vec![],
            vhost_index: Default::default(),
            apps: vec![],
            state: Default::default(),
        };

        assert!(crate::config::validate(&cfg).is_ok());
    }
}
#[test]
fn http3_configuration_requires_build_feature_tls_and_valid_bounds() {
    let example: crate::config::ServerConfig =
        toml::from_str(include_str!("../../examples/http3.toml")).unwrap();
    assert!(example.http3.as_ref().unwrap().enabled);
    assert_eq!(
        example.server.port,
        example.http3.as_ref().unwrap().port.unwrap()
    );
    let mut cfg = crate::default_config();
    cfg.http3 = Some(toml::from_str("enabled = false").unwrap());
    assert!(crate::config::validate(&cfg).is_ok());
    cfg.http3.as_mut().unwrap().enabled = true;
    let error = crate::config::validate(&cfg).unwrap_err().to_string();
    if cfg!(feature = "http3") {
        assert!(error.contains("TLS"));
    } else {
        assert!(error.contains("--features http3"));
    }
    cfg.tls = Some(crate::config::TlsConfig {
        enabled: true,
        acme: None,
        cert: None,
        key: None,
        http_port: 80,
        redirect_http_to_https: false,
    });
    for streams in [0, 129] {
        cfg.http3.as_mut().unwrap().max_concurrent_streams = streams;
        assert!(crate::config::validate(&cfg).is_err());
    }
    cfg.http3.as_mut().unwrap().max_concurrent_streams = 32;
    cfg.http3.as_mut().unwrap().port = Some(0);
    assert!(crate::config::validate(&cfg).is_err());
    assert!(
        toml::from_str::<crate::config::parser::Http3Config>("enabled = true\nprt = 443").is_err()
    );
}
