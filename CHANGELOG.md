# Changelog

## 2.0.0 — in progress

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

### Still required before a production v2.0.0 release

- Fully streamed uploads, active health checks and PHP-FPM pooling.
- Real-domain ACME issuance/renewal validation and independent SNI certificates.
- Observability/tracing and broader integration/security test coverage.
- Production benchmark comparisons, SBOM/dependency review and signed artifacts. Cross-platform
  regression CI has been added but has not yet run on a remote host.

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
