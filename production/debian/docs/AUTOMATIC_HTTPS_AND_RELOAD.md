# Automatic HTTPS and hot reload

## Automatic certificates (optional `acme` build feature)

Build with `cargo build --release --locked --features acme`. This includes TLS;
HTTP-only and manual-TLS builds do not include the ACME client. Flashlin uses
rustls-acme for Let's Encrypt issuance, cached account/certificate loading,
automatic renewal and retry backoff. Certificates are installed through a live
resolver; renewal does not restart the listener.

Start from [automatic-https.toml](../examples/automatic-https.toml). Replace the
example domains and email. Read the [Let's Encrypt subscriber agreement](https://letsencrypt.org/repository/)
before explicitly setting `accept_terms = true`. `--check` does not contact a CA,
accept terms or issue a certificate; the template intentionally fails validation
until consent is configured. Keep `production = false` for staging first. Staging
certificates are not browser-trusted. Set `production = true` only when ready.

All configured public DNS names must resolve to this server. TLS-ALPN-01 validation
requires public TCP port 443 to reach this listener, including correct IPv6 routing
if an AAAA record exists. Port mapping is allowed. A TLS-terminating proxy/CDN in
front must not intercept validation. HTTP port 80 is optional for redirects, not
the ACME challenge. Wildcard certificates and DNS-01 are not supported.

An ACME configuration manages one certificate covering its domain list (up to 100
names). It is not independent per-host SNI certificate management. Manual cert/key
and ACME are mutually exclusive. Changing ACME account/domains/cache/staging mode
requires a restart. Before initial issuance, ordinary HTTPS handshakes may fail;
check lifecycle logs for deployment of a cached or newly issued certificate.

Keep `cache_dir` outside **every** served document root, persist it between starts,
and protect backups: it contains private certificate and account keys. Configuration
validation rejects cache directories inside configured public roots. Unix creates
the directory with mode 0700 and rejects an existing group/world-accessible cache.
On Windows, set a private directory ACL for the service identity and administrators
before use. Do not expose these files through separate file servers or symlinks.
Account/cache write failures are logged; correct permissions promptly so restarts
do not cause needless re-issuance. Use a persistent container volume at
`/var/lib/flashlin/acme` with owner UID 1001 and mode 0700.
The default `.flashlin` cache is excluded from Git and Docker build contexts.
If you choose another cache path, exclude that location from source control and
build contexts too.

The automated offline test verifies cached certificate loading, a trusted TLS
handshake and HTTP/2 ALPN, plus verified HTTP/3 with the `http3` feature. Fresh public issuance and renewal against a real CA
still require deployment-domain validation; they have not been exercised here.

## Live configuration and manual certificates

```text
flashlin --check server.toml
flashlin --watch server.toml
```

`--watch` checks every two seconds on Windows and Unix. Normal startup without
`--watch` is unchanged. For the container entrypoint set `FLASHLIN_WATCH=1`.
It reloads the main TOML, virtual-host TOMLs (including additions/removals), routes,
static mounts, security rules, cache/compression settings, PHP settings, proxy
groups and manual certificate/key files. Paths other than `domains_dir` remain
relative to the process working directory. The domains directory is relative to
the main configuration file.

A complete valid generation replaces the active configuration and manual TLS
acceptor together. Invalid TOML, duplicate hosts, missing domain directories,
invalid certificates or mismatched keys preserve the previous generation. Errors
are logged without repeatedly printing an unchanged error. Replace edited files
atomically; a multi-file deployment is not a filesystem transaction, so publish
the main configuration last when coordinating multiple changes.

Each new request, including requests on existing keep-alive connections, reads the
latest generation. In-flight requests retain their old settings until completion.
Cache/rate-limit/upstream state resets on a successful reload; old state is freed
when old requests finish. Frequent reloads reset rate-limit history and can briefly
retain both generations' cache memory. Avoid reload loops during peak traffic.

Transport limits (socket/header timeouts, keep-alive and header buffer size) take
effect on new connections; existing connections retain their original transport
settings. A refreshed certificate is used for new TLS handshakes, not existing
sessions. Listener address/ports, worker count, maximum connections, logging
configuration, TLS mode/redirect settings and ACME settings require a restart;
changes to those reject the entire attempted reload.

`.flashweb` retains its separate up-to-60-second cache behavior; it is not part of
this two-second watcher. Separate per-host manual SNI certificates remain future
work. This implementation does not claim atomic deployment across multiple files
or changes to already-running application processes.
