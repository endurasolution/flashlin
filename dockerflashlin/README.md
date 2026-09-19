# FlashLin Docker

Docker setup for **FlashLin** — a fast, cached, powerful Rust web server.

```
dockerflashlin/
├── Dockerfile                  # Multi-stage build from source
├── Dockerfile.prebuilt         # Download binary via install.sh
├── docker-compose.yml          # HTTP on port 80
├── docker-compose.tls.yml      # HTTPS on port 443 + HTTP 80→443 redirect
├── docker-compose.php.yml      # PHP-FPM + HTTP port 80
├── docker-compose.php.tls.yml  # PHP-FPM + HTTPS port 443
├── entrypoint.sh
├── config/
│   ├── server.toml             # HTTP port 80
│   ├── server.tls.toml         # HTTPS port 443 (TLS enabled)
│   ├── server.php.toml         # PHP-FPM + HTTP port 80
│   └── server.php.tls.toml    # PHP-FPM + HTTPS port 443
├── certs/                      # Mount your TLS certs here (see SSL section)
└── public/
    ├── index.html
    └── index.php
```

---

## Quick start

### HTTP (port 80)

```bash
cd dockerflashlin
docker compose up -d
```

### HTTP + PHP-FPM (port 80)

```bash
cd dockerflashlin
docker compose -f docker-compose.php.yml up -d
```

### HTTPS (ports 80 + 443)

```bash
cd dockerflashlin
docker compose -f docker-compose.tls.yml up -d
```

### HTTPS + PHP-FPM (ports 80 + 443)

```bash
cd dockerflashlin
docker compose -f docker-compose.php.tls.yml up -d
```

---

## SSL / TLS setup

### Optional HTTP/3

The source Dockerfile builds `acme,http3`. After configuring TLS certificates,
add this to the selected TLS server configuration:

```toml
[http3]
enabled = true
port = 443
max_concurrent_streams = 32
```

Add `"443:443/udp"` to the service's `ports` list alongside the existing TCP
mapping, allow UDP 443 through your firewall, and rebuild the source image.
HTTP/1.1 and HTTP/2 continue on TCP 443. See the
[protocol guide](../docs/HTTP_PROTOCOLS.md) for limits and verification. HTTP/3
remains experimental; the prebuilt/download image is not guaranteed to include it.

### Step 1 — get your certificates

**Option A — Let's Encrypt (recommended for production)**

```bash
# Install certbot on the host, then:
certbot certonly --standalone -d yourdomain.com

# Certificates are placed at:
#   /etc/letsencrypt/live/yourdomain.com/fullchain.pem
#   /etc/letsencrypt/live/yourdomain.com/privkey.pem
```

**Option B — self-signed (for local / dev)**

```bash
mkdir -p certs
openssl req -x509 -newkey rsa:4096 -sha256 -days 365 -nodes \
  -keyout certs/privkey.pem \
  -out    certs/fullchain.pem \
  -subj   "/CN=localhost"
```

### Step 2 — place certs in `./certs/`

```
dockerflashlin/certs/
├── fullchain.pem    ← certificate + intermediate chain
└── privkey.pem      ← private key
```

If you used Let's Encrypt, symlink or copy the files:

```bash
mkdir -p certs
cp /etc/letsencrypt/live/yourdomain.com/fullchain.pem certs/
cp /etc/letsencrypt/live/yourdomain.com/privkey.pem   certs/
```

### Step 3 — start with TLS

```bash
cd dockerflashlin
docker compose -f docker-compose.tls.yml up -d
```

FlashLin will:
- Listen on **port 443** for HTTPS
- Listen on **port 80** and redirect all HTTP traffic to HTTPS automatically

### Renew Let's Encrypt certificates

```bash
certbot renew
cp /etc/letsencrypt/live/yourdomain.com/fullchain.pem certs/
cp /etc/letsencrypt/live/yourdomain.com/privkey.pem   certs/
docker compose -f docker-compose.tls.yml restart flashlin
```

---

## Build options

### Option A — build from source

Build context is the **flashlin project root**:

```bash
# From the flashlin project root:
docker build -f dockerflashlin/Dockerfile -t flashlin .

# Or let Compose handle it:
cd dockerflashlin && docker compose build
```

### Option B — pre-built binary via official install script

Build context is the **dockerflashlin/** directory:

```bash
cd dockerflashlin
docker build -f Dockerfile.prebuilt -t flashlin .
```

---

## Configuration

### Ports

| Compose file                   | Ports         | PHP | TLS |
|--------------------------------|---------------|-----|-----|
| `docker-compose.yml`           | 80            | no  | no  |
| `docker-compose.tls.yml`       | 80 + **443**  | no  | yes |
| `docker-compose.php.yml`       | 80            | yes | no  |
| `docker-compose.php.tls.yml`   | 80 + **443**  | yes | yes |

### Config files

| Config file              | Port | TLS | PHP |
|--------------------------|------|-----|-----|
| `server.toml`            | 80   | no  | no  |
| `server.tls.toml`        | 443  | yes | no  |
| `server.php.toml`        | 80   | no  | yes |
| `server.php.tls.toml`    | 443  | yes | yes |

### Environment variables

| Variable          | Default                     | Description                           |
|-------------------|-----------------------------|---------------------------------------|
| `WEBROOT`         | `./public`                  | Host directory mounted as webroot     |
| `RUST_LOG`        | `info`                      | Log level (error/warn/info/debug)     |
| `FLASHLIN_CONFIG` | `/etc/flashlin/server.toml` | Config path inside the container      |

### Use a custom webroot

```bash
WEBROOT=/srv/mysite docker compose -f docker-compose.tls.yml up -d
```

### Mount your own config

```bash
docker run -d \
  -p 80:80 -p 443:443 \
  -v /path/to/server.toml:/etc/flashlin/server.toml:ro \
  -v /path/to/site:/var/www/html:ro \
  -v /path/to/certs:/etc/flashlin/certs:ro \
  flashlin
```

---

## Container paths

| Host path                    | Container path                    |
|------------------------------|-----------------------------------|
| `./public/`                  | `/var/www/html/`                  |
| `./config/server.tls.toml`   | `/etc/flashlin/server.toml`       |
| `./certs/`                   | `/etc/flashlin/certs/`            |
| *(named volume)*             | `/var/log/flashlin/`              |

---

## PHP-FPM details

The PHP compose files start two services:

| Service    | Image                 | Role                                      |
|------------|-----------------------|-------------------------------------------|
| `flashlin` | built from source     | HTTP/HTTPS server, static files, FastCGI client |
| `php-fpm`  | `php:8.2-fpm-alpine`  | PHP execution engine                      |

Both mount the same `WEBROOT`. FlashLin proxies `.php` requests to `php-fpm:9000`.

**Add PHP extensions** (e.g. `pdo_mysql`):

```yaml
# docker-compose.php.tls.yml
  php-fpm:
    build:
      context: .
      dockerfile_inline: |
        FROM php:8.2-fpm-alpine
        RUN docker-php-ext-install pdo pdo_mysql
```

---

## Useful commands

```bash
# View logs
docker compose logs -f flashlin

# Restart (picks up new certs or config)
docker compose -f docker-compose.tls.yml restart flashlin

# Open a shell inside the container
docker compose exec flashlin sh

# Stop
docker compose down

# Stop and remove volumes
docker compose down -v
```

---

## Production checklist

- [ ] Use real Let's Encrypt certs — not self-signed
- [ ] Set `WEBROOT` to your actual site directory
- [ ] Remove `public/index.php` (information disclosure)
- [ ] Set `restart: always` instead of `unless-stopped`
- [ ] Pin PHP image version (e.g. `php:8.2.27-fpm-alpine`)
- [ ] Set `json = true` in `[logging]` for structured log ingestion
- [ ] Review rate limit in `[security.rate_limit]`
- [ ] Automate cert renewal with a cron job or Certbot timer
