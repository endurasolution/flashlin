# Running Node.js and Python apps with FlashLin

FlashLin 3.0 can start your backend applications, keep them running, and put
them behind its own HTTP/1.1, HTTP/2, HTTP/3, TLS, compression, rate limiting
and security headers. You don't need PM2, supervisord or a separate systemd unit
per app.

```toml
[[apps]]
name         = "api"
runtime      = "node"            # "node" or "python"; or set `command`
script       = "server.js"
cwd          = "/srv/api"
port         = 3000              # FlashLin passes this to the app as $PORT
health_check = "/health"
user         = "www-data"        # required when FlashLin runs as root

[[routes]]
path = "/api"
app  = "api"                     # proxy /api/* to the app
```

Validate it, then start FlashLin as usual:

```bash
flashlin --check /etc/flashlin/server.toml
sudo systemctl restart flashlin
flashlin apps
```

```
NAME                 INST   PORT  STATE          PID RESTARTS  HEALTH       UPTIME  LAST EXIT
api                     0   3000  running      41237        0  healthy       2m14s  -
```

## What FlashLin does for each app

| | |
|---|---|
| **Start** | Every `[[apps]]` entry is launched as the server starts. FlashLin doesn't wait for an app to become ready, so requests that arrive while it's still booting get a 502. |
| **Port** | The app gets `PORT`, `HOST=127.0.0.1`, `FLASHLIN_APP` and `FLASHLIN_INSTANCE` in its environment. Listen on `process.env.PORT` / `os.environ["PORT"]`. |
| **Connect** | A route with `app = "<name>"` proxies to the app over loopback. WebSockets, `X-Forwarded-For` / `-Proto` / `-Host`, streaming responses and QUERY all work as they do for `proxy_pass`. |
| **Scale** | `instances = N` runs N copies on `port`, `port + 1`, … and load-balances across them round-robin. A failing instance is skipped for 5 seconds (passive failover). |
| **Restart** | A crashed app restarts after 0.5 s, 1 s, 2 s, … up to 30 s between attempts. A run longer than 60 s resets the delay. After `max_restarts` consecutive crashes (default 10) the app is marked `failed` until you run `flashlin apps restart <name>`. |
| **Health** | With `health_check = "/path"`, FlashLin sends `GET /path` on loopback (every 0.5 s until the first success, then every `health_interval`). A 2xx/3xx answer is healthy. It restarts the app after 3 consecutive failures, or if the app hasn't answered once within `start_timeout`. |
| **Logs** | stdout/stderr lines go to FlashLin's own log (journald under systemd), tagged with the app name. Set `log_file` to append them to a file instead. |
| **Stop** | On shutdown FlashLin first drains client connections, then sends SIGTERM to each app's whole process group. Whatever is still running after `stop_timeout` gets SIGKILL. |

## Examples

### Node.js (Express, Fastify, Next.js, plain `http`)

```js
// server.js
const express = require("express");
const app = express();
app.get("/health", (req, res) => res.send("ok"));
app.get("/api/hello", (req, res) => res.json({ hello: "world" }));
const server = app.listen(process.env.PORT, process.env.HOST);

// Stop promptly on FlashLin's SIGTERM. FlashLin's proxy keeps idle
// keep-alive connections open, and server.close() alone waits ~5 s for them.
process.on("SIGTERM", () => {
  server.close(() => process.exit(0));
  server.closeIdleConnections();
});
```

```toml
[[apps]]
name         = "api"
runtime      = "node"            # sets NODE_ENV=production unless env overrides it
script       = "server.js"
cwd          = "/srv/api"
port         = 3000
instances    = 2                 # use your CPU cores; Node is single-threaded
health_check = "/health"
user         = "www-data"
env          = { DATABASE_URL = "postgres://localhost/app" }
```

To run an npm script instead, set `command = "npm"` and `args = ["start"]`. On
Windows that is `command = "npm.cmd"`.

### Python: FastAPI / Starlette (uvicorn)

```toml
[[apps]]
name         = "fastapi"
command      = "/srv/fastapi/.venv/bin/uvicorn"
args         = ["main:app", "--host", "127.0.0.1", "--port", "{port}", "--proxy-headers"]
cwd          = "/srv/fastapi"
port         = 8000
health_check = "/health"
user         = "www-data"
```

`{port}` (or `$PORT`) in `args` becomes the instance's port. FlashLin starts
commands directly, not through a shell, so shell syntax like `&&` or `>` is not
interpreted.

### Python: Flask / Django (gunicorn)

```toml
[[apps]]
name    = "django"
command = "/srv/site/.venv/bin/gunicorn"
args    = ["site.wsgi", "--bind", "127.0.0.1:{port}", "--workers", "4"]
cwd     = "/srv/site"
port    = 8100
user    = "www-data"

[[routes]]
path = "/"
app  = "django"

[[routes]]                       # static files served by FlashLin itself
path          = "/static"
root          = "/srv/site/staticfiles"
cache_control = "public, max-age=31536000, immutable"
```

### Python: a plain script

```toml
[[apps]]
name    = "worker-api"
runtime = "python"               # python3 on Linux/macOS, python on Windows;
script  = "app.py"               # sets PYTHONUNBUFFERED=1 so logs arrive promptly
port    = 8200
```

### Per-domain apps

Routes in `domains/*.toml` can use `app = "..."` too, so each virtual host can
front a different app. The `[[apps]]` themselves are always declared in
`server.toml`.

```toml
# domains/shop.example.com.toml
[domain]
name = "shop.example.com"

[[routes]]
path = "/"
app  = "shop"
```

## Reference

| Key | Default | Meaning |
|---|---|---|
| `name` | required | Letters, digits, `-`, `_`. Used by `app = "..."` and `flashlin apps restart`. |
| `runtime` | — | `"node"` or `"python"`. Picks the interpreter and sets `NODE_ENV=production` / `PYTHONUNBUFFERED=1`. |
| `command` | interpreter | Program to run, e.g. a virtualenv's `python`, `gunicorn`, `npm`. Overrides `runtime`'s interpreter. |
| `script` | — | First argument, e.g. `server.js`, `app.py`. |
| `args` | `[]` | Further arguments; `{port}` / `$PORT` are substituted. |
| `cwd` | FlashLin's | Working directory. Relative paths in `script`/`args` resolve from here. |
| `env` | `{}` | Extra environment variables (FlashLin's environment is inherited). |
| `port` | required | Loopback port of instance 0; instance N uses `port + N`. |
| `instances` | `1` | 1–64 copies, load-balanced. |
| `restart` | `"always"` | `"always"`, `"on-failure"` (not after exit code 0) or `"never"`. |
| `max_restarts` | `10` | Consecutive crashes before giving up; `0` = retry forever. |
| `health_check` | — | Loopback HTTP path to probe. |
| `health_interval` | `"10s"` | Probe interval once healthy. |
| `start_timeout` | `"30s"` | Time allowed to pass the first health check. |
| `stop_timeout` | `"10s"` | SIGTERM → SIGKILL grace period. |
| `log_file` | — | Append stdout/stderr here instead of FlashLin's log. |
| `user` | — | Unix account to run as. Required when FlashLin runs as root, see below. |

## Control commands

```bash
flashlin apps [config]                  # status of every instance
flashlin apps restart <name> [config]   # graceful restart (also revives a failed app)
```

These talk to the running server over the same loopback control endpoint as
`flashlin purge cache` (`GET /flashlin/apps`, `POST /flashlin/apps/<name>/restart`).
See [Control endpoint security](#control-endpoint-security).

Changing `[[apps]]` needs a restart. With `--watch`, FlashLin rejects a reload
that changes apps and keeps the running configuration, the same as it does for
listener changes. Routes that point at existing apps can be changed live.

## Security

- **Never as root by accident.** `production/install.sh` runs FlashLin as root so
  it can bind ports 80/443. FlashLin will not start an app as root unless you
  say so: set `user = "www-data"` (or a dedicated account), or `user = "root"` to
  opt in. When switching user, FlashLin also drops root's supplementary groups.
- **Keep apps on loopback.** FlashLin passes `HOST=127.0.0.1`. Bind to it. An app
  that listens on `0.0.0.0` is also reachable directly on its port, bypassing
  FlashLin's rate limits, IP rules and TLS, unless your firewall blocks it.
- **Process groups.** Each app runs in its own process group, so a stop also
  reaches children it spawned (`npm start` → `node`). Leftover children of a
  crashed app are killed before it restarts, so they can't keep holding its port.
- **No orphans if FlashLin is killed.** On Linux each app is set to receive
  SIGTERM if FlashLin dies without stopping it (SIGKILL, OOM killer), so it
  can't keep holding its port and break the next start. Under systemd the
  unit's cgroup also cleans up.
- **Bounded logs.** Output lines longer than 16 KiB are truncated, so an app
  printing without newlines can't grow FlashLin's memory.
- **Health checks are loopback-only.** The check path is validated at load time,
  so a config value can't inject headers into the probe request.

### Control endpoint security

Requests to `/flashlin/*` must:

1. come from a loopback address,
2. carry none of `Forwarded`, `X-Forwarded-For`, `X-Real-IP` or `Via`, so a
   reverse proxy or tunnel on the same machine (nginx, Cloudflare Tunnel, …)
   can't relay internet traffic to them, and
3. if `server.control_token` is set, send `Authorization: Bearer <token>`. The
   CLI reads the token from the same config file. Keep that file `chmod 640`.

```toml
[server]
control_token = "a-long-random-string-at-least-16-chars"
```

## Systemd

The unit in `production/flashlin.service` sets `KillMode=mixed`. With it,
`systemctl stop flashlin` signals FlashLin only, and FlashLin stops the apps in
order. Without it, systemd signals every app at the same moment it signals
FlashLin, while requests may still be in flight. If you wrote your own unit,
add:

```ini
KillMode=mixed
TimeoutStopSec=60
```

## Windows

Apps work on Windows for development. `runtime = "python"` runs `python`, and
npm needs `command = "npm.cmd"`. Stopping uses TerminateProcess on the app
process. Unlike on Unix, that doesn't reach child processes the app started
itself, so prefer running `node` / `python` directly over wrappers like `npm`.

Stop FlashLin with Ctrl-C so it can stop its apps. If FlashLin is killed
abruptly (Task Manager, `taskkill /F`), its apps keep running. Find their PIDs
with `flashlin apps` before stopping it, or look them up in Task Manager.
