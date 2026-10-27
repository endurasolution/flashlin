//! `flashlin start|stop|restart|status|remove|purge cache|apps` — control-plane
//! commands for an already-installed FlashLin, as a companion to running the
//! server itself (`flashlin server.toml`).
//!
//! None of these run the server in-process. `start`/`stop`/`restart`/`status`
//! delegate to systemd, since `production/install.sh` already sets up a unit
//! that supervises restarts and log capture properly — reimplementing that
//! here would just be a worse copy of it. `remove` reverses the same two
//! artifacts (binary, systemd unit) that `install.sh` created, the same safe,
//! non-destructive default as `production/uninstall.sh`; it never touches
//! config, TLS keys or your site content; use `uninstall.sh --purge` for that.
//! `purge cache` is different in kind: the response cache it clears lives in
//! the memory of the *running* server process, not this short-lived CLI
//! invocation, so there is no file or systemd call that can reach it — it
//! sends a loopback-only HTTP request to a control endpoint the server
//! exposes for exactly this (`POST /flashlin/purge-cache`, see
//! `src/http/router.rs`). `apps` works the same way, against
//! `GET /flashlin/apps` and `POST /flashlin/apps/<name>/restart`.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const SERVICE_NAME: &str = "flashlin";
const SERVICE_FILE: &str = "/etc/systemd/system/flashlin.service";
const BIN_PATH: &str = "/usr/local/bin/flashlin";
const DEFAULT_CONFIG_CANDIDATES: &[&str] = &["/etc/flashlin/server.toml", "server.toml"];

/// Recognizes one of this module's subcommands as the first CLI argument and
/// runs it. Returns `None` for anything else, so `main` falls through to its
/// existing argument handling (config path, `--check`, `--watch`, ...)
/// unchanged.
pub fn try_dispatch(args: &[String]) -> Option<i32> {
    match args.first().map(String::as_str) {
        Some(cmd @ ("start" | "stop" | "restart" | "status")) => Some(service_command(cmd)),
        Some("remove") => Some(remove_installation()),
        Some("purge") if args.get(1).map(String::as_str) == Some("cache") => {
            Some(purge_cache(args.get(2).map(String::as_str)))
        }
        Some("apps") => Some(apps_command(&args[1..])),
        _ => None,
    }
}

fn service_command(action: &str) -> i32 {
    match Command::new("systemctl").arg(action).arg(SERVICE_NAME).status() {
        Ok(status) => {
            if !status.success() && !Path::new(SERVICE_FILE).exists() {
                eprintln!();
                eprintln!("No flashlin systemd service is installed yet.");
                eprintln!("Run production/install.sh first, or start FlashLin directly:");
                eprintln!("  flashlin /etc/flashlin/server.toml");
            }
            status.code().unwrap_or(1)
        }
        Err(e) => {
            eprintln!("Could not run systemctl ({e}).");
            eprintln!("This command manages the systemd service install.sh sets up;");
            eprintln!("without systemd, run FlashLin directly:");
            eprintln!("  flashlin /etc/flashlin/server.toml");
            1
        }
    }
}

fn remove_installation() -> i32 {
    if !running_as_root() {
        eprintln!("Run as root: sudo flashlin remove");
        return 1;
    }

    println!("Stopping and disabling the flashlin service...");
    let _ = Command::new("systemctl")
        .args(["stop", SERVICE_NAME])
        .status();
    let _ = Command::new("systemctl")
        .args(["disable", SERVICE_NAME])
        .status();

    if Path::new(SERVICE_FILE).exists() {
        match std::fs::remove_file(SERVICE_FILE) {
            Ok(()) => {
                println!("Removed {SERVICE_FILE}");
                let _ = Command::new("systemctl").arg("daemon-reload").status();
            }
            Err(e) => eprintln!("Could not remove {SERVICE_FILE}: {e}"),
        }
    }

    // Unlinking the binary that is currently executing this code is safe on
    // Linux: the process keeps running from the already-open inode until it
    // exits, and BIN_PATH is always the installed copy regardless of which
    // copy of the binary is invoking `remove`.
    match std::fs::remove_file(BIN_PATH) {
        Ok(()) => println!("Removed {BIN_PATH}"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            eprintln!("Could not remove {BIN_PATH}: {e}");
            return 1;
        }
    }

    println!();
    println!("Service, binary and systemd unit removed.");
    println!("Config, web root and TLS/ACME data were left in place, since they may");
    println!("hold real content or private keys. Run uninstall.sh --purge from the");
    println!("production package to remove those too.");
    0
}

/// Port and control token of the server described by `config_arg` (or the
/// default config location).
struct ControlTarget {
    port: u16,
    token: Option<String>,
}

fn control_target(config_arg: Option<&str>) -> Result<ControlTarget, i32> {
    let config_path = config_arg.map(str::to_owned).unwrap_or_else(|| {
        DEFAULT_CONFIG_CANDIDATES
            .iter()
            .find(|p| Path::new(p).is_file())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "server.toml".to_string())
    });

    let config = match crate::server::reload::load_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Could not read {config_path}: {e}");
            return Err(1);
        }
    };
    let port = config.server.port;

    if config.tls.as_ref().is_some_and(|t| t.enabled) {
        eprintln!(
            "Note: TLS is enabled in {config_path}. This command only speaks plain \
             HTTP; if port {port} doesn't also accept plain HTTP, the request below \
             will fail to connect."
        );
    }
    Ok(ControlTarget {
        port,
        token: config.server.control_token,
    })
}

fn purge_cache(config_arg: Option<&str>) -> i32 {
    let target = match control_target(config_arg) {
        Ok(target) => target,
        Err(code) => return code,
    };
    match control_request(&target, "POST", "/flashlin/purge-cache") {
        Ok((200, body)) => {
            print_body(&body);
            0
        }
        Ok((status, body)) => report_refusal(status, &body),
        Err(e) => report_unreachable(target.port, &e),
    }
}

fn apps_command(args: &[String]) -> i32 {
    let (restart, config_arg) = match args.first().map(String::as_str) {
        Some("restart") => match args.get(1) {
            Some(name) => (Some(name.as_str()), args.get(2)),
            None => {
                eprintln!("Usage: flashlin apps restart <name> [config]");
                return 2;
            }
        },
        Some("status") => (None, args.get(1)),
        _ => (None, args.first()),
    };
    let target = match control_target(config_arg.map(String::as_str)) {
        Ok(target) => target,
        Err(code) => return code,
    };

    if let Some(name) = restart {
        let encoded =
            percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC);
        return match control_request(
            &target,
            "POST",
            &format!("/flashlin/apps/{encoded}/restart"),
        ) {
            Ok((200, body)) => {
                print_body(&body);
                0
            }
            Ok((status, body)) => report_refusal(status, &body),
            Err(e) => report_unreachable(target.port, &e),
        };
    }

    match control_request(&target, "GET", "/flashlin/apps") {
        Ok((200, body)) => match serde_json::from_str::<Vec<crate::apps::AppStatus>>(&body) {
            Ok(apps) => {
                print_apps(&apps);
                0
            }
            Err(e) => {
                eprintln!("Unexpected response from FlashLin: {e}");
                1
            }
        },
        Ok((status, body)) => report_refusal(status, &body),
        Err(e) => report_unreachable(target.port, &e),
    }
}

fn print_apps(apps: &[crate::apps::AppStatus]) {
    println!(
        "{:<20} {:>4} {:>6}  {:<9} {:>8} {:>8}  {:<9} {:>9}  LAST EXIT",
        "NAME", "INST", "PORT", "STATE", "PID", "RESTARTS", "HEALTH", "UPTIME"
    );
    for app in apps {
        println!(
            "{:<20} {:>4} {:>6}  {:<9} {:>8} {:>8}  {:<9} {:>9}  {}",
            app.name,
            app.instance,
            app.port,
            app.state,
            app.pid.map_or("-".into(), |p| p.to_string()),
            app.restarts,
            match app.healthy {
                Some(true) => "healthy",
                Some(false) => "unhealthy",
                None => "-",
            },
            app.uptime_secs.map_or("-".into(), format_uptime),
            app.last_exit.as_deref().unwrap_or("-"),
        );
    }
}

fn format_uptime(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{:02}s", secs / 60, secs % 60),
        3600..=86399 => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d{:02}h", secs / 86400, secs % 86400 / 3600),
    }
}

fn print_body(body: &str) {
    print!("{body}");
    if !body.ends_with('\n') {
        println!();
    }
}

fn report_refusal(status: u16, body: &str) -> i32 {
    match status {
        403 => eprintln!(
            "FlashLin refused the control request (403). It must come from this \
             machine, and must match server.control_token if one is set."
        ),
        _ => eprint!("FlashLin answered {status}: {body}"),
    }
    1
}

fn report_unreachable(port: u16, error: &std::io::Error) -> i32 {
    eprintln!("Could not reach FlashLin on 127.0.0.1:{port}: {error}");
    eprintln!("Is it running? Try: flashlin status");
    1
}

/// Sends one request to the running server's loopback control endpoint and
/// returns its status code and body.
fn control_request(
    target: &ControlTarget,
    method: &str,
    path: &str,
) -> std::io::Result<(u16, String)> {
    let port = target.port;
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let authorization = target
        .token
        .as_deref()
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         {authorization}\
         Connection: close\r\n\
         Content-Length: 0\r\n\
         \r\n"
    );
    stream.write_all(request.as_bytes())?;
    // Deliberately not calling shutdown(Write) here: on this connection, doing
    // so immediately after write_all (loopback, so the FIN can arrive at the
    // peer within microseconds) reproducibly made the server's hyper listener
    // close the connection without responding at all — read_to_end returned
    // Ok with zero bytes, confirmed with debug instrumentation before this
    // fix. `Connection: close` in the request is enough on its own: the
    // server closes its side after responding, which ends read_to_end
    // naturally; the read timeout above still bounds a server that never
    // responds at all.
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let text = String::from_utf8_lossy(&response);

    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::other("malformed response from server"))?;
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or("")
        .to_string();
    Ok((status, body))
}

fn running_as_root() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid() takes no arguments and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}
