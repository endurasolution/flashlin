//! `flashlin start|stop|restart|status|remove|purge cache` — control-plane
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
//! `src/http/router.rs`).

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

fn purge_cache(config_arg: Option<&str>) -> i32 {
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
            return 1;
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

    match request_purge(port) {
        Ok(body) => {
            print!("{body}");
            if !body.ends_with('\n') {
                println!();
            }
            0
        }
        Err(e) => {
            eprintln!("Could not reach FlashLin on 127.0.0.1:{port}: {e}");
            eprintln!("Is it running? Try: flashlin status");
            1
        }
    }
}

fn request_purge(port: u16) -> std::io::Result<String> {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let request = format!(
        "POST /flashlin/purge-cache HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
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

    let status_line = text.lines().next().unwrap_or("");
    if !status_line.contains(" 200 ") {
        return Err(std::io::Error::other(format!(
            "unexpected response: {status_line}"
        )));
    }
    Ok(text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or("")
        .to_string())
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
