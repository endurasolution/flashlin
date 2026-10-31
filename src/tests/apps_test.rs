//! FlashLin 3.0: `[[apps]]` config, supervision of real Node.js/Python
//! processes, control endpoints, per-generation virtual-host index.
use crate::apps::{AppManager, AppStatus};
use crate::config::ServerConfig;
use crate::server::reload::load_config;
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::{Method, Request};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn fixture_dir() -> PathBuf {
    let dir = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!("apps-fixture-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// TOML-safe absolute path.
fn toml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn has_runtime(program: &str) -> bool {
    std::process::Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn python() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "python3"
    }
}

/// `count` consecutive loopback ports that are free right now and not handed
/// to another test in this process. Without that record, the OS can offer a
/// port another test picked but whose app hasn't bound it yet.
fn free_ports(count: u16) -> u16 {
    static TAKEN: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());
    let mut taken = TAKEN.lock().unwrap();
    loop {
        let base = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let Some(end) = base.checked_add(count) else {
            continue;
        };
        if taken.iter().any(|port| (base..end).contains(port)) {
            continue;
        }
        if (1..count).all(|i| std::net::TcpListener::bind(("127.0.0.1", base + i)).is_ok()) {
            taken.extend(base..end);
            return base;
        }
    }
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn send(
    config: &Arc<ServerConfig>,
    peer: &str,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
) -> (u16, String) {
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let peer: SocketAddr = peer.parse().unwrap();
    let mut request = request.body(Empty::<Bytes>::new()).unwrap();
    request
        .extensions_mut()
        .insert(crate::http::request::ConnectionInfo(peer, false));
    let response = crate::http::router::dispatch(request, peer, config.clone()).await;
    let status = response.status().as_u16();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn app_states(manager: &AppManager, name: &str) -> Vec<AppStatus> {
    manager
        .status()
        .into_iter()
        .filter(|s| s.name == name)
        .collect()
}

// ─── Config ──────────────────────────────────────────────────────────────────

fn write_config(dir: &Path, body: &str) -> String {
    let path = dir.join("server.toml");
    std::fs::create_dir_all(dir.join("public")).unwrap();
    std::fs::write(
        &path,
        format!(
            "[server]\nport = 18080\nroot = '{}'\n\n{body}",
            toml_path(&dir.join("public"))
        ),
    )
    .unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn app_routes_resolve_to_loopback_upstreams_in_global_and_virtual_hosts() {
    let dir = fixture_dir();
    std::fs::create_dir_all(dir.join("domains")).unwrap();
    std::fs::write(
        dir.join("domains/example.toml"),
        "[domain]\nname = \"example.com\"\n\n[[routes]]\npath = \"/\"\napp = \"web\"\n",
    )
    .unwrap();
    let path = dir.join("server.toml");
    std::fs::create_dir_all(dir.join("public")).unwrap();
    std::fs::write(
        &path,
        format!(
            "[server]\nport = 18080\ndomains_dir = '{}'\n\n\
             [[apps]]\nname = \"web\"\nruntime = \"node\"\nscript = \"server.js\"\nport = 3000\n\n\
             [[apps]]\nname = \"api\"\ncommand = \"gunicorn\"\nport = 4000\ninstances = 3\n\n\
             [[routes]]\npath = \"/\"\nroot = '{}'\n\n\
             [[routes]]\npath = \"/api\"\napp = \"api\"\n",
            toml_path(&dir.join("domains")),
            toml_path(&dir.join("public")),
        ),
    )
    .unwrap();
    let cfg = load_config(path.to_str().unwrap()).unwrap();

    let api = &cfg.routes.as_ref().unwrap()[1];
    assert_eq!(api.proxy_pass, None);
    assert_eq!(
        api.proxy_upstreams,
        [
            "http://127.0.0.1:4000",
            "http://127.0.0.1:4001",
            "http://127.0.0.1:4002"
        ]
    );
    let vhost_route = &cfg.virtual_hosts[0].routes.as_ref().unwrap()[0];
    assert_eq!(
        vhost_route.proxy_pass.as_deref(),
        Some("http://127.0.0.1:3000")
    );
}

#[test]
fn invalid_app_configs_are_rejected() {
    let base = "[[apps]]\nname = \"web\"\nruntime = \"node\"\nscript = \"s.js\"\nport = 3000\n";
    let cases = [
        ("[[routes]]\npath = \"/x\"\napp = \"missing\"\n", "not defined"),
        (
            "[[routes]]\npath = \"/x\"\napp = \"web\"\nproxy_pass = \"http://127.0.0.1:9\"\n",
            "cannot be combined",
        ),
        ("[[apps]]\nname = \"web\"\ncommand = \"x\"\nport = 5000\n", "duplicate"),
        ("[[apps]]\nname = \"b\"\ncommand = \"x\"\nport = 2999\ninstances = 2\n", "overlap app 'web'"),
        ("[[apps]]\nname = \"b\"\ncommand = \"x\"\nport = 18080\n", "FlashLin itself listens"),
        ("[[apps]]\nname = \"b\"\nport = 5000\n", "runtime"),
        ("[[apps]]\nname = \"b c\"\ncommand = \"x\"\nport = 5000\n", "must be 1-64"),
        ("[[apps]]\nname = \"b\"\ncommand = \"x\"\nport = 65535\ninstances = 2\n", "fit in 65535"),
        (
            "[[apps]]\nname = \"b\"\ncommand = \"x\"\nport = 5000\nhealth_check = \"/h\\r\\nHost: evil\"\n",
            "health_check",
        ),
        ("[[apps]]\nname = \"b\"\ncommand = \"x\"\nport = 5000\nstop_timeout = \"soon\"\n", "stop_timeout"),
        ("[[apps]]\nname = \"b\"\ncommand = \"x\"\nport = 5000\nbogus = 1\n", "unknown field"),
    ];
    for (extra, expected) in cases {
        let dir = fixture_dir();
        let path = write_config(&dir, &format!("{base}\n{extra}"));
        let error = load_config(&path).expect_err(extra).to_string();
        assert!(error.contains(expected), "{extra:?}: {error}");
    }

    let dir = fixture_dir();
    let short_token = write_config(&dir, "").replace("server.toml", "short.toml");
    std::fs::write(
        &short_token,
        "[server]\nport = 18080\ncontrol_token = \"short\"\n",
    )
    .unwrap();
    assert!(load_config(&short_token)
        .unwrap_err()
        .to_string()
        .contains("control_token"));
}

#[test]
fn changing_apps_requires_a_restart_on_hot_reload() {
    let dir = fixture_dir();
    let app = "[[apps]]\nname = \"web\"\ncommand = \"x\"\nport = 3000\n";
    let path = write_config(&dir, app);
    let live = Arc::new(arc_swap::ArcSwap::from_pointee(
        crate::server::reload::Snapshot::new(Arc::new(load_config(&path).unwrap())).unwrap(),
    ));
    write_config(&dir, &app.replace("3000", "3005"));
    let error = crate::server::reload::reload_once(&live, &path).unwrap_err();
    assert!(error.to_string().contains("[[apps]]"), "{error}");
}

// ─── Virtual-host index ──────────────────────────────────────────────────────

#[tokio::test]
async fn virtual_hosts_are_merged_once_and_served_by_name_and_alias() {
    let dir = fixture_dir();
    for (folder, body) in [("public", "default site"), ("site", "example site")] {
        std::fs::create_dir_all(dir.join(folder)).unwrap();
        std::fs::write(dir.join(folder).join("index.html"), body).unwrap();
    }
    std::fs::create_dir_all(dir.join("domains")).unwrap();
    std::fs::write(
        dir.join("domains/example.toml"),
        format!(
            "[domain]\nname = \"Example.com\"\naliases = [\"www.example.com\"]\nroot = '{}'\n",
            toml_path(&dir.join("site"))
        ),
    )
    .unwrap();
    // Still inside [server]: write_config's body follows that table.
    let path = write_config(
        &dir,
        &format!("domains_dir = '{}'\n", toml_path(&dir.join("domains"))),
    );

    let cfg = Arc::new(load_config(&path).unwrap());
    let index = format!("{:?}", cfg.vhost_index);
    assert!(
        index.contains("example.com") && index.contains("www.example.com"),
        "{index}"
    );

    let site = |host: &'static str| {
        let cfg = cfg.clone();
        async move { send(&cfg, "127.0.0.1:5000", Method::GET, "/", &[("host", host)]).await }
    };
    assert_eq!(site("example.com").await, (200, "example site".into()));
    assert_eq!(
        site("WWW.EXAMPLE.COM:8080").await,
        (200, "example site".into())
    );
    assert_eq!(site("other.test").await, (200, "default site".into()));
}

// ─── Control endpoints ───────────────────────────────────────────────────────

#[tokio::test]
async fn control_endpoints_refuse_remote_proxied_and_unauthenticated_requests() {
    let mut cfg = crate::default_config();
    let config = Arc::new(cfg.clone());
    let purge = |peer: &'static str, headers: &'static [(&'static str, &'static str)]| {
        let config = config.clone();
        async move {
            send(
                &config,
                peer,
                Method::POST,
                "/flashlin/purge-cache",
                headers,
            )
            .await
            .0
        }
    };
    assert_eq!(purge("127.0.0.1:1", &[]).await, 200);
    assert_eq!(purge("[::1]:1", &[]).await, 200);
    assert_eq!(purge("203.0.113.9:1", &[]).await, 403);
    assert_eq!(
        purge("127.0.0.1:1", &[("x-forwarded-for", "203.0.113.9")]).await,
        403
    );
    assert_eq!(
        purge("127.0.0.1:1", &[("forwarded", "for=203.0.113.9")]).await,
        403
    );
    assert_eq!(
        purge("127.0.0.1:1", &[("x-real-ip", "203.0.113.9")]).await,
        403
    );

    cfg.server.control_token = Some("0123456789abcdef-secret".into());
    let config = Arc::new(cfg);
    let with = |auth: &'static str| {
        let config = config.clone();
        async move {
            let headers: &[(&str, &str)] = if auth.is_empty() {
                &[]
            } else {
                &[("authorization", auth)]
            };
            send(
                &config,
                "127.0.0.1:1",
                Method::POST,
                "/flashlin/purge-cache",
                headers,
            )
            .await
            .0
        }
    };
    assert_eq!(with("").await, 403);
    assert_eq!(with("Bearer wrong").await, 403);
    assert_eq!(with("Bearer 0123456789abcdef-secre").await, 403);
    assert_eq!(with("Bearer 0123456789abcdef-secret").await, 200);
}

// ─── Live supervision ────────────────────────────────────────────────────────

fn node_app(dir: &Path, name: &str, port: u16, extra: &str) -> crate::config::parser::AppConfig {
    std::fs::write(
        dir.join("server.js"),
        r#"
const http = require('http');
http.createServer((req, res) => {
  if (req.url === '/health') return res.end('ok');
  res.end(`node ${process.env.FLASHLIN_APP} ${process.env.FLASHLIN_INSTANCE} ${req.url} ${req.headers['x-forwarded-for']}`);
}).listen(Number(process.env.PORT), process.env.HOST, () => console.log('listening'));
"#,
    )
    .unwrap();
    toml::from_str(&format!(
        "name = \"{name}\"\nruntime = \"node\"\nscript = '{}'\nport = {port}\n{extra}",
        toml_path(&dir.join("server.js"))
    ))
    .unwrap()
}

fn kill_process(pid: u32) {
    let status = if cfg!(windows) {
        std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output()
    } else {
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output()
    };
    assert!(status.unwrap().status.success());
}

fn port_closed(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(200),
    )
    .is_err()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn node_app_is_proxied_balanced_restarted_and_stopped() {
    if !has_runtime("node") {
        eprintln!("skipping: node is not installed");
        return;
    }
    let dir = fixture_dir();
    let port = free_ports(2);
    let app = node_app(
        &dir,
        "web",
        port,
        "instances = 2\nhealth_check = \"/health\"\nhealth_interval = \"1s\"\nstop_timeout = \"3s\"\n",
    );

    let mut cfg = crate::default_config();
    cfg.apps = vec![app.clone()];
    let mut route = cfg.routes.as_ref().unwrap()[0].clone();
    route.path = "/api".into();
    route.root = None;
    route.app = Some("web".into());
    cfg.routes.as_mut().unwrap().push(route);
    crate::apps::resolve_routes(&mut cfg).unwrap();
    crate::config::validate(&cfg).unwrap();
    let cfg = Arc::new(cfg);

    let manager = AppManager::start(&cfg.apps);
    wait_until("both instances healthy", || {
        app_states(&manager, "web")
            .iter()
            .all(|s| s.healthy == Some(true) && s.state == "running")
    })
    .await;

    // Requests reach both instances through the ordinary proxy path, with the
    // client address forwarded.
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..4 {
        let (status, body) = send(&cfg, "198.51.100.7:4000", Method::GET, "/api/hi", &[]).await;
        assert_eq!(status, 200, "{body}");
        assert!(
            body.starts_with("node web ") && body.ends_with(" /api/hi 198.51.100.7"),
            "{body}"
        );
        seen.insert(body);
    }
    assert_eq!(seen.len(), 2, "both instances should serve: {seen:?}");

    // Manual restart replaces every process.
    let before: Vec<_> = app_states(&manager, "web").iter().map(|s| s.pid).collect();
    assert!(manager.restart("web"));
    assert!(!manager.restart("nope"));
    wait_until("restarted instances healthy", || {
        let now = app_states(&manager, "web");
        now.iter()
            .zip(&before)
            .all(|(s, old)| s.pid.is_some() && s.pid != *old && s.healthy == Some(true))
    })
    .await;

    // A crashed instance is brought back automatically.
    let victim = app_states(&manager, "web")[0].pid.unwrap();
    kill_process(victim);
    wait_until("crashed instance replaced", || {
        let s = &app_states(&manager, "web")[0];
        s.pid.is_some_and(|pid| pid != victim) && s.healthy == Some(true)
    })
    .await;
    let s = &app_states(&manager, "web")[0];
    assert!(s.restarts >= 2, "{s:?}");
    assert!(s.last_exit.is_some(), "{s:?}");

    manager.stop().await;
    for s in app_states(&manager, "web") {
        assert_eq!((s.state.as_str(), s.pid), ("stopped", None));
    }
    assert!(port_closed(port) && port_closed(port + 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_looping_app_gives_up_and_restart_policies_are_honoured() {
    if !has_runtime("node") {
        eprintln!("skipping: node is not installed");
        return;
    }
    let crashing: crate::config::parser::AppConfig = toml::from_str(&format!(
        "name = \"crash\"\nruntime = \"node\"\nargs = [\"-e\", \"process.exit(3)\"]\nport = {}\nmax_restarts = 2\n",
        free_ports(1)
    ))
    .unwrap();
    let clean_exit: crate::config::parser::AppConfig = toml::from_str(&format!(
        "name = \"once\"\nruntime = \"node\"\nargs = [\"-e\", \"0\"]\nport = {}\nrestart = \"on-failure\"\n",
        free_ports(1)
    ))
    .unwrap();
    let manager = AppManager::start(&[crashing, clean_exit]);

    wait_until("crash loop to give up", || {
        app_states(&manager, "crash")[0].state == "failed"
    })
    .await;
    let crash = &app_states(&manager, "crash")[0];
    assert_eq!(crash.last_exit.as_deref(), Some("exit code 3"));
    assert_eq!(crash.restarts, 2, "{crash:?}");

    wait_until("clean exit to stop", || {
        app_states(&manager, "once")[0].state == "stopped"
    })
    .await;
    let once = &app_states(&manager, "once")[0];
    assert_eq!(
        (once.restarts, once.last_exit.as_deref()),
        (0, Some("exit code 0"))
    );

    // A manual restart revives a failed app.
    assert!(manager.restart("crash"));
    wait_until("failed app to be retried", || {
        app_states(&manager, "crash")[0].state != "failed"
    })
    .await;
    manager.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_app_serves_through_flashlin() {
    if !has_runtime(python()) {
        eprintln!("skipping: {} is not installed", python());
        return;
    }
    let dir = fixture_dir();
    std::fs::write(
        dir.join("app.py"),
        r#"
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        body = f"python {os.environ['FLASHLIN_APP']} {self.path}".encode()
        self.send_response(200)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        print("request", self.path)

ThreadingHTTPServer((os.environ["HOST"], int(os.environ["PORT"])), Handler).serve_forever()
"#,
    )
    .unwrap();
    let log = dir.join("logs").join("py.log");
    let app: crate::config::parser::AppConfig = toml::from_str(&format!(
        "name = \"py\"\nruntime = \"python\"\nscript = '{}'\nport = {}\nhealth_check = \"/\"\nlog_file = '{}'\n",
        toml_path(&dir.join("app.py")),
        free_ports(1),
        toml_path(&log),
    ))
    .unwrap();
    let mut cfg = crate::default_config();
    cfg.apps = vec![app];
    cfg.routes.as_mut().unwrap()[0].root = None;
    cfg.routes.as_mut().unwrap()[0].app = Some("py".into());
    crate::apps::resolve_routes(&mut cfg).unwrap();
    let cfg = Arc::new(cfg);

    let manager = AppManager::start(&cfg.apps);
    wait_until("python app healthy", || {
        app_states(&manager, "py")[0].healthy == Some(true)
    })
    .await;
    assert_eq!(
        send(&cfg, "127.0.0.1:1", Method::GET, "/hello?x=1", &[]).await,
        (200, "python py /hello?x=1".into())
    );
    // http.server answers HTTP/1.0; that must not leak to FlashLin's clients.
    let mut request = Request::get("/").body(Empty::<Bytes>::new()).unwrap();
    let peer: SocketAddr = "127.0.0.1:1".parse().unwrap();
    request
        .extensions_mut()
        .insert(crate::http::request::ConnectionInfo(peer, false));
    let response = crate::http::router::dispatch(request, peer, cfg.clone()).await;
    assert_eq!(response.version(), hyper::Version::HTTP_11);
    // PYTHONUNBUFFERED makes the app's prints reach the log file promptly.
    wait_until("app output in log file", || {
        std::fs::read_to_string(&log)
            .is_ok_and(|text| text.contains("py[0] stdout: request /hello?x=1"))
    })
    .await;
    manager.stop().await;
}
