<div align="center">

```
  ______ _           _     _     _
 |  ____| |         | |   | |   (_)
 | |__  | | __ _ ___| |__ | |    _ _ __
 |  __| | |/ _` / __| '_ \| |   | | '_ \
 | |    | | (_| \__ \ | | | |___| | | | |
 |_|    |_|\__,_|___/_| |_|_____|_|_| |_|
```

**Fast · Reliable · Cached · Powerful**

A Rust web server. Version 2.0 is a production release; see the roadmap below for ongoing hardening work.

[![License: CC BY-NC 4.0](https://img.shields.io/badge/License-CC%20BY--NC%204.0-lightgrey.svg)](https://creativecommons.org/licenses/by-nc/4.0/)
[![Rust](https://img.shields.io/badge/Rust-tested%201.95-orange)](https://www.rust-lang.org)
[![Version](https://img.shields.io/badge/version-2.0.0-blue)](https://github.com/endurasolution/flashlin)
[![Docker](https://img.shields.io/docker/pulls/endurasolution/flashlin)](https://hub.docker.com/r/endurasolution/flashlin)

</div>

---

## What is FlashLin?

FlashLin is a single-binary web server built on **Tokio + Hyper**, with optional
HTTP/3 through Quinn. Its feature set includes capabilities familiar from other servers:

| FlashLin does this | Like |
|---|---|
| Reverse proxy, round-robin upstream routing | Nginx |
| PHP-FPM / FastCGI hosting, virtual hosts | Apache |
| In-memory response cache | OpenLiteSpeed |
| Simple TOML config, zero-friction setup | Caddy |
| Memory-safe, zero `unsafe` code | Rust |

---

## Features

| Feature | Status |
|---|---|
| HTTP/1.1 + HTTP/2 (Tokio + Hyper) | ✅ |
| HTTP/3 over QUIC + TLS 1.3 | ✅ Optional experimental `http3` build; [setup and limits](docs/HTTP_PROTOCOLS.md) |
| TLS / HTTPS (rustls — no OpenSSL required) | ✅ |
| PHP-FPM + FastCGI (full protocol) | ✅ |
| Reverse proxy | ✅ |
| Virtual hosts (per-domain `.toml` files) | ✅ |
| In-memory response cache (RAM, with TTL) | ✅ |
| Gzip + Brotli compression | ✅ |
| ETag / Last-Modified / 304 Not Modified | ✅ |
| Security headers (CSP, HSTS, X-Frame, …) | ✅ |
| Rate limiting (req/min + burst) | ✅ |
| IP allow / deny lists | ✅ |
| Path traversal prevention | ✅ |
| Dotfile + sensitive file blocking | ✅ |
| Per-directory config (`.flashweb` files) | ✅ |
| `.htaccess` redirect / rewrite rules | ✅ |
| `nginx.conf` redirect rules | ✅ |
| Prometheus metrics endpoint | ✅ |
| Connection overload limit (HTTP 503) | ✅ |
| Graceful shutdown | ✅ |
| Structured JSON logging | ✅ |
| Per-virtual-host cache and rate-limit state | ✅ v2 |
| Static directory mounts | ✅ v2 |
| `.flashweb` directory ACLs and error pages | ✅ v2 |
| Configured Tokio worker count and header-read limits | ✅ v2 |
| Single binary — no runtime dependencies | ✅ |
| Hot config reload | ✅ Opt-in `--watch`, validated snapshots |
| WebSocket proxy (HTTP/1 upgrades) | ✅ v2 |
| HTTP QUERY method (RFC 10008) | ✅ Application/proxy transport, body validation and safe failover |
| Streaming files, byte ranges, precompressed assets | ✅ v2 |
| Pooled HTTP/HTTPS upstreams and streamed responses | ✅ v2 |
| Round-robin upstreams and passive failover | ✅ v2 |
| Auto TLS / ACME (Let's Encrypt) | ✅ Optional `acme` build; deployment validation pending |

---

## Install

### Linux — one-line install

```bash
curl -fsSL https://flashlin.com/install_linux.sh | bash
```

### Build from source

Tested with **Rust 1.95**; use a current stable toolchain:

```bash
rustup update stable

# HTTP-only binary
cargo build --release

# With TLS/HTTPS support
cargo build --release --features tls

# All three protocols, plus automatic certificates
cargo build --release --locked --features acme,http3
```

Binary is at `target/release/flashlin`.

HTTP/3 also needs `[http3] enabled = true`, TLS certificates and an open UDP port.
See [the protocol guide](docs/HTTP_PROTOCOLS.md) and [example config](examples/http3.toml).

### Docker (recommended for production)

```bash
# Pull the public image
docker pull endurasolution/flashlin:latest

# Run (HTTP)
docker run -d -p 80:80 \
  -v ./config/server.toml:/etc/flashlin/server.toml:ro \
  -v ./public:/var/www/html:ro \
  endurasolution/flashlin:latest

# Run (HTTPS)
docker run -d -p 80:80 -p 443:443 \
  -v ./config/server.toml:/etc/flashlin/server.toml:ro \
  -v ./public:/var/www/html:ro \
  -v ./certs:/etc/flashlin/certs:ro \
  endurasolution/flashlin:latest
```

See [dockerflashlin/README.md](dockerflashlin/README.md) for the full Docker + docker-compose guide.

---

## Quick start

```bash
# 1. Clone
git clone https://github.com/endurasolution/flashlin.git
cd flashlin

# 2. Build
cargo build --release

# 3. Run (uses server.toml in current directory)
./target/release/flashlin

# 4. Custom config path
./target/release/flashlin /etc/flashlin/server.toml
```

Open [http://localhost:80](http://localhost:80).

---

## Configuration

All config lives in a single `server.toml`. See [server.toml](server.toml) for the full reference with every option documented inline.

### Static site

```toml
[server]
host = "0.0.0.0"
port = 80
root = "./public"

[[routes]]
path  = "/"
root  = "./public"
index = "index.html"
```

### HTTPS

```toml
[server]
host = "0.0.0.0"
port = 443          # HTTPS listener

[tls]
enabled                = true
cert                   = "./certs/fullchain.pem"
key                    = "./certs/privkey.pem"
redirect_http_to_https = true  # port 80 → 301 → https://
http_port              = 80

[[routes]]
path = "/"
root = "./public"
```

Generate a self-signed cert for local testing:

```bash
openssl req -x509 -newkey rsa:4096 -days 365 -nodes \
  -keyout certs/privkey.pem -out certs/fullchain.pem \
  -subj "/CN=localhost"
```

### PHP-FPM

```toml
[server]
port = 80

[php]
enabled         = true
default_handler = "php82"

[[php.handlers]]
name          = "php82"
mode          = "fpm"
socket        = "/run/php/php8.2-fpm.sock"  # Unix socket (Linux)
# address     = "127.0.0.1:9000"            # TCP (Windows / Docker)
document_root = "./public"
timeout       = "30s"

[[routes]]
path        = "/"
root        = "./public"
index       = "index.php"
php_handler = "php82"
php_enabled = true
```

### Reverse proxy

```toml
[[routes]]
path       = "/api"
proxy_pass = "http://127.0.0.1:3000"
```

### Virtual hosts (multiple domains)

```toml
# server.toml
[server]
port        = 80
domains_dir = "./domains"   # one *.toml file per domain
```

```toml
# domains/example.com.toml
[domain]
name    = "example.com"
aliases = ["www.example.com"]
root    = "./sites/example.com/public"

[tls]
enabled = true
cert    = "./certs/example.com.crt"
key     = "./certs/example.com.key"

[[routes]]
path = "/"
root = "./sites/example.com/public"
```

---

## Per-directory config — `.flashweb`

Place a `.flashweb` file in any directory under your web root to set redirects, rewrites, custom headers, CORS, cache-control, and IP restrictions for that path and everything below it:

```ini
# ./public/.flashweb

[redirect]
/old-page = /new-page

[rewrite]
/app/(.*) = /index.php?route=$1

[headers]
X-Custom-Header = hello

[cors]
origins = ["https://app.example.com"]
methods = ["GET", "POST"]

[cache]
control = "public, max-age=3600"
```

FlashLin checks `.flashweb` → `.htaccess` → `nginx.conf` in that order for every request.

---

## Security defaults

FlashLin is **secure by default** — everything dangerous is blocked without configuration:

| Threat | Default |
|---|---|
| Dotfile access (`.env`, `.git`, `.htaccess`) | Blocked |
| Sensitive files (`*.sql`, `*.bak`, `composer.json`) | Blocked |
| Path traversal (`../../etc/passwd`) | Blocked |
| Null byte injection | Blocked |
| Windows device names (`CON`, `NUL`, `PRN`) | Blocked |
| Security headers (CSP, HSTS, X-Frame-Options, …) | Enabled |
| Rate limiting | Configurable |
| Server version disclosure | Hidden |

---

## Prometheus metrics

FlashLin exposes metrics at `/metrics` in Prometheus text format:

```
flashlin_requests_total 42381
flashlin_responses_2xx 41900
flashlin_responses_4xx 420
flashlin_responses_5xx 61
flashlin_bytes_sent 198432100
```

The endpoint only accepts loopback clients. Use a local monitoring agent; remote
clients receive 403 even if allowed by a directory ACL.

---

## Systemd (Linux production)

```ini
# /etc/systemd/system/flashlin.service
[Unit]
Description=FlashLin Web Server
After=network.target

[Service]
Type=simple
User=www-data
WorkingDirectory=/var/www
ExecStart=/usr/local/bin/flashlin /etc/flashlin/server.toml
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now flashlin
```

---

## Docker

A complete Docker setup with docker-compose, TLS, and PHP-FPM support lives in [dockerflashlin/](dockerflashlin/):

| Compose file | Ports | PHP | TLS |
|---|---|---|---|
| `docker-compose.yml` | 80 | no | no |
| `docker-compose.tls.yml` | 80 + 443 | no | yes |
| `docker-compose.php.yml` | 80 | yes | no |
| `docker-compose.php.tls.yml` | 80 + 443 | yes | yes |

```bash
cd dockerflashlin

# HTTP
docker compose up -d

# HTTPS (put certs in ./certs/ first)
docker compose -f docker-compose.tls.yml up -d
```

---

## Testing

```bash
cargo test                          # all tests
cargo test -- --nocapture           # with output
cargo test security                 # security tests only
cargo test config                   # config tests only
cargo test routing                  # routing tests only
```

---

## Load testing

```bash
wrk -t4 -c100 -d30s http://localhost/
hey -n 100000 -c 200 http://localhost/
ab  -n 50000  -c 100 http://localhost/
```

---

## Repository layout

```
flashlin/
├── src/
│   ├── main.rs              # Entry point, config loading, banner
│   ├── server/              # TCP listener, worker pool, graceful shutdown
│   ├── http/                # Request parsing, response helpers, router
│   ├── config/              # TOML parser + validator
│   ├── cache/               # In-memory response cache (DashMap + TTL)
│   ├── static_files/        # File serving, ETag, cache headers, path security
│   ├── php/                 # PHP-FPM + FastCGI protocol implementation
│   ├── proxy/               # Reverse proxy + upstream management
│   ├── tls/                 # rustls config + certificate loader
│   ├── security/            # Rate limiter, IP ACL, security headers, sensitive-file list
│   ├── middleware/          # Compression (gzip/brotli), request-ID, access logging
│   ├── logging/             # Access log + error log writers
│   ├── metrics/             # Prometheus atomic counters + exposition
│   ├── flashweb/            # Per-directory .flashweb / .htaccess / nginx.conf parser
│   ├── error_pages.rs       # Built-in HTML error pages (404, 500, 111, …)
│   ├── version.rs           # Version constants + license text
│   └── tests/               # Integration tests (security, config, routing)
├── public/                  # Default web root (welcome page + PHP test)
├── domains/                 # Example virtual host configs
├── server.toml              # Full configuration reference
├── sample.toml              # Minimal sample config
├── dockerflashlin/          # Docker + docker-compose setup
└── .github/workflows/       # CI/CD — build + publish Docker image
```

---

## v2.0 roadmap

| Milestone | Status |
|---|---|
| **v2.0 core** | Delivered: per-host runtime state, metrics endpoint, static mounts, directory ACLs/error pages, enforced worker and header-read settings |
| **v2.0 proxy** | Delivered: streamed responses, verified HTTPS upstreams, connection pools, round-robin balancing, passive failover, WebSockets. Pending: fully streamed uploads, active health probes |
| **v2.0 operations** | Delivered: hot reload. Pending: OpenTelemetry, cache purge/revalidation, log rotation |
| **v2.0 TLS** | Delivered: ACME and manual certificate reload. Pending: independent per-domain SNI certificates |
| **v2.0 hardening** | Pending: independent security review, broader compatibility suite, SBOM/dependency review, signed release artifacts |

The current implementation and its limits are documented in
[PERFORMANCE.md](docs/PERFORMANCE.md). Start with
[examples/performance.toml](examples/performance.toml), validate with
`flashlin --check examples/performance.toml`, and benchmark representative traffic
before deploying. Use `--watch` for validated configuration and manual certificate
reload, and build with `--features acme` for automatic certificates. See
[automatic HTTPS and reload](docs/AUTOMATIC_HTTPS_AND_RELOAD.md) and the
[benchmark suite](benchmarks/README.md). Before enabling the opt-in
`server.sendfile`/`server.direct_static_io` fast paths, read
[docs/PRODUCTION.md](docs/PRODUCTION.md) — both default off. Requests/uploads are still buffered;
independent SNI certificates, active health probes and PHP-FPM pooling remain
pending. Real-domain ACME issuance still requires deployment validation.

A CPU-isolated Docker/Ubuntu benchmark against nginx and Apache (see
[benchmarks/ubuntu/](benchmarks/ubuntu/) and
[benchmarks/ubuntu-results/run8/REPORT.md](benchmarks/ubuntu-results/run8/REPORT.md))
showed large-file throughput matching or exceeding both with `server.sendfile`
enabled; small-file throughput still trails nginx at high concurrency. This is
one controlled measurement, not a general speed claim across all workloads —
benchmark your own traffic before relying on it.

HTTP/3 is available as an optional, explicitly experimental QUIC listener
alongside HTTP/1.1 and HTTP/2 (`--features http3`). Independent HTTP/3
interoperability and production load validation remain pending.

---

## License

FlashLin is licensed under **Creative Commons Attribution-NonCommercial 4.0 International (CC BY-NC 4.0)**.

- Free to use, share, and adapt for **non-commercial** purposes with attribution.
- **Commercial use requires written permission** from the FlashLin team.

```
flashlin --license
```

Full license: https://creativecommons.org/licenses/by-nc/4.0/
