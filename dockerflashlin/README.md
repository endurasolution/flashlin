<div align="center">
  <a href="https://www.flashlin.com/">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://www.flashlin.com/flashlin-lightlogo.png">
      <source media="(prefers-color-scheme: light)" srcset="https://www.flashlin.com/flashlin-darklogo.png">
      <img src="https://www.flashlin.com/flashlin-darklogo.png" alt="FlashLin logo" width="260">
    </picture>
  </a>
</div>

# FlashLin Docker

Fast, Cached, and Powerful Rust Web Server — container-ready with HTTP/3, automatic HTTPS, TLS, reverse proxy, caching, compression, PHP-FPM, and supervised Node.js / Python apps.

[![FlashLin](https://img.shields.io/badge/FlashLin-v3.1.0-00d2d2?style=for-the-badge)](https://www.flashlin.com/)
[![Docker Pulls](https://img.shields.io/docker/pulls/endurasolution/flashlin?style=for-the-badge&logo=docker)](https://hub.docker.com/r/endurasolution/flashlin)
[![Rust](https://img.shields.io/badge/Rust-Tokio%20%2B%20Hyper-orange?style=for-the-badge&logo=rust)](https://www.rust-lang.org/)
[![HTTP/3](https://img.shields.io/badge/HTTP%2F3-QUIC-7B42BC?style=for-the-badge)](https://www.flashlin.com/docs)
[![TLS](https://img.shields.io/badge/TLS-rustls-777BB4?style=for-the-badge)](https://www.flashlin.com/docs)
[![License](https://img.shields.io/badge/License-CC%20BY--NC%204.0-lightgrey?style=for-the-badge)](https://creativecommons.org/licenses/by-nc/4.0/)

[Website](https://www.flashlin.com/) · [Documentation](https://www.flashlin.com/docs) · [GitHub](https://github.com/endurasolution/flashlin) · [Docker Hub](https://hub.docker.com/r/endurasolution/flashlin)

---

## Overview

**FlashLin Docker** is the containerized deployment of **FlashLin**, a high-performance Rust web server built on Tokio and Hyper.

It provides a production-oriented container environment for static websites, HTTPS, reverse proxying, WebSockets, PHP-FPM, Node.js and Python app hosting, compression, caching, security features, metrics, and optional HTTP/3 support.

### Features

- 🚀 High-performance Rust web server, memory-safe Rust
- ⚡ Tokio + Hyper async runtime
- 🌐 HTTP/1.1 + HTTP/2 by default
- 🛜 HTTP/3 over QUIC — optional, experimental listener (image is built with `acme,http3`)
- 🔒 Automatic HTTPS via Let's Encrypt (ACME) — cached accounts, auto-renewal, no restart on renew
- 🔐 TLS via rustls — no OpenSSL dependency
- 🔁 Reverse proxy with round-robin upstreams, passive failover, and WebSocket upgrade forwarding
- 🐘 PHP-FPM / FastCGI hosting alongside static files
- 🟢 Supervised Node.js / Python / any-command apps (`[[apps]]`): auto-restart, health checks, load balancing
- 📦 Gzip + Brotli compression, precompressed `.br`/`.gz` sidecar negotiation
- 🗄️ In-memory response cache (RAM, TTL-based)
- 🏷️ ETag / Last-Modified / 304 Not Modified, byte-range requests
- 🛡️ Security headers (CSP, HSTS, X-Frame-Options, …) + per-host rate limiting (IPv6 /64-aware)
- 🚧 Per-client connection limit (`max_connections_per_ip`)
- 📊 Prometheus metrics at `/metrics` (loopback-only)
- 🧩 Virtual hosts — one `.toml` file per domain, no central config edits
- ♻️ Validated hot config reload (`--watch` / `FLASHLIN_WATCH=1`) — no dropped connections
- 🧹 In-container cache purge via `flashlin purge cache`, and app control via `flashlin apps` (loopback HTTP, works through `docker exec`)
- 🐳 Four ready-made Compose modes: static HTTP, static HTTPS, PHP HTTP, PHP HTTPS

### 🆕 New in 3.1

| Feature | Notes |
|---|---|
| RealWeb realtime hub | `[realweb]` in `server.toml`: browsers connect to FlashLin over WebSocket (`realweb.v1`); your app signs a JWT per user and publishes with `POST /flashlin/realweb/publish`. Signed expiring tokens, Origin checks, per-channel permissions, resume after reconnect, acknowledged publishes, presence, heartbeats, slow-client and flood limits. JS client served at `/realweb/client.js`. [Guide](https://github.com/endurasolution/flashlin/blob/master/docs/REALWEB.md) |
| `flashlin realweb` | `token`, `publish` and `status` subcommands, through `docker exec` |
| WebSocket proxy | Unchanged: upgrades are still forwarded to your backend through `proxy_pass` / `app` |

### Added in 3.0

| Feature | Notes |
|---|---|
| Supervised Node.js / Python apps | `[[apps]]` in `server.toml`: FlashLin starts your app, passes it `$PORT`, restarts it on crash, health-checks it and proxies a route to it with `app = "<name>"`. See [Node.js and Python apps](#nodejs-and-python-apps) |
| `flashlin apps` | App status and graceful restarts, through `docker exec` |
| Per-client connection limit | `server.max_connections_per_ip`, enforced before any TLS/HTTP work |
| Security fixes | `allow_ips` / `deny_ips` now match IPv4 clients on dual-stack listeners; rate limiting groups IPv6 by /64; control endpoints refuse proxied requests and accept an optional `control_token` |
| Proxy fix | Responses from HTTP/1.0 backends are served as HTTP/1.1, keeping client keep-alive |

Full list: [CHANGELOG](https://github.com/endurasolution/flashlin/blob/master/CHANGELOG.md).

### Added in 2.0

| Feature | Notes |
|---|---|
| HTTP/3 over QUIC | Optional listener, image built with `--features http3`. Experimental — see [HTTP/3](#http3-quic--optional-experimental) |
| Automatic HTTPS (ACME) | Image built with `--features acme`. Requests and renews Let's Encrypt certificates itself |
| WebSocket proxy | HTTP/1 upgrade tunnels forwarded to your backend through `proxy_pass` |
| Reverse proxy failover | Round-robin upstream groups with passive failure cooldown |
| Validated hot reload | `--watch` / `FLASHLIN_WATCH=1` applies a fully validated config snapshot with no dropped connections |
| Streaming, byte ranges, precompressed assets | 64 KiB streamed reads, single/suffix ranges, `If-Range` |
| In-container cache purge | `flashlin purge cache` via `docker exec` — see [Useful commands](#useful-commands) |
| Per-virtual-host cache & rate-limit state | Each domain's `.toml` gets independent cache and rate-limit tracking |

---

## Repository overview

`dockerflashlin/` is one part of the main [flashlin](https://github.com/endurasolution/flashlin) repository — the Rust source itself, not a separate project:

```text
flashlin/
├── src/                      # Rust source
│   ├── main.rs                 # Entry point, config loading, banner
│   ├── apps/                    # [[apps]] supervisor for Node.js / Python / commands
│   ├── server/                  # TCP listener, worker pool, graceful shutdown
│   ├── http/                    # Request parsing, response helpers, router
│   ├── config/                  # TOML parser + validator
│   ├── cache/                   # In-memory response cache (DashMap + TTL)
│   ├── static_files/            # File serving, ETag, cache headers, path security
│   ├── php/                     # PHP-FPM + FastCGI protocol implementation
│   ├── proxy/                   # Reverse proxy + upstream management
│   ├── realweb/                 # RealWeb realtime hub + JS client
│   ├── tls/                     # rustls config + certificate loader
│   ├── security/                # Rate limiter, IP ACL, security headers
│   ├── middleware/               # Compression, request-ID, access logging
│   ├── metrics/                  # Prometheus counters + exposition
│   └── tests/                     # Integration tests
├── server.toml                # Full configuration reference
├── domains/                   # Example virtual host configs
├── examples/                  # Example configs (HTTP/3, ACME, performance, …)
├── docs/                      # APPS.md, HTTP_PROTOCOLS.md, PRODUCTION.md, PERFORMANCE.md, …
├── production/                # Self-contained per-distro deploy packages
│   ├── centos/  debian/  raspberry-pi/  universal/
│   └── install.sh, install_linux_cdn.sh, systemd unit, …
├── dockerflashlin/            # ← you are here — Docker & Compose setup
│   ├── Dockerfile, Dockerfile.prebuilt
│   ├── docker-compose*.yml
│   ├── config/, certs/, public/
│   └── README.md (this file)
└── .github/workflows/         # CI — tests, and build + publish this Docker image
```

Building the image (below) needs the **repository root** as its build context, not this folder — the Dockerfile compiles `src/` directly. Everything else on this page (Compose files, TLS certs, config mounts) only touches files inside `dockerflashlin/` itself.

---

## Docker image

```bash
docker pull endurasolution/flashlin:3.1.0
```

Also available as `endurasolution/flashlin:3.1`, `endurasolution/flashlin:3` and `endurasolution/flashlin:latest`. Pin to the full
`3.1.0` tag for anything you deploy — `latest` will move. Currently published for **linux/amd64**.
Previous releases stay available as `3.0.0` and `2.0.0`.

---

## Highlights

| Feature | Status |
|---|---|
| Rust web server (Tokio + Hyper) | Yes |
| HTTP/1.1 + HTTP/2 | Yes |
| HTTP/3 over QUIC | Optional listener — see [HTTP/3](#http3-quic--optional-experimental) |
| Automatic HTTPS (Let's Encrypt / ACME) | Yes — built with `acme` feature |
| Manual TLS certificates | Yes — rustls, no OpenSSL |
| PHP-FPM / FastCGI | Yes |
| Node.js / Python app supervisor | Yes — bring the runtime, see [below](#nodejs-and-python-apps) |
| Reverse proxy + WebSocket upgrades | Yes |
| Gzip + Brotli compression | Yes |
| ETag / Last-Modified / 304 caching | Yes |
| Security headers + rate limiting | Yes |
| Prometheus metrics (loopback-only) | Yes |
| In-container cache purge | Yes — `flashlin purge cache` |
| App status / restart | Yes — `flashlin apps` |
| Docker Compose modes (HTTP/TLS/PHP) | Yes |

---

## Directory structure

```text
dockerflashlin/
├── Dockerfile                  # Multi-stage build from source (default)
├── Dockerfile.prebuilt         # Installs the released binary via install_linux.sh
├── docker-compose.yml          # HTTP on port 80
├── docker-compose.tls.yml      # HTTPS on 443 + HTTP 80→443 redirect
├── docker-compose.php.yml      # PHP-FPM + HTTP port 80
├── docker-compose.php.tls.yml  # PHP-FPM + HTTPS port 443
├── entrypoint.sh
├── config/
│   ├── server.toml             # HTTP, port 80
│   ├── server.tls.toml         # HTTPS, port 443
│   ├── server.php.toml         # PHP-FPM + HTTP
│   └── server.php.tls.toml     # PHP-FPM + HTTPS
├── certs/                      # Mount your TLS certs here
└── public/
    ├── index.html               # Example static page
    └── index.php                # Example PHP page
```

---

## Quick start

### Plain `docker run`

```bash
docker run -d --name flashlin -p 80:80 endurasolution/flashlin:3.1.0
```

Open `http://localhost`. Then:

```bash
docker logs -f flashlin      # tail logs
docker stop flashlin         # stop
docker rm flashlin           # remove
```

### With Docker Compose

```bash
cd dockerflashlin
docker compose up -d              # HTTP, port 80
docker compose logs -f flashlin
docker compose down
```

---

## Compose modes

| Mode | Compose file | Ports | PHP | TLS |
|---|---|---|---|---|
| Static HTTP | `docker-compose.yml` | 80 | No | No |
| Static HTTPS | `docker-compose.tls.yml` | 80, 443 | No | Yes |
| PHP HTTP | `docker-compose.php.yml` | 80 | Yes | No |
| PHP HTTPS | `docker-compose.php.tls.yml` | 80, 443 | Yes | Yes |

`docker-compose.yml` — static HTTP:

```yaml
services:
  flashlin:
    image: endurasolution/flashlin:3.1.0
    container_name: flashlin
    restart: unless-stopped
    ports:
      - "80:80"
    volumes:
      - ./public:/var/www/html:ro
      - ./config/server.toml:/etc/flashlin/server.toml:ro
      - flashlin_logs:/var/log/flashlin
    environment:
      RUST_LOG: info
      FLASHLIN_CONFIG: /etc/flashlin/server.toml

volumes:
  flashlin_logs:
```

The `tls` and `php` variants follow the same shape — swap the config file mounted at
`/etc/flashlin/server.toml`, add `443:443` for TLS, and add the `php-fpm` service for PHP
(see [PHP-FPM](#php-fpm) below).

---

## TLS / HTTPS setup

FlashLin expects certificates mounted at:

```text
dockerflashlin/certs/
├── fullchain.pem    # certificate + intermediate chain
└── privkey.pem      # private key
```

### Option A — Let's Encrypt (recommended for production)

```bash
# On the host:
certbot certonly --standalone -d yourdomain.com

mkdir -p certs
cp /etc/letsencrypt/live/yourdomain.com/fullchain.pem certs/
cp /etc/letsencrypt/live/yourdomain.com/privkey.pem   certs/
```

### Option B — self-signed (local / dev only)

```bash
mkdir -p certs
openssl req -x509 -newkey rsa:4096 -sha256 -days 365 -nodes \
  -keyout certs/privkey.pem \
  -out    certs/fullchain.pem \
  -subj   "/CN=localhost"
```

### Start with TLS

```bash
docker compose -f docker-compose.tls.yml up -d
```

FlashLin listens on 443 for HTTPS and 80 for HTTP, redirecting all HTTP traffic to HTTPS.

### Renewing Let's Encrypt certificates

```bash
certbot renew
cp /etc/letsencrypt/live/yourdomain.com/fullchain.pem certs/
cp /etc/letsencrypt/live/yourdomain.com/privkey.pem   certs/
docker compose -f docker-compose.tls.yml restart flashlin
```

> Prefer not managing certbot yourself? Build with the `acme` feature (already on for the published
> image) and let FlashLin request and renew Let's Encrypt certificates itself — see
> [Domain configuration and HTTPS](https://www.flashlin.com/docs/domain-configuration).

---

## HTTP/3 (QUIC) — optional, experimental

The published image is built with `acme,http3`. After TLS is configured, enable it in the mounted
`server.toml`:

```toml
[http3]
enabled = true
port = 443
max_concurrent_streams = 32
```

Add `"443:443/udp"` to the service's `ports` list alongside the existing TCP mapping, and allow UDP
443 through your host firewall. HTTP/1.1 and HTTP/2 continue to work over TCP 443 unaffected. This
listener is explicitly experimental — see
[HTTP/1.1, HTTP/2 and HTTP/3](https://www.flashlin.com/docs/http-protocols) for current limits.

---

## Building the image

### From source (default)

Build context is the **flashlin project root**, not `dockerflashlin/`:

```bash
# From the project root:
docker build -f dockerflashlin/Dockerfile -t flashlin .

# Or let Compose handle it:
cd dockerflashlin && docker compose build
```

### Pre-built binary via the official install script

Build context is `dockerflashlin/` itself:

```bash
cd dockerflashlin
docker build -f Dockerfile.prebuilt -t flashlin .
```

---

## Node.js and Python apps

FlashLin 3.0 can run your backend inside the same container: it starts the app, passes it
`PORT` / `HOST=127.0.0.1`, restarts it if it crashes, health-checks it, and proxies a route to it.
The published image contains only FlashLin, so add the runtime your app needs:

```dockerfile
# Dockerfile
FROM endurasolution/flashlin:3.1.0

USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends nodejs python3 \
    && rm -rf /var/lib/apt/lists/*
USER 1001

COPY apps/ /srv/apps/
COPY server.toml /etc/flashlin/server.toml
```

```toml
# server.toml
[server]
host = "0.0.0.0"
port = 80
root = "/var/www/html"

[[apps]]
name         = "api"
runtime      = "node"                       # or "python"; or command = "gunicorn", args = [...]
script       = "/srv/apps/node-api/server.js"
port         = 3000
instances    = 2                            # ports 3000 and 3001, load-balanced
health_check = "/health"

[[routes]]
path = "/"
root = "/var/www/html"

[[routes]]
path = "/api"
app  = "api"
```

```bash
docker build -t my-flashlin-app .
docker run -d --name flashlin -p 80:80 --stop-timeout 30 my-flashlin-app
docker exec flashlin flashlin apps                # state, pid, restarts, health
docker exec flashlin flashlin apps restart api    # graceful restart
```

- The container already runs as the unprivileged user `1001`, so apps need no `user = ...` setting.
- On `docker stop`, FlashLin drains connections, then sends SIGTERM to each app. Give it time:
  `--stop-timeout` / `stop_grace_period` at least as long as the apps' `stop_timeout` (default 10s).
- Node apps should close idle keep-alive connections on SIGTERM (`server.closeIdleConnections()`),
  or `server.close()` waits ~5 s for them.
- Example apps and the full reference: [docs/APPS.md](https://github.com/endurasolution/flashlin/blob/master/docs/APPS.md).

---

## Configuration

### Config files

| Config file | Port | TLS | PHP |
|---|---|---|---|
| `server.toml` | 80 | No | No |
| `server.tls.toml` | 443 | Yes | No |
| `server.php.toml` | 80 | No | Yes |
| `server.php.tls.toml` | 443 | Yes | Yes |

### Environment variables

| Variable | Default | Description |
|---|---|---|
| `WEBROOT` | `./public` | Host directory mounted as the webroot (Compose only) |
| `RUST_LOG` | `info` | Log level: `error`, `warn`, `info`, `debug`, `trace` |
| `FLASHLIN_CONFIG` | `/etc/flashlin/server.toml` | Config path inside the container |
| `FLASHLIN_WATCH` | unset | Set to `1` to hot-reload config on change instead of a static start |

### Custom webroot

```bash
WEBROOT=/srv/mysite docker compose -f docker-compose.tls.yml up -d
```

### Mount everything explicitly

```bash
docker run -d \
  --name flashlin \
  -p 80:80 -p 443:443 \
  -v /path/to/server.toml:/etc/flashlin/server.toml:ro \
  -v /path/to/site:/var/www/html:ro \
  -v /path/to/certs:/etc/flashlin/certs:ro \
  endurasolution/flashlin:3.1.0
```

### Container paths

| Host path | Container path |
|---|---|
| `./public/` | `/var/www/html/` |
| `./config/server*.toml` | `/etc/flashlin/server.toml` |
| `./certs/` | `/etc/flashlin/certs/` |
| *(named volume)* | `/var/log/flashlin/` |

---

## PHP-FPM

The PHP Compose files run two services:

| Service | Image | Role |
|---|---|---|
| `flashlin` | built from source / `endurasolution/flashlin` | HTTP/HTTPS server + FastCGI client |
| `php-fpm` | `php:8.2-fpm-alpine` | PHP execution engine |

Both mount the same webroot. FlashLin forwards `.php` requests to `php-fpm:9000`.

**Adding PHP extensions** (e.g. `pdo_mysql`):

```yaml
# docker-compose.php.tls.yml
  php-fpm:
    build:
      context: .
      dockerfile_inline: |
        FROM php:8.2-fpm-alpine
        RUN docker-php-ext-install pdo pdo_mysql
```

```bash
docker compose -f docker-compose.php.yml build
docker compose -f docker-compose.php.yml up -d
```

---

## Useful commands

```bash
# Logs
docker compose logs -f flashlin

# Restart (picks up new certs or a changed config)
docker compose -f docker-compose.tls.yml restart flashlin

# Shell inside the container
docker compose exec flashlin sh

# Clear the running server's in-memory response cache
# (loopback-only — must run inside the container's network namespace)
docker exec flashlin flashlin purge cache

# Check config without starting a listener
docker exec flashlin flashlin --check /etc/flashlin/server.toml

# Supervised apps: status, and graceful restart of one app
docker exec flashlin flashlin apps
docker exec flashlin flashlin apps restart api

# Stop everything
docker compose down

# Stop and remove volumes too
docker compose down -v

# Update to a newer image
docker pull endurasolution/flashlin:3.1.0
docker compose down && docker compose up -d
```

`flashlin start`/`stop`/`restart` aren't listed above — those wrap `systemctl` for a systemd-managed
install and don't apply inside a container. Use the `docker`/`docker compose` commands above instead;
`purge cache`, `apps` and `--check` work fine via `docker exec` since they only talk to the FlashLin
process itself.

---

## Production checklist

- [ ] Use real Let's Encrypt certificates — never self-signed in production
- [ ] Set `WEBROOT` (or the volume mount) to your actual site directory
- [ ] Remove the demo `public/index.php`
- [ ] Pin the PHP image to a specific version, e.g. `php:8.2.27-fpm-alpine`
- [ ] Set `restart: always` instead of `unless-stopped`
- [ ] Set `json = true` under `[logging]` for structured log ingestion
- [ ] Review `[security.rate_limit]` for your expected traffic, and consider `max_connections_per_ip`
- [ ] Make sure FlashLin sees real client IPs (Linux bridge networking or `--network host`) — rate and
      connection limits key on the source address, and some setups (e.g. Docker Desktop) show every
      client as the gateway address
- [ ] Automate certificate renewal (a cron job/systemd timer, or FlashLin's own ACME client)
- [ ] Pin the image tag (`3.1.0`) rather than tracking `latest`
- [ ] Running apps? Set `stop_grace_period` (Compose) or `docker stop -t` to at least your apps' `stop_timeout`
- [ ] Back up mounted config and certs outside the container

---

## Links

| Resource | Link |
|---|---|
| Website | [flashlin.com](https://www.flashlin.com/) |
| Documentation | [flashlin.com/docs](https://www.flashlin.com/docs) |
| GitHub | [github.com/endurasolution/flashlin](https://github.com/endurasolution/flashlin) |
| Docker Hub | [hub.docker.com/r/endurasolution/flashlin](https://hub.docker.com/r/endurasolution/flashlin) |
| One-line installer (non-Docker) | `curl -fsSL https://flashlin.com/install_linux.sh \| bash` |

---

## License

FlashLin is licensed under **Creative Commons Attribution-NonCommercial 4.0 International (CC BY-NC 4.0)**.
Free to use, share, and adapt for non-commercial purposes with attribution — commercial use requires
written permission. Full license: <https://creativecommons.org/licenses/by-nc/4.0/>.
