//! Supervised background applications: Node.js, Python, or any command.
//!
//! Each `[[apps]]` entry is started together with the server, restarted with
//! exponential backoff when it exits, optionally health-checked over HTTP, and
//! stopped gracefully (SIGTERM to its whole process group, then SIGKILL after
//! `stop_timeout`) on shutdown.
//!
//! A route connects to an app with `app = "<name>"`. Config loading turns that
//! into ordinary loopback `proxy_pass` / `proxy_upstreams` targets
//! ([`resolve_routes`]), so apps reuse the existing proxy path unchanged:
//! WebSocket upgrades, passive failover across instances, X-Forwarded-*.
//!
//! The running manager is registered process-wide ([`current`]) so the
//! loopback control endpoints behind `flashlin apps` can report status and
//! trigger restarts.

use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::config::parser::{AppConfig, AppRuntime, RestartPolicy};
use crate::config::{parse_duration_secs, ServerConfig};
use crate::error::{FlashError, Result};

/// A run at least this long counts as stable and resets the crash backoff.
const STABLE_RUN: Duration = Duration::from_secs(60);
const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Longest line forwarded from an app's stdout/stderr; the rest is dropped so a
/// newline-free stream cannot grow memory without bound.
const MAX_LOG_LINE: usize = 16 * 1024;
/// Consecutive failed probes after the first success that trigger a restart.
const HEALTH_FAILURES: u32 = 3;

static REGISTRY: Mutex<Option<Arc<AppManager>>> = Mutex::new(None);

/// The manager started by the running server, if any.
pub fn current() -> Option<Arc<AppManager>> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

// ─── Config resolution ───────────────────────────────────────────────────────

/// Loopback upstream URLs for every instance of `app`.
pub fn upstream_urls(app: &AppConfig) -> Vec<String> {
    (0..app.instances)
        .map(|i| format!("http://127.0.0.1:{}", app.port + i))
        .collect()
}

/// Rewrites every `app = "<name>"` route (global and virtual-host) into the
/// equivalent loopback proxy target, so the router needs no app-specific path.
pub fn resolve_routes(cfg: &mut ServerConfig) -> Result<()> {
    let apps = cfg.apps.clone();
    let resolve = |routes: &mut Option<Vec<crate::config::RouteConfig>>| -> Result<()> {
        for route in routes.iter_mut().flatten() {
            let Some(name) = route.app.as_deref() else {
                continue;
            };
            let app = apps.iter().find(|a| a.name == name).ok_or_else(|| {
                FlashError::Config(format!(
                    "Route '{}': app '{name}' is not defined in [[apps]]",
                    route.path
                ))
            })?;
            if route.root.is_some()
                || route.proxy_pass.is_some()
                || !route.proxy_upstreams.is_empty()
            {
                return Err(FlashError::Config(format!(
                    "Route '{}': 'app' cannot be combined with root, proxy_pass or proxy_upstreams",
                    route.path
                )));
            }
            let mut urls = upstream_urls(app);
            if urls.len() == 1 {
                route.proxy_pass = urls.pop();
            } else {
                route.proxy_upstreams = urls;
            }
        }
        Ok(())
    };
    resolve(&mut cfg.routes)?;
    for host in &mut cfg.virtual_hosts {
        resolve(&mut host.routes)?;
    }
    Ok(())
}

// ─── Manager ─────────────────────────────────────────────────────────────────

pub struct AppManager {
    apps: Vec<(Arc<AppConfig>, Vec<Arc<Instance>>)>,
    stop: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

struct Instance {
    index: u16,
    port: u16,
    status: Mutex<InstanceStatus>,
    restart: Notify,
}

#[derive(Default)]
struct InstanceStatus {
    state: &'static str,
    pid: Option<u32>,
    restarts: u32,
    healthy: Option<bool>,
    started: Option<Instant>,
    last_exit: Option<String>,
}

/// One instance's state as reported by `GET /flashlin/apps`.
#[derive(Debug, Serialize, serde::Deserialize, PartialEq)]
pub struct AppStatus {
    pub name: String,
    pub instance: u16,
    pub port: u16,
    pub state: String,
    pub pid: Option<u32>,
    pub restarts: u32,
    pub healthy: Option<bool>,
    pub uptime_secs: Option<u64>,
    pub last_exit: Option<String>,
}

impl Instance {
    fn update(&self, f: impl FnOnce(&mut InstanceStatus)) {
        f(&mut self.status.lock().unwrap_or_else(|e| e.into_inner()));
    }
}

impl AppManager {
    /// Spawns a supervisor per configured instance. With no `[[apps]]` this
    /// does nothing.
    pub fn start(apps: &[AppConfig]) -> Arc<Self> {
        let (stop, _) = watch::channel(false);
        let manager = Arc::new(AppManager {
            apps: apps
                .iter()
                .map(|app| {
                    let instances = (0..app.instances)
                        .map(|index| {
                            Arc::new(Instance {
                                index,
                                port: app.port + index,
                                status: Mutex::new(InstanceStatus {
                                    state: "starting",
                                    ..Default::default()
                                }),
                                restart: Notify::new(),
                            })
                        })
                        .collect();
                    (Arc::new(app.clone()), instances)
                })
                .collect(),
            stop,
            tasks: Mutex::new(Vec::new()),
        });

        let mut tasks = manager.tasks.lock().unwrap_or_else(|e| e.into_inner());
        for (app, instances) in &manager.apps {
            let log = LogSink::open(app);
            for instance in instances {
                tasks.push(tokio::spawn(supervise(
                    app.clone(),
                    instance.clone(),
                    log.clone(),
                    manager.stop.subscribe(),
                )));
            }
        }
        drop(tasks);
        manager
    }

    /// Makes this manager the one the control endpoints report on.
    pub fn register(self: &Arc<Self>) {
        if !self.apps.is_empty() {
            *REGISTRY.lock().unwrap_or_else(|e| e.into_inner()) = Some(self.clone());
        }
    }

    /// Stops every app gracefully and waits for the supervisors to finish.
    pub async fn stop(&self) {
        // send_replace stores the value even if every supervisor already exited.
        self.stop.send_replace(true);
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
        for task in tasks {
            let _ = task.await;
        }
        let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        if registry
            .as_ref()
            .is_some_and(|m| std::ptr::eq(Arc::as_ptr(m), self))
        {
            *registry = None;
        }
    }

    pub fn status(&self) -> Vec<AppStatus> {
        self.apps
            .iter()
            .flat_map(|(app, instances)| {
                instances.iter().map(move |instance| {
                    let s = instance.status.lock().unwrap_or_else(|e| e.into_inner());
                    AppStatus {
                        name: app.name.clone(),
                        instance: instance.index,
                        port: instance.port,
                        state: s.state.to_string(),
                        pid: s.pid,
                        restarts: s.restarts,
                        healthy: s.healthy,
                        uptime_secs: s.started.map(|t| t.elapsed().as_secs()),
                        last_exit: s.last_exit.clone(),
                    }
                })
            })
            .collect()
    }

    /// Restarts every instance of `name`, including a stopped or failed one.
    /// Returns false when no such app exists.
    pub fn restart(&self, name: &str) -> bool {
        let Some((_, instances)) = self.apps.iter().find(|(app, _)| app.name == name) else {
            return false;
        };
        for instance in instances {
            instance.restart.notify_one();
        }
        true
    }
}

// ─── Supervisor ──────────────────────────────────────────────────────────────

enum Outcome {
    Exited(std::io::Result<ExitStatus>),
    Unhealthy,
    RestartRequested,
    Shutdown,
}

async fn supervise(
    app: Arc<AppConfig>,
    instance: Arc<Instance>,
    log: Option<LogSink>,
    mut stop: watch::Receiver<bool>,
) {
    let stop_timeout = Duration::from_secs(parse_duration_secs(&app.stop_timeout).unwrap_or(10));
    let mut failures: u32 = 0;

    loop {
        if *stop.borrow() {
            break;
        }
        if let Some(reason) = refuse_root(&app) {
            error!(app = %app.name, instance = instance.index, "{reason}");
            instance.update(|s| {
                s.state = "failed";
                s.last_exit = Some(reason);
            });
            if !wait_for_restart(&instance, &mut stop).await {
                break;
            }
            continue;
        }
        instance.update(|s| {
            s.state = "starting";
            s.healthy = None;
        });
        let mut child = match spawn(&app, &instance) {
            Ok(child) => child,
            Err(e) => {
                error!(app = %app.name, instance = instance.index, error = %e, "App failed to start");
                instance.update(|s| s.last_exit = Some(format!("spawn failed: {e}")));
                failures += 1;
                if !backoff_or_stop(&app, &instance, &mut failures, &mut stop).await {
                    break;
                }
                continue;
            }
        };
        let pid = child.id();
        let started = Instant::now();
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(forward_output(
                stdout,
                app.clone(),
                instance.index,
                "stdout",
                log.clone(),
            ));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(forward_output(
                stderr,
                app.clone(),
                instance.index,
                "stderr",
                log.clone(),
            ));
        }
        instance.update(|s| {
            s.pid = pid;
            s.started = Some(started);
            if app.health_check.is_none() {
                s.state = "running";
            }
        });
        info!(app = %app.name, instance = instance.index, pid, port = instance.port, "App started");

        let outcome = tokio::select! {
            status = child.wait() => Outcome::Exited(status),
            _ = health_watch(&app, &instance, started) => Outcome::Unhealthy,
            _ = instance.restart.notified() => Outcome::RestartRequested,
            _ = stop.changed() => Outcome::Shutdown,
        };

        let exit = match outcome {
            Outcome::Exited(status) => {
                kill_group(pid);
                status.map_err(|e| e.to_string())
            }
            Outcome::Unhealthy => {
                warn!(app = %app.name, instance = instance.index, "App failed health checks; restarting");
                terminate(&mut child, pid, stop_timeout).await;
                Err("failed health checks".to_string())
            }
            Outcome::RestartRequested => {
                info!(app = %app.name, instance = instance.index, "App restart requested");
                terminate(&mut child, pid, stop_timeout).await;
                instance.update(|s| {
                    s.restarts += 1;
                    s.last_exit = Some("restarted on request".into());
                });
                failures = 0;
                continue;
            }
            Outcome::Shutdown => {
                info!(app = %app.name, instance = instance.index, "Stopping app");
                terminate(&mut child, pid, stop_timeout).await;
                break;
            }
        };

        let description = match &exit {
            Ok(status) => describe(status),
            Err(reason) => reason.clone(),
        };
        let succeeded = exit.as_ref().is_ok_and(ExitStatus::success);
        instance.update(|s| {
            s.pid = None;
            s.started = None;
            s.healthy = None;
            s.last_exit = Some(description.clone());
        });

        let restart = match app.restart {
            RestartPolicy::Always => true,
            RestartPolicy::OnFailure => !succeeded,
            RestartPolicy::Never => false,
        };
        if !restart {
            info!(app = %app.name, instance = instance.index, exit = %description, "App exited; not restarting (restart policy)");
            instance.update(|s| s.state = "stopped");
            if !wait_for_restart(&instance, &mut stop).await {
                break;
            }
            failures = 0;
            continue;
        }

        warn!(app = %app.name, instance = instance.index, exit = %description, "App exited; restarting");
        if started.elapsed() >= STABLE_RUN {
            failures = 0;
        }
        failures += 1;
        if !backoff_or_stop(&app, &instance, &mut failures, &mut stop).await {
            break;
        }
    }

    instance.update(|s| {
        s.state = "stopped";
        s.pid = None;
        s.started = None;
        s.healthy = None;
    });
}

/// Sleeps the crash backoff, or parks a crash-looping app as `failed` until a
/// manual restart. Returns false when the server is shutting down.
async fn backoff_or_stop(
    app: &AppConfig,
    instance: &Instance,
    failures: &mut u32,
    stop: &mut watch::Receiver<bool>,
) -> bool {
    if app.max_restarts > 0 && *failures > app.max_restarts {
        error!(
            app = %app.name,
            instance = instance.index,
            attempts = *failures,
            "App keeps crashing; giving up until `flashlin apps restart {}`",
            app.name
        );
        instance.update(|s| s.state = "failed");
        let restarted = wait_for_restart(instance, stop).await;
        *failures = 0;
        return restarted;
    }
    instance.update(|s| {
        s.state = "backoff";
        s.restarts += 1;
    });
    let delay = backoff_delay(*failures);
    tokio::select! {
        _ = tokio::time::sleep(delay) => true,
        _ = instance.restart.notified() => { *failures = 0; true }
        _ = stop.changed() => false,
    }
}

fn backoff_delay(failures: u32) -> Duration {
    BACKOFF_BASE
        .saturating_mul(1 << failures.saturating_sub(1).min(16))
        .min(BACKOFF_MAX)
}

async fn wait_for_restart(instance: &Instance, stop: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = instance.restart.notified() => true,
        _ = stop.changed() => false,
    }
}

fn describe(status: &ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exit code {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("killed by signal {signal}");
        }
    }
    status.to_string()
}

// ─── Process control ─────────────────────────────────────────────────────────

/// Program and arguments for an app; `{port}` / `$PORT` in args become the
/// instance's port so commands like gunicorn/uvicorn can bind it directly.
fn command_line(app: &AppConfig, port: u16) -> (String, Vec<String>) {
    let program = app.command.clone().unwrap_or_else(|| {
        match app.runtime {
            Some(AppRuntime::Node) => "node",
            Some(AppRuntime::Python) if cfg!(windows) => "python",
            Some(AppRuntime::Python) => "python3",
            None => "",
        }
        .to_string()
    });
    let port = port.to_string();
    let args = app
        .script
        .iter()
        .chain(&app.args)
        .map(|arg| arg.replace("{port}", &port).replace("$PORT", &port))
        .collect();
    (program, args)
}

fn spawn(app: &AppConfig, instance: &Instance) -> std::io::Result<Child> {
    let (program, args) = command_line(app, instance.port);
    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("PORT", instance.port.to_string())
        .env("HOST", "127.0.0.1")
        .env("FLASHLIN_APP", &app.name)
        .env("FLASHLIN_INSTANCE", instance.index.to_string());
    match app.runtime {
        Some(AppRuntime::Node) => {
            cmd.env("NODE_ENV", "production");
        }
        // Unbuffered output so log lines arrive as they are printed.
        Some(AppRuntime::Python) => {
            cmd.env("PYTHONUNBUFFERED", "1");
        }
        None => {}
    }
    cmd.envs(&app.env);
    if let Some(cwd) = &app.cwd {
        cmd.current_dir(cwd);
    }
    #[cfg(unix)]
    {
        // Own process group, so stop signals reach children an app forks
        // (`npm start` -> node) and a terminal Ctrl-C reaches only FlashLin,
        // which then stops apps in order.
        cmd.process_group(0);
        if let Some(user) = &app.user {
            let (uid, gid) = lookup_user(user)?;
            // std also clears supplementary groups when switching uid.
            cmd.uid(uid).gid(gid);
        }
    }
    #[cfg(target_os = "linux")]
    {
        // If FlashLin dies without stopping its apps (SIGKILL, OOM killer), the
        // kernel sends each app SIGTERM instead of leaving it orphaned on its
        // port. The signal is tied to the spawning thread; spawns run on Tokio
        // worker threads, which live as long as the runtime (nothing here uses
        // block_in_place, which could retire one early).
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: the closure only makes async-signal-safe calls (prctl,
        // getppid) and does not allocate. std runs it after switching uid/gid,
        // which would otherwise clear the setting.
        unsafe {
            cmd.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                // FlashLin may have died between fork and prctl.
                if libc::getppid() != parent {
                    return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    cmd.spawn()
}

/// FlashLin usually runs as root to bind ports 80/443; its apps should not
/// silently inherit that. Running one as root needs an explicit `user = "root"`.
fn refuse_root(app: &AppConfig) -> Option<String> {
    #[cfg(unix)]
    // SAFETY: geteuid() takes no arguments and cannot fail.
    if app.user.is_none() && unsafe { libc::geteuid() } == 0 {
        return Some(format!(
            "Refusing to run app '{}' as root: set user = \"www-data\" (or another \
             unprivileged account) in its [[apps]] entry, or user = \"root\" to allow it",
            app.name
        ));
    }
    #[cfg(not(unix))]
    let _ = app;
    None
}

#[cfg(unix)]
fn lookup_user(name: &str) -> std::io::Result<(u32, u32)> {
    let c_name = std::ffi::CString::new(name)?;
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: zeroed passwd is a valid out-parameter; getpwnam_r writes into
    // it and `buffer`, both of which outlive the call.
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwnam_r(
            c_name.as_ptr(),
            &mut passwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("unknown user '{name}'"),
        ));
    }
    Ok((passwd.pw_uid, passwd.pw_gid))
}

/// SIGTERM the app's process group, then SIGKILL whatever is left after
/// `timeout`. On Windows the process is terminated directly.
async fn terminate(child: &mut Child, pid: Option<u32>, timeout: Duration) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // SAFETY: kill(2) on our own child's process group; the leader has
        // not been reaped yet, so its pid (and pgid) cannot have been reused.
        unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGTERM) };
        if tokio::time::timeout(timeout, child.wait()).await.is_ok() {
            return;
        }
        unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = (pid, timeout);
    let _ = child.kill().await;
}

/// After the group leader exits on its own, kill any children it left behind
/// so they cannot keep holding the app's port across the restart.
fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // SAFETY: plain kill(2); ESRCH (group already empty) is the common case.
        unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = pid;
}

// ─── Health checks ───────────────────────────────────────────────────────────

/// Resolves once the instance should be restarted for failing its health
/// check: 3 consecutive failures after it was healthy once, or no success
/// within `start_timeout`. Never resolves when no health check is configured.
async fn health_watch(app: &AppConfig, instance: &Instance, started: Instant) {
    let Some(path) = app.health_check.as_deref() else {
        return std::future::pending().await;
    };
    let interval = Duration::from_secs(parse_duration_secs(&app.health_interval).unwrap_or(10));
    let grace = Duration::from_secs(parse_duration_secs(&app.start_timeout).unwrap_or(30));
    let mut seen_healthy = false;
    let mut failures = 0;
    loop {
        // Poll quickly until the app first answers, then at the configured rate.
        let wait = if seen_healthy {
            interval
        } else {
            Duration::from_millis(500)
        };
        tokio::time::sleep(wait).await;
        let healthy = probe(instance.port, path).await;
        instance.update(|s| {
            s.healthy = Some(healthy);
            if healthy {
                s.state = "running";
            }
        });
        if healthy {
            seen_healthy = true;
            failures = 0;
            continue;
        }
        failures += 1;
        if (seen_healthy && failures >= HEALTH_FAILURES)
            || (!seen_healthy && started.elapsed() >= grace)
        {
            return;
        }
    }
}

/// One HTTP/1.1 GET against the app on loopback; 2xx/3xx is healthy.
async fn probe(port: u16, path: &str) -> bool {
    let attempt = async {
        let mut stream = crate::server::net::connect_tcp(("127.0.0.1", port)).await?;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
             User-Agent: FlashLin-HealthCheck\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await?;
        let mut head = [0u8; 32];
        let mut read = 0;
        while read < 12 {
            let n = stream.read(&mut head[read..]).await?;
            if n == 0 {
                break;
            }
            read += n;
        }
        Ok::<_, std::io::Error>(status_is_healthy(&head[..read]))
    };
    matches!(
        tokio::time::timeout(Duration::from_secs(5), attempt).await,
        Ok(Ok(true))
    )
}

fn status_is_healthy(head: &[u8]) -> bool {
    // "HTTP/1.1 200 ..." — status code is bytes 9..12.
    head.starts_with(b"HTTP/1.")
        && head.len() >= 12
        && matches!(head[9], b'2' | b'3')
        && head[10].is_ascii_digit()
        && head[11].is_ascii_digit()
}

// ─── Output forwarding ───────────────────────────────────────────────────────

/// Per-app log file shared by its instances; without one, output goes to
/// FlashLin's own log through tracing.
#[derive(Clone)]
struct LogSink(Arc<tokio::sync::Mutex<tokio::fs::File>>);

impl LogSink {
    fn open(app: &AppConfig) -> Option<Self> {
        let path = app.log_file.as_deref()?;
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(file) => Some(LogSink(Arc::new(tokio::sync::Mutex::new(
                tokio::fs::File::from_std(file),
            )))),
            Err(e) => {
                warn!(app = %app.name, path, error = %e, "Cannot open app log file; logging to FlashLin's log instead");
                None
            }
        }
    }
}

async fn forward_output(
    reader: impl AsyncRead + Unpin,
    app: Arc<AppConfig>,
    instance: u16,
    stream: &'static str,
    sink: Option<LogSink>,
) {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::with_capacity(256);
    loop {
        let buffer = match reader.fill_buf().await {
            Ok([]) | Err(_) => break,
            Ok(buffer) => buffer,
        };
        let (chunk, complete) = match buffer.iter().position(|&b| b == b'\n') {
            Some(end) => (&buffer[..=end], true),
            None => (buffer, false),
        };
        let room = MAX_LOG_LINE.saturating_sub(line.len());
        line.extend_from_slice(&chunk[..chunk.len().min(room)]);
        let consumed = chunk.len();
        reader.consume(consumed);
        if complete {
            emit(&line, &app.name, instance, stream, sink.as_ref()).await;
            line.clear();
        }
    }
    if !line.is_empty() {
        emit(&line, &app.name, instance, stream, sink.as_ref()).await;
    }
}

async fn emit(line: &[u8], app: &str, instance: u16, stream: &str, sink: Option<&LogSink>) {
    let text = String::from_utf8_lossy(line);
    let text = text.trim_end_matches(['\r', '\n']);
    match sink {
        Some(LogSink(file)) => {
            let entry = format!(
                "{} {app}[{instance}] {stream}: {text}\n",
                chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z")
            );
            let _ = file.lock().await.write_all(entry.as_bytes()).await;
        }
        None => info!(app, instance, stream, "{text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(toml_src: &str) -> AppConfig {
        toml::from_str(toml_src).unwrap()
    }

    #[test]
    fn command_line_uses_runtime_script_and_port_placeholders() {
        let node = app("name = \"web\"\nruntime = \"node\"\nscript = \"server.js\"\nport = 3000");
        assert_eq!(
            command_line(&node, 3000),
            ("node".into(), vec!["server.js".into()])
        );

        let gunicorn = app("name = \"api\"\ncommand = \"gunicorn\"\nport = 8000\n\
             args = [\"app:app\", \"--bind\", \"127.0.0.1:{port}\", \"--x=$PORT\"]");
        let (program, args) = command_line(&gunicorn, 8001);
        assert_eq!(program, "gunicorn");
        assert_eq!(args, ["app:app", "--bind", "127.0.0.1:8001", "--x=8001"]);
    }

    #[test]
    fn backoff_grows_exponentially_and_is_capped() {
        assert_eq!(backoff_delay(1), Duration::from_millis(500));
        assert_eq!(backoff_delay(2), Duration::from_secs(1));
        assert_eq!(backoff_delay(4), Duration::from_secs(4));
        assert_eq!(backoff_delay(10), BACKOFF_MAX);
        assert_eq!(backoff_delay(u32::MAX), BACKOFF_MAX);
    }

    #[test]
    fn health_status_line_parsing() {
        assert!(status_is_healthy(b"HTTP/1.1 200 OK\r\n"));
        assert!(status_is_healthy(b"HTTP/1.0 302 Found"));
        assert!(!status_is_healthy(b"HTTP/1.1 503 Service Unavailable"));
        assert!(!status_is_healthy(b"HTTP/1.1 404"));
        assert!(!status_is_healthy(b"garbage"));
        assert!(!status_is_healthy(b"HTTP/1.1 2"));
    }

    #[tokio::test]
    async fn output_lines_are_bounded() {
        let long = vec![b'x'; MAX_LOG_LINE * 3];
        let mut input = long.clone();
        input.extend_from_slice(b"\nshort\n");
        // Only checks that forwarding a newline-free flood terminates and does
        // not panic; the bound is enforced by `room` in forward_output.
        let app = Arc::new(app("name = \"t\"\ncommand = \"x\"\nport = 1"));
        forward_output(&input[..], app, 0, "stdout", None).await;
    }
}
