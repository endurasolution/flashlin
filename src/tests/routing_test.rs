#[cfg(test)]
mod tests {
    use crate::config::{parser::ServerSection, RouteConfig, SecurityConfig, ServerConfig};
    use std::sync::Arc;

    fn make_config(routes: Vec<RouteConfig>) -> Arc<ServerConfig> {
        Arc::new(ServerConfig {
            server: ServerSection {
                host: "127.0.0.1".into(),
                port: 8080,
                workers: None,
                server_name: "test".into(),
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
            security: None,
            compression: None,
            cache: None,
            php: None,
            logging: None,
            routes: Some(routes),
            static_dirs: None,
            virtual_hosts: vec![],
            vhost_index: Default::default(),
            apps: vec![],
            realweb: None,
            state: Default::default(),
        })
    }

    #[test]
    fn finds_exact_route() {
        let cfg = make_config(vec![
            RouteConfig {
                path: "/api".into(),
                root: None,
                index: None,
                proxy_pass: Some("http://127.0.0.1:3000".into()),
                proxy_upstreams: vec![],
                php_handler: None,
                php_enabled: false,
                cache_control: None,
                methods: None,
                directory_listing: false,
                app: None,
            },
            RouteConfig {
                path: "/".into(),
                root: Some("./public".into()),
                index: None,
                proxy_pass: None,
                proxy_upstreams: vec![],
                php_handler: None,
                php_enabled: false,
                cache_control: None,
                methods: None,
                directory_listing: false,
                app: None,
            },
        ]);

        // Simulate the longest-prefix routing logic from router.rs.
        let path = "/api/users";
        let routes = cfg.routes.as_deref().unwrap();
        let matched = routes
            .iter()
            .filter(|r| {
                path == r.path || path.starts_with(&format!("{}/", r.path)) || r.path == "/"
            })
            .max_by_key(|r| r.path.len());

        assert!(matched.is_some());
        assert_eq!(matched.unwrap().path, "/api");
    }

    #[test]
    fn falls_back_to_root_route() {
        let cfg = make_config(vec![RouteConfig {
            path: "/".into(),
            root: Some("./public".into()),
            index: None,
            proxy_pass: None,
            proxy_upstreams: vec![],
            php_handler: None,
            php_enabled: false,
            cache_control: None,
            methods: None,
            directory_listing: false,
            app: None,
        }]);

        let path = "/anything/here";
        let routes = cfg.routes.as_deref().unwrap();
        let matched = routes
            .iter()
            .filter(|r| {
                path == r.path || path.starts_with(&format!("{}/", r.path)) || r.path == "/"
            })
            .max_by_key(|r| r.path.len());

        assert!(matched.is_some());
        assert_eq!(matched.unwrap().path, "/");
    }

    #[test]
    fn no_match_when_no_routes() {
        let cfg = make_config(vec![]);
        let routes = cfg.routes.as_deref().unwrap_or(&[]);
        let matched = routes.iter().find(|r| r.path == "/strict");
        assert!(matched.is_none());
    }
}
