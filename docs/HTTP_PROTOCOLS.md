# HTTP/1.1, HTTP/2 and HTTP/3 in FlashLin v2

HTTP/3 is implemented as an **optional, explicitly experimental origin
listener** using [h3](https://github.com/hyperium/h3) and
[Quinn](https://docs.rs/quinn/0.11.11/quinn/), off by default and requiring
`--features http3`. Enabling it is not a claim of a measured speed advantage
over Nginx / Apache; see [benchmarks/README.md](../benchmarks/README.md).

## Protocol matrix

| Protocol | Connection | Build | Discovery / negotiation |
|---|---|---|---|
| HTTP/1.1 | TCP, plaintext or HTTPS | Default; `tls` for HTTPS | Normal HTTP or TLS ALPN `http/1.1` |
| HTTP/2 | TCP, plaintext prior knowledge or HTTPS | Default; `tls` for HTTPS | h2c prior knowledge or TLS ALPN `h2` |
| HTTP/3 | QUIC over UDP, TLS 1.3 required | `http3` (includes `tls`) | TLS ALPN `h3`; HTTPS TCP responses advertise `Alt-Svc` |

HTTP/2 cleartext `Upgrade: h2c` is not implemented. WebSocket reverse-proxy
tunnels currently use HTTP/1.1 Upgrade only. HTTP/2 / HTTP/3 extended CONNECT,
WebTransport, HTTP datagrams, server push, outgoing HTTP/3 proxy connections and
0-RTT requests are not implemented/enabled. HTTP/3 CONNECT is rejected rather than
treated as a normal application request. Active QUIC address migration is disabled
so IP access/rate policies remain tied to the established peer address.

## Build and configure

Tested toolchain: Rust 1.95. Use a current stable toolchain with a C compiler for
the TLS cryptography dependency. Keep Cargo.lock for reproducible dependency versions.

```sh
# Small HTTP/1.1 + HTTP/2 build
cargo build --release --locked
# All three protocols, manual certificates
cargo build --release --locked --features http3
# All three protocols plus automatic certificates
cargo build --release --locked --features acme,http3
```

Start from [examples/http3.toml](../examples/http3.toml). Supply your certificate
chain and private key **outside the document root**, then run:

```sh
flashlin --check examples/http3.toml
flashlin --watch examples/http3.toml
```

The example serves HTTPS HTTP/1.1 and HTTP/2 on TCP 443 and HTTP/3 on UDP 443.
It does not open a plaintext companion listener because `tls.http_port` equals
`server.port`. To add HTTP on TCP 80, set `tls.http_port = 80`. The example does
not contain certificates and therefore intentionally fails validation until you
provide them. Running on low ports may require appropriate OS privileges.

```toml
[http3]
enabled = true
# Omit port to use server.port; this must be the externally reachable UDP port.
port = 443
max_concurrent_streams = 32 # per connection, range 1..=128
```

Open **both TCP 443 and UDP 443** in the firewall, cloud security rules and load
balancer. TCP-only proxies cannot forward QUIC. Docker needs both
`-p 443:443/tcp -p 443:443/udp`; `EXPOSE` alone does not publish ports. If using a
different UDP port, forward it without port translation to match `[http3].port`.
The source Dockerfile now includes `acme,http3`, but HTTP/3 stays disabled until
configured. Previously published images are not changed by local source edits.

HTTPS TCP responses advertise `h3=":443"; ma=300` (using the configured port).
Plain HTTP responses do not receive this automatic advertisement. Clients can
continue using HTTP/1.1 or HTTP/2 when QUIC is unavailable. Browsers commonly need
an initial HTTPS request before discovering HTTP/3. Turning off HTTP/3 can leave
a cached advertisement for up to five minutes; keep TCP HTTPS working.

For automatic certificates, replace manual cert/key with the documented
`[tls.acme]` settings in [automatic HTTPS](AUTOMATIC_HTTPS_AND_RELOAD.md).
ACME TLS-ALPN-01 validation still requires public **TCP 443**. HTTP/3 uses the same
certificate resolver; it does not implement a separate CA challenge or accept CA
terms automatically. Live public issuance/renewal remains deployment validation work.

## Shared behavior and limits

All three protocols enter the same router: virtual hosts, IP rules, per-host rate
limits, path checks, `.flashweb`, cache, static file streaming, ranges, HEAD,
compression, reverse proxy, PHP dispatch and QUERY processing are not separate
HTTP/3 reimplementations. QUERY bodies are limited and forwarded to the backend;
the server does not invent query semantics or cache responses by URL alone.

HTTP/3 adds source-address validation before handshake work, a ten-second handshake
deadline, header / body read deadlines, response write-idle deadlines, QUIC idle
timeout, per-connection stream limits and bounded transport flow-control windows.
Malformed Content-Length framing resets the affected stream. Hop-by-hop response
headers are removed before HTTP/3 transmission. Shutdown sends GOAWAY and allows
up to 30 seconds to drain; connections still active at the deadline are closed.

`server.max_connections` is shared across TCP and QUIC. QUIC overload refuses the
connection; it cannot send an HTTP 503 before establishing HTTP/3. Set a finite
connection cap and an appropriate `security.max_body_size`: application request
bodies are still buffered, so concurrent uploads can consume roughly the body
limit multiplied by active requests, in addition to transport/cache memory.
HTTP/3 defaults to 32 concurrent request streams to bound that multiplication.

Hot reload updates routing for new requests on existing QUIC connections. New
handshakes use reloaded manual certificates or the current automatic resolver.
Changing `[http3]`, listening ports or the global connection cap requires restart;
invalid reloads keep the previous configuration. Transport settings already copied
into an established connection do not change in place.

## Verification and remaining release work

Local Windows verification on 2026-09-14, Rust 1.95: 51 HTTP-only tests,
52 TLS tests, 56 HTTP/3 tests and 58 all-feature tests passed. These are
overlapping feature configurations. The optimized all-feature Windows build
also succeeded; existing unused-code compiler warnings remain.

```sh
cargo test --locked --no-default-features
cargo test --locked --features tls
cargo test --locked --features http3
cargo test --locked --all-features
```

The protocol suite uses real loopback TCP/UDP sockets with certificate validation,
and covers ALPN, shared TCP/UDP port use, Alt-Svc, HTTP/3 file streaming, ranges,
HEAD, conditional responses, path blocking, upload limits/timeouts, concurrent
streams, QUERY forwarding, malformed framing, reload, certificate rotation,
cached-ACME QUIC, shared connection limits and response delivery during shutdown.
Existing HTTP/1.1 / HTTP/2 / WebSocket regression tests remain part of the suite.

For deployment verification, use a curl build listing `HTTP2` and `HTTP3` in
`curl --version`, with a trusted certificate:

```sh
curl --http1.1 -I https://your-domain.example/
curl --http2 -I https://your-domain.example/
curl --http3-only -I https://your-domain.example/
```

The installed Windows curl used during development lacks HTTP/2 and HTTP/3, so it
is not an independent HTTP/3 interoperability test. Browser/curl interoperability,
lossy-network soak/load tests, fresh public ACME, Linux/remote CI, security review
and production benchmarks are still required before a production release. HTTP/3
support alone does not establish faster page loading on a particular workload.
