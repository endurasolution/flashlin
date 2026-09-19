# Fast delivery in FlashLin v2

The v2 delivery path uses bounded streaming, reusable upstream connections and
precompressed assets. These reduce avoidable allocation, CPU work and backend
connection setup. They do not establish a performance advantage over Nginx or
Apache without a comparable benchmark.

## Static pages and assets

- Files larger than 1 MiB, or excluded by the RAM cache policy, stream with a
  64 KiB file reader. This is a per-reader buffer, not total connection memory.
- Small cache entries share immutable byte buffers. The configured cache budget
  limits resident payload bytes, excluding metadata and in-flight responses.
- File metadata is checked before cache lookup. Changed files use new keys;
  old entries expire or are evicted. Metadata checks are intentional for freshness.
- `HEAD`, conditional requests, and single byte ranges avoid unnecessary data
  transfer. Ranges support explicit, open-ended and suffix offsets plus `If-Range`.
  Multiple ranges are ignored and receive the full representation.
- Put `app.js.br` and/or `app.js.gz` beside `app.js`. FlashLin negotiates these
  when the compressed file is at least as new as the original, and honors q=0.
  The original file must exist; deploy source and sidecars together.
- Dynamic compression handles buffered responses up to 1 MiB. It runs on blocking
  workers with at most min(CPU count, 8) jobs. When jobs are saturated, identity
  content is served. Streaming responses pass through without recompression.
- Fingerprint assets (for example `app.a17e9.js`) before assigning a year-long
  immutable cache lifetime. Revalidate HTML with `Cache-Control: no-cache`.

## Proxy and application delivery

`proxy_pass` uses a shared connection pool (16 idle connections per origin,
60-second idle eviction). HTTP framing and chunk decoding are handled by Hyper.
Upstream responses stream with downstream backpressure; SSE is not fully buffered.
Requests are still buffered up to `security.max_body_size`, including proxy
uploads. Size this limit together with your concurrency limit.

Build with `--features tls` for HTTPS upstreams. Certificate verification uses
bundled WebPKI roots; custom private roots are not yet configurable. HTTP-only
builds reject HTTPS upstream configuration. Upstream connection establishment
has a 10-second limit; response headers and body idle waits use `read_timeout`.

Use either `proxy_pass` or a list:

```toml
[[routes]]
path = "/api"
proxy_upstreams = ["http://127.0.0.1:3000", "http://127.0.0.1:3001"]
php_enabled = false
```

Backends rotate round-robin. 502/503/504 results cause a 5-second passive cooldown;
empty-body GET/HEAD and buffered QUERY requests may try up to three eligible backends. Writes and
WebSocket upgrades are never automatically retried. This is passive failure
detection, not a scheduled active health probe. If every backend is cooling down,
one is probed for recovery.

Proxy URLs with a base path prepend it to the original request path. For example,
`http://backend:3000/service` plus `/api/x` forwards `/service/api/x`. The escaped
request URI and query are preserved; this is not full Nginx proxy_pass rewrite
compatibility. Configure application routing accordingly.

HTTP/1 WebSocket upgrades tunnel bidirectionally (up to 1,024 concurrent tunnels
per process). Socket idle timeouts apply to downstream tunnel traffic too. HTTP/2
extended CONNECT is not implemented. Forwarded client IP/protocol headers are
replaced from the actual connection; a configurable trusted-proxy chain is pending.

See [QUERY and WebSocket setup](QUERY_AND_WEBSOCKETS.md) for route examples,
QUERY discovery/CORS, request-body limits and caching/replay behavior.

## Operating the server

Start with [the performance profile](../examples/performance.toml), and measure
under representative traffic. Paths remain relative to the working directory.
`flashlin --check path/to/server.toml` (or `-t`) validates before binding a port;
invalid or missing configuration stops startup. `logging.level` and `logging.json`
are honored. File log destinations/rotation remain pending.

`read_timeout` bounds HTTP/1 headers and request-body collection. HTTP/2 allows
128 concurrent streams per connection and caps header lists. `keep_alive_timeout`
and `write_timeout` bound pending socket reads/writes. TLS handshakes expire after
10 seconds. SIGTERM/Ctrl-C stops accepting and drains ordinary connections for up
to 30 seconds. Long-lived upgraded tunnels may end at process shutdown.

Accepted sockets enable TCP_NODELAY, and the timeout wrapper preserves vectored
writes when the underlying transport supports them. These avoid unnecessary
small-write delays and preserve Hyper's scatter/gather write path; their impact
depends on workload and must be measured, not assumed.

`/metrics` is restricted to loopback clients. Scrape it using a local monitoring
agent; a `.flashweb` rule cannot grant remote access to this built-in endpoint.

## Verification and remaining release work

The TCP regression test covers small-file cache hits, file-change freshness,
HEAD, range/suffix/416/If-Range, large streamed transfers, mounted-directory
redirects, precompressed and dynamic gzip, q=0, header forwarding, upstream pool
reuse, passive failover, early SSE delivery, WebSocket tunneling and slow headers.
Run `cargo test --all-features` and `cargo test --no-default-features`.

Use a release build for benchmarks. Record throughput, p50/p95/p99 latency,
errors, peak memory and CPU under the same TLS, compression, cache state, files,
hardware and concurrency for every server. No cross-server speedup is claimed.

Automatic certificates and opt-in configuration/certificate reload are implemented;
see [operating guide](AUTOMATIC_HTTPS_AND_RELOAD.md). The offline tests cover cached
ACME certificates and live keep-alive reload, not fresh public CA issuance.

Use the [reproducible benchmark suite](../benchmarks/README.md) for controlled
Nginx/Apache comparisons. Local loopback measurements are not production results.
The [Windows baseline report](../benchmarks/LOCAL_BASELINE.md) contains measured
throughput, tail latency, CPU, memory and raw data for this development build.

The v2 release is still in development: independent SNI certificate selection,
fully streamed uploads, PHP-FPM pooling, active health probes, broader HTTP
compliance/security testing, real-domain ACME validation and production benchmark
comparisons remain release work.
