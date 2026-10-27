# Changelog

## 3.0.0

### Supervised Node.js and Python apps (`[[apps]]`)

- New `[[apps]]` config: FlashLin starts Node.js, Python or any other
  command with the server, passes it `PORT`/`HOST`, and stops it on shutdown.
  `runtime = "node" | "python"` picks the interpreter; `command`/`args` run
  anything else (gunicorn, uvicorn, npm), with `{port}` / `$PORT` substituted.
- Routes connect with `app = "<name>"`, in `server.toml` or in `domains/*.toml`.
  At load time this becomes loopback `proxy_pass` / `proxy_upstreams`, so apps
  get the existing proxy path unchanged: WebSockets, streaming, QUERY,
  X-Forwarded-*, passive failover.
- `instances = N` runs N copies on consecutive ports, load-balanced.
- Crash restarts back off exponentially (0.5 s up to 30 s; a 60 s run resets
  it). After `max_restarts` consecutive crashes the app is marked `failed`
  until restarted by hand. `restart = "always" | "on-failure" | "never"`.
- Optional HTTP health checks restart an app that stops answering, or that
  doesn't answer within `start_timeout` after starting.
- Graceful stop: SIGTERM to the app's whole process group, SIGKILL after
  `stop_timeout`. Apps are stopped after client connections drain.
- stdout/stderr go to FlashLin's log tagged with the app name, or to `log_file`.
  Lines are capped at 16 KiB.
- New CLI: `flashlin apps [config]` (state, pid, port, restarts, health, uptime,
  last exit) and `flashlin apps restart <name> [config]`.
- Security: FlashLin refuses to run an app as root unless the entry sets `user`
  (it switches to that account and drops supplementary groups), or sets
  `user = "root"` to allow it explicitly. On Linux, apps get SIGTERM if
  FlashLin itself is killed, instead of being orphaned on their ports.
- Changing `[[apps]]` requires a restart. `--watch` rejects such reloads and
  keeps the running config.
- See [docs/APPS.md](docs/APPS.md) and [examples/apps.toml](examples/apps.toml).

### Listening on IPv4 and IPv6

- **Fixed:** an IPv6 `server.host` such as `"::"` stopped FlashLin at startup
  ("invalid socket address syntax"): the address was built as text, giving
  `:::80`. Hosts are now parsed as IP addresses, with or without brackets,
  and checked by `flashlin --check`.
- `host = "::"` now accepts IPv4 and IPv6 clients on every OS, like nginx's
  `listen [::]:80 ipv6only=off`. Windows (and Linux with
  `net.ipv6.bindv6only=1`) used to make it IPv6-only. Applies to the HTTP,
  HTTPS, redirect and HTTP/3 listeners. IPv4 clients arriving this way are
  reported as plain `1.2.3.4` in logs, `X-Forwarded-For`, ACLs and limits.

### Directory listings

- **Fixed (security):** file and directory names were inserted into listing
  pages without HTML escaping. A name containing `<` or `"` (allowed on
  Unix, e.g. in an upload folder) could inject script into the page. Names
  and the page title are now escaped and links are percent-encoded, which
  also fixes links to names containing `#`, `?`, `%` or spaces.
- **Fixed:** listings showed the names of dotfiles and sensitive files
  (`.env`, `*.sql`, …) even though FlashLin refuses to serve them. They are
  now left out whenever `block_dotfiles` / `block_sensitive_files` apply.
- Listings are now an nginx-`autoindex`-style table with last-modified time
  and size, with CSS classes on every row and cell.
- New `server.directory_listing_css` (per domain:
  `[domain] directory_listing_css`) links your own stylesheet after the
  built-in styles.

### Security

- **Fixed:** `allow_ips` / `deny_ips` and `.flashweb` access rules did not
  match IPv4 clients on a dual-stack (`::`) listener, which reports them as
  `::ffff:a.b.c.d`. A `deny_ips = ["1.2.3.4"]` rule could therefore be
  bypassed there. Addresses are now canonicalized before matching.
- Rate limiting groups IPv6 clients by /64, so a client can no longer get a
  fresh bucket for every address in its own prefix. IPv4-mapped clients share
  their IPv4 address's bucket.
- New `server.max_connections_per_ip`: caps simultaneous connections per
  client (IPv4 address or IPv6 /64), checked at accept before any TLS or HTTP
  work. Excess connections are closed silently. Default 0 (off).
- Control endpoints (`/flashlin/*`) now also refuse requests carrying
  `Forwarded`, `X-Forwarded-For`, `X-Real-IP` or `Via`. Previously a reverse
  proxy or tunnel on the same host could relay internet traffic to them,
  because it arrived from loopback. Optional `server.control_token` adds
  bearer-token authentication (constant-time compare); the CLI sends it
  automatically.
- `parse_size` no longer overflows on huge values (`99999999999GB`).
- The production systemd units (all distros) now set `KillMode=mixed` and
  `TimeoutStopSec=60`, so FlashLin drains connections and stops its apps in
  order instead of systemd signalling every process at once.

### Performance

- Virtual hosts: each host's effective config is merged once per config
  generation instead of deep-cloning the whole server config on every request.
- Rate limiting is keyed by IP address instead of a string, so it no longer
  allocates one per request. `parse_size`, which runs twice per request, no
  longer allocates either.

### Fixes

- Proxied responses always use HTTP/1.1 toward the client. FlashLin used to
  pass the upstream's HTTP version through, so an HTTP/1.0 backend (Python's
  `http.server`, some WSGI servers) made it answer HTTP/1.1 clients with
  HTTP/1.0. That ruled out chunked encoding, so keep-alive connections were
  closed after streamed bodies. It also meant an unexpected upstream version
  could reach a panic in Hyper's response encoder; release builds use
  `panic = "abort"`, so that would have stopped the server.
- On Windows, sockets created by Tokio are inheritable by child processes.
  The FastCGI and health-check connections now use non-inheritable sockets,
  so an app spawned while one is open can't keep it alive or reset it.
  Listener and proxy sockets were already non-inheritable.

### Known limitations

- Apps on Windows are for development: stopping reaches only the app
  process, not children it spawned, and if FlashLin is killed abruptly its
  apps keep running.
- The Unix-specific app supervision paths (process groups, signals, user
  switching, parent-death signal) compile for Linux and follow the
  documented POSIX/Linux semantics. The live tests in this release ran on
  Windows; run `cargo test` on Linux (the CI matrix includes ubuntu-latest)
  before deploying.
- No new nginx comparison benchmark has been run for 3.0. The per-request
  savings above are allocation and copy removals, not measured throughput
  gains.

## 2.0.0

### Service control commands

- Added `flashlin start|stop|restart|status`: thin wrappers around
  `systemctl <action> flashlin`, for the unit `production/install.sh`
  installs. Not a second process supervisor — systemd still owns
  restart-on-failure and log capture.
- Added `flashlin purge cache [config]`: clears the running server's
  in-memory response cache (default and every virtual host). The cache
  lives in that process's memory, so this works by sending a loopback-only
  HTTP request (`POST /flashlin/purge-cache`, same access model as
  `/metrics`) to the already-running server rather than touching anything
  on disk.
- Added `flashlin remove`: stops/disables the service and removes the
  binary and systemd unit — the same safe default as
  `production/uninstall.sh` with no flags. Config, web root and TLS/ACME
  data are left in place; use `uninstall.sh --purge` to remove those too.
- Added `flashlin --help`.

### Zero-copy static delivery (opt-in)

- Added `server.sendfile` (Linux-only, default off): a minimal HTTP/1.1
  keep-alive loop that serves eligible plain-HTTP static GET/HEAD requests
  with `sendfile(2)`, transferring file bytes kernel-to-socket with no
  userspace copy. It reuses the ordinary path's security checks (path
  containment, symlink verification, IP access, rate limiting,
  sensitive-file blocking) rather than reimplementing them, and falls back
  to the ordinary Hyper connection handler — with every already-read byte
  replayed intact — for anything outside its scope: TLS, virtual hosts,
  request bodies, `Range`, compression, rewrite rules, PHP and proxy
  routes. See [docs/PRODUCTION.md](docs/PRODUCTION.md) before enabling it.
- Added `server.direct_static_io` (default off): serves static files
  recently observed to be ≤16 KiB directly on the async worker instead of
  the blocking thread pool, skipping one executor round trip per request.
  Larger files always keep using the blocking pool — serving them inline
  was measured to stall other connections on the same worker thread more
  than the handoff it saved.
- Uncached static responses between 1 MiB and 8 MiB now complete inside the
  single blocking job that already opened the file, instead of re-entering
  the blocking pool once per additional 1 MiB chunk; still uses the same
  pooled buffers as the streaming path, not a large unpooled allocation.
- Measured on a CPU-isolated Docker/Ubuntu nginx/Apache/Flashlin harness
  (see [benchmarks/ubuntu/](benchmarks/ubuntu/)): with `sendfile` and
  `direct_static_io` enabled, 4 MiB static-file throughput moved from
  roughly half of nginx/Apache to matching or exceeding both, and peak
  memory under concurrent large-file load dropped from up to 775 MiB to
  7.4 MiB. Small-file throughput still trails nginx at high concurrency.
  Raw results: `benchmarks/ubuntu-results/run8/REPORT.md`.

### HTTP/1.1, HTTP/2 and optional HTTP/3

- Added an opt-in `http3` build and `[http3]` UDP listener using h3 / Quinn / TLS 1.3.
  TCP HTTP/1.1 and HTTP/2 remain available; HTTPS advertises HTTP/3 with Alt-Svc.
- QUIC uses the shared router for static delivery, security controls and QUERY
  forwarding, with bounded streams/flow control, body/header/time limits,
  framing checks, source validation and shutdown draining. No 0-RTT is enabled.
- Manual certificate rotation and request-level config reload work on QUIC;
  HTTP/3 listener changes require restart. Automatic TLS shares its resolver.
- Added certificate-verifying TCP/UDP protocol regression tests and a setup guide
  at docs/HTTP_PROTOCOLS.md. HTTP/3 remains experimental; browser interoperability,
  public deployment testing and production benchmark comparisons are pending.
- Source container builds include `acme,http3` and expose UDP 443. Default config
  does not enable HTTP/3; published images/releases are not changed here.

### QUERY method support

- Added explicit QUERY media-type validation, static-resource rejection and
  body-preserving safe failover through proxy_upstreams. QUERY results bypass
  the static cache; backend applications own query semantics and formats.
- Proxy OPTIONS discovery/preflight now reaches the upstream unless .flashweb
  explicitly supplies local CORS. Local OPTIONS also applies custom headers.
- Route method matching is case-sensitive and 405 responses include Allow.
- Preserve CGI Content-Type even when a QUERY request body is empty.
- Added real TCP QUERY regression coverage alongside existing WebSocket tests;
  see docs/QUERY_AND_WEBSOCKETS.md for supported scope and examples.

### Delivered foundation

- Versioned the v2 release track and made `server.workers` configure Tokio's
  multi-thread runtime.
- Enforced the configured HTTP/1 header buffer limit and header-read timeout.
- Added a Prometheus endpoint at `GET /metrics`; all dispatched responses are
  counted, including early error responses.
- Scoped response caches and rate limiters per virtual host rather than sharing
  the first host's settings across the process.
- Added periodic idle-client eviction to rate-limit state.
- Added static directory mounts with URL-prefix stripping.
- Implemented `.flashweb` IP allow/deny rules and custom error documents.
- Added `Vary: Accept-Encoding` to compressed responses.

### Known limitations and ongoing hardening

v2.0.0 is a production release; this section is a roadmap of areas still being
hardened, not a list of blockers. Match your rollout to what you actually use:

- Fully streamed uploads, active health checks and PHP-FPM pooling are not yet
  implemented; uploads are buffered and passive failover is what's available today.
- ACME issuance/renewal has been validated against cached certificates and live
  keep-alive reload, not yet against a fresh real-domain CA issuance; independent
  per-domain SNI certificates are pending.
- Observability/tracing beyond the `/metrics` endpoint, and broader integration/
  security test coverage, are ongoing work.
- SBOM/dependency review and signed release artifacts are not yet produced.
  Cross-platform regression CI has been added but has not yet run on a remote host.
- The `server.sendfile` and `server.direct_static_io` fast paths (both default
  off) are new: unit- and loopback-tested, and `sendfile` has had a manual
  security review (path traversal, symlink TOCTOU, header injection, request
  smuggling, panic-reachability, rate-limiter accounting), but neither has been
  fuzzed, soak-tested, or exercised against adversarial/slow clients yet. See
  [docs/PRODUCTION.md](docs/PRODUCTION.md) before enabling either for real traffic.

### Delivery and capacity improvements

- Large static files stream through a 64 KiB reader; small cache entries share
  byte buffers. Cache insertions enforce the payload budget under concurrency.
- Added single byte ranges, suffix ranges, If-Range, HEAD and fresh-file cache
  keys using nanosecond metadata validators. Resolved static symlinks stay in root.
- Added fresh .br/.gz sidecar negotiation and bounded asynchronous dynamic
  compression. Encoding qualities and Vary are honored for identity responses too.
- Replaced manual proxy framing with Hyper's pooled client and streamed bodies.
  HTTPS upstreams verify certificates when built with the tls feature.
- Added WebSocket tunnels, round-robin proxy_upstreams and passive failure
  cooldown with limited empty-body GET/HEAD failover.
- Enforced socket read/write idle timeouts, HTTP/2 stream/header limits and TLS
  handshake deadlines. Fixed the missing HTTP/1 timer attachment.
- Added --check/-t configuration validation; startup now rejects missing/invalid
  configuration. Logging level and JSON output follow configuration.
- Added SIGTERM handling and a 30-second ordinary-connection drain deadline.
  HTTP overload responses now use standard 503 instead of informational 111.
- Restricted built-in metrics to loopback and corrected CORS preflight ordering.
- Added real TCP regression tests, a cross-platform test workflow and a documented
  performance profile. See docs/PERFORMANCE.md for scope and limitations.

### Automatic HTTPS, reload and measurement

- Added optional acme builds with explicit CA terms consent, staging by default,
  persistent certificate/account cache, renewal and TLS-ALPN-01 challenge handling.
- Added --watch for validated configuration generations and manual certificate
  rotation. Existing keep-alive requests see new routing; in-flight requests retain
  their generation. Invalid changes and restart-only changes preserve active state.
- Scoped cache, rate-limit and upstream state to each generation to avoid retaining
  removed configuration forever. Reload resets these stateful controls.
- Added cached-ACME TLS, certificate/key mismatch and live keep-alive reload tests.
- Added a bounded-memory JSON load generator, matched Docker comparison profiles,
  deterministic fixtures and a Windows native baseline runner with CPU/RSS sampling.
  Production comparisons and fresh CA issuance have not been completed locally.
- Container source builds include acme; FLASHLIN_WATCH=1 enables reload.
- Enabled TCP_NODELAY on accepted sockets and preserved transport vectored writes
  through the idle-timeout wrapper; added byte-preservation/deadline regression.
- Recorded a Windows loopback baseline for 1 KiB, 64 KiB and 4 MiB files with
  three repetitions each and zero measured request failures. See
  benchmarks/LOCAL_BASELINE.md; this is not a production or Nginx/Apache comparison.
