# FlashLin v2.0 — Latest Updates

Last updated: 14 September 2026  
Version track: 2.0.0  
Status: In development — not a published production release

## Overview

FlashLin v2 adds optional HTTP/3, QUERY support, WebSocket proxying, improved
static delivery, pooled upstream connections, automatic certificate management
and validated hot reload. Optional build features keep the default HTTP-only
build free of the QUIC and ACME dependencies.

Automatic certificates and hot reload are implemented, but live public
certificate issuance/renewal and production benchmarks still need validation.
HTTP/3 remains experimental. No speed advantage over Nginx or Apache has been
measured, and these updates do not establish complete feature parity with either.

## 1. HTTP/1.1, HTTP/2 and HTTP/3

- Verified existing HTTP/1.1 and HTTP/2 support over TCP.
- HTTPS negotiates HTTP/1.1 or HTTP/2 through TLS ALPN.
- Plaintext HTTP/2 supports prior-knowledge connections; h2c Upgrade is not implemented.
- Added optional HTTP/3 over QUIC and TLS 1.3 using h3 and Quinn.
- Added an `[http3]` configuration section and a dedicated UDP listener.
- HTTPS TCP responses advertise the configured HTTP/3 port through `Alt-Svc`.
- TCP and UDP can use the same port number while HTTP/1.1 and HTTP/2 remain available.
- HTTP/3 uses the shared routing, virtual-host, security and application pipeline.
- Added QUIC source-address validation, stream and flow-control limits, handshake
  deadlines, request timeouts, response write deadlines and shutdown draining.
- Malformed HTTP/3 Content-Length framing resets the affected stream.
- HTTP/3 uses reloaded manual certificates or the current automatic certificate resolver.

HTTP/3 is disabled unless explicitly configured and built with the `http3`
feature. It requires certificates and a reachable UDP port. 0-RTT requests,
active address migration, WebTransport, HTTP datagrams and outgoing HTTP/3 proxy
connections are not enabled/implemented.

Guide: [HTTP protocols](docs/HTTP_PROTOCOLS.md)  
Example: [HTTP/3 configuration](examples/http3.toml)

## 2. QUERY method support

- Added QUERY request handling over HTTP/1.1, HTTP/2 and optional HTTP/3.
- Preserves method, body bytes, escaped URI and end-to-end headers when proxying.
- Validates QUERY Content-Type and enforces request body size and read-time limits.
- Static resources reject QUERY with HTTP 405 and an `Allow` header.
- Route method matching is case-sensitive.
- QUERY responses bypass the static cache to avoid mixing different request bodies.
- Supports bounded, body-preserving QUERY failover through configured upstream groups.
- OPTIONS reaches the backend for discovery and preflight unless `.flashweb`
  explicitly supplies local CORS handling.
- PHP/CGI metadata preserves QUERY, content type and body length, including empty bodies.

Backend applications must implement the query format and its read-only semantics.
FlashLin does not provide a database engine or a built-in query language.

Guide: [QUERY and WebSockets](docs/QUERY_AND_WEBSOCKETS.md)  
Example: [QUERY and WebSocket routes](examples/query-websocket.toml)

## 3. Reverse proxy, WebSockets and streaming

- Added HTTP/1.1 WebSocket upgrade forwarding and bidirectional tunnels.
- Added pooled HTTP/HTTPS upstream connections and streamed upstream responses.
- HTTPS upstream certificates are verified in TLS-enabled builds.
- Added round-robin upstream groups, passive failure cooldown and bounded failover.
- Supports streamed responses such as server-sent events.
- WebSocket upgrades are not automatically replayed.

WebSocket support is reverse-proxy support: the application backend performs
the handshake and processes messages. HTTP/2 and HTTP/3 extended CONNECT
WebSockets are not implemented. Request uploads remain buffered within limits.

## 4. Static delivery, caching and compression

- Large static responses stream through a 64 KiB reader.
- Small cached responses share byte buffers; concurrent cache insertion respects
  the configured payload budget.
- Added single byte ranges, suffix ranges, If-Range and HEAD handling.
- Uses fresh-file cache keys with high-resolution metadata validators.
- Added negotiation of fresh precompressed `.br` and `.gz` sidecar files.
- Added bounded asynchronous dynamic compression.
- Honors encoding quality preferences and `Vary: Accept-Encoding`.
- Added static directory mounts with URL-prefix stripping.
- Keeps resolved static symlinks within the document root.
- Enabled TCP_NODELAY on accepted sockets and preserved vectored writes through
  the transport timeout wrapper.

These are delivery improvements, not evidence of a measured page-load advantage.

Guide: [Performance and limits](docs/PERFORMANCE.md)  
Example: [Performance configuration](examples/performance.toml)

## 5. Automatic HTTPS and hot reload

- Added optional ACME certificate management with persistent account/certificate
  storage, renewal handling and TLS-ALPN-01 challenges.
- Uses staging by default and requires explicit acceptance of CA terms.
- Added `--watch` for validated configuration and manual certificate reloads.
- New requests on existing keep-alive and QUIC connections see updated routing.
- In-flight requests retain their original configuration generation.
- Invalid configurations and mismatched certificate/key updates keep the previous state.
- Cache, rate-limit and upstream state are scoped to configuration generations;
  successful reloads reset these stateful controls.
- Listener, HTTP/3, worker, global connection-limit, logging and ACME setting
  changes require a restart rather than an unsafe partial reload.

Cached-certificate TLS and QUIC behavior has been tested locally. Fresh public
issuance and renewal still require real-domain deployment validation. Independent
per-host SNI certificate selection remains pending.

Guide: [Automatic HTTPS and reload](docs/AUTOMATIC_HTTPS_AND_RELOAD.md)  
Example: [Automatic HTTPS configuration](examples/automatic-https.toml)

## 6. Security, capacity and operations

- Scoped caches and rate-limit state per virtual host.
- Added idle-client eviction from rate-limit state.
- Implemented `.flashweb` directory IP rules and custom error documents.
- Enforced worker settings, header limits and header-read deadlines.
- Added socket read/write idle timeouts and TLS handshake deadlines.
- Added HTTP/2 stream/header limits and HTTP/3 transport/request limits.
- Added `--check` / `-t` configuration validation; invalid or missing configuration
  prevents startup.
- Corrected ordinary HTTP overload responses from informational 111 to HTTP 503.
- Shares the global connection cap across TCP and QUIC; overloaded QUIC
  connections are refused before HTTP requests can be served.
- Added graceful shutdown handling with a bounded drain period and Unix SIGTERM support.
- Added a loopback-only Prometheus endpoint at `GET /metrics`.
- Counts responses from all dispatch paths, including early security failures.
- Applies configured logging level and JSON output settings.

## 7. Build, Docker and verification

```sh
# HTTP/1.1 and HTTP/2 without TLS
cargo build --release --locked

# HTTP/1.1 and HTTP/2 with HTTPS
cargo build --release --locked --features tls

# All three protocols with manual certificates
cargo build --release --locked --features http3

# All three protocols plus automatic certificates
cargo build --release --locked --features acme,http3
```

- The source Dockerfile now builds with `acme,http3` and exposes UDP 443.
- HTTP/3 still needs explicit configuration and UDP port publication, for example
  `-p 443:443/tcp -p 443:443/udp`.
- `FLASHLIN_WATCH=1` enables reload in the container entrypoint.
- Added a Windows/Linux CI matrix covering the four feature configurations below.
- Version output now reports development status and compiled protocol/TLS capabilities.
- Built the optimized all-feature Windows executable at
  `target/v2-check/release/flashlin.exe`.

### Recorded local test results

Windows, Rust 1.95, 14 September 2026:

| Build configuration | Passing tests |
|---|---:|
| HTTP-only, no default features | 51 |
| TLS | 52 |
| HTTP/3, including TLS | 56 |
| All features, including ACME | 58 |

These are overlapping test suites, not separate totals. Tests use real loopback
TCP/UDP sockets and certificate-verifying clients. Coverage includes protocol
negotiation, QUERY forwarding, WebSocket tunnels, static delivery, body limits,
timeouts, reload, certificate rotation, cached ACME, connection caps and shutdown.
The optimized build succeeded; existing unused-code compiler warnings remain.

The installed Windows curl lacks HTTP/2 and HTTP/3, so these results do not
constitute independent curl/browser HTTP/3 interoperability verification.
Remote Linux/Windows CI execution remains unverified.

### Benchmark tooling and recorded baseline

- Added a bounded-memory JSON load generator and deterministic test fixtures.
- Added matched Docker comparison profiles and a Windows runner with CPU/RSS sampling.
- Recorded an earlier Windows loopback baseline for 1 KiB, 64 KiB and 4 MiB files,
  with three repetitions and no measured request failures.

That baseline is not a production comparison or a benchmark of the latest HTTP/3
update. No measured Nginx/Apache speed advantage is claimed.

References: [Benchmark suite](benchmarks/README.md),
[local baseline](benchmarks/LOCAL_BASELINE.md), [Docker setup](dockerflashlin/README.md)

## 8. Remaining work before a production release

- Independent HTTP/3 browser/client interoperability and lossy-network soak testing.
- Real-domain ACME issuance and renewal validation.
- Independent SNI certificates for different hosted domains.
- Fully streamed uploads, active upstream health checks and PHP-FPM pooling.
- Expanded observability/tracing, log rotation and cache management.
- Broader security review, dependency/SBOM review and compatibility testing.
- Remote cross-platform CI validation and production benchmarks against matched workloads.
- Signed release artifacts and an explicitly approved production release.

Local source changes and builds have not published or updated public releases,
download installers or container images.

Detailed change history: [CHANGELOG.md](CHANGELOG.md)
