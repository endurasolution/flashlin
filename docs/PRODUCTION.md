# Production readiness

This is a checklist and a status report, not a certification. It exists
because two opt-in flags — `server.sendfile` and `server.direct_static_io` —
were added and benchmarked in one working session, and that is not, by
itself, a reason to trust them with real traffic. Read this before enabling
either one outside a controlled environment.

## Status summary

| Area | Status |
| --- | --- |
| Core routing, TLS, PHP, proxying, config reload, security checks | Pre-existing; see [CHANGELOG.md](../CHANGELOG.md) for what's still outstanding for v2.0.0 generally. |
| `server.direct_static_io` (default off) | New. Small, reviewed change to an already-tested code path (still calls the same `prepare_static` function; only the executor hop is skipped). Size-gated to ≤16 KiB to avoid stalling other requests on the same worker thread. |
| `server.sendfile` (default off, Linux only) | New. A custom, from-scratch HTTP/1.1 request parser and response writer that bypasses Hyper for eligible requests. Unit-tested, loopback-integration-tested, and manually security-reviewed (see below) in this session. **Not** fuzz-tested, not soak-tested, not exercised against adversarial or slow clients, and has had exactly one reviewer. |

Both flags default to `false`. Nothing about enabling them is required to run
FlashLin; they exist to accelerate one specific case (plain-HTTP static
GET/HEAD) once you've decided the tradeoffs below are acceptable for your
deployment.

## What `server.sendfile` actually changes

When enabled, on Linux, for connections with no virtual hosts configured,
eligible requests skip Hyper's connection handling entirely and are served
by a purpose-built loop in `src/server/fastpath.rs`: it parses the request
with `httparse` (the same parser Hyper uses internally), and for a plain GET
or HEAD with no body, no `Range`, and a resolvable static file, writes the
response headers directly and streams the body with `sendfile(2)` — a true
zero-copy kernel-to-socket transfer.

Everything the fast path can't cleanly decide — TLS, virtual hosts, a
request body, `Range`, a compression preference, `.flashweb`/`.htaccess`
rewrites, PHP, proxy routes, anything it fails to parse — falls back
immediately to the ordinary Hyper-based handler, with every byte already
read from the socket replayed intact. **Enabling `sendfile` can only add a
faster code path for a narrow case; it cannot remove functionality**, because
anything outside that narrow case is handled exactly as before.

It reuses the existing security primitives rather than reimplementing them:
`safe_join`/`validate_url_path` for path containment, `open_file_within_root`
for symlink-escape verification (the same procfs-`readlink` check
`prepare_static` uses), `check_ip_access`, the existing rate limiter, and
`is_sensitive_path`/dotfile blocking. If you find a bug in one of those, it
affects both paths identically — that's intentional.

## Manual security review (this session)

No prior git history existed for this repository, so the automated
`/security-review` tool (which diffs against a base branch) had nothing to
diff against. A manual review of `src/server/fastpath.rs` was done instead,
covering:

- **Path traversal / symlink TOCTOU** — confirmed the fast path calls the
  same containment and procfs-based symlink-verification functions the
  existing static-file path uses; no new resolution logic was written.
- **Header/response injection** — request header values are validated by
  `hyper::header::HeaderValue::from_bytes` (rejects control characters)
  before use; no request-controlled bytes are echoed into response headers.
- **Request smuggling** — any request with `Content-Length` or
  `Transfer-Encoding` is rejected outright (falls back), so this path never
  interprets a body boundary itself.
- **Panic reachability** — `Cargo.toml` sets `panic = "abort"`, so *any*
  reachable panic kills the whole process, not just one task. Audited every
  `unwrap`/`expect`/slice-index/arithmetic op reachable from request input;
  found none that panic on attacker-controlled data. The one `unsafe` block
  (the `sendfile(2)` FFI call) was checked for pointer/fd validity.
  (`Ok(n) => remaining.saturating_sub(...)` hardens the one arithmetic op
  that theoretically depends on the kernel honoring `sendfile`'s contract.)
- **Rate-limiter double-accounting** — found and fixed: the limiter is now
  checked *last*, immediately before committing to serve, so a request that
  falls back after passing it is never charged twice.
- **Configured header-size limit** — found and fixed: the fast path now
  derives its request-head buffer cap from `security.max_header_size`
  (matching `connection.rs`'s own clamp) instead of a fixed constant, so a
  deliberately lower operator-configured limit is actually honored.
- **Resource bounds** — request head reads are capped (see above); a stalled
  write during `sendfile` is bounded by `write_timeout`; an idle connection
  between requests is bounded by `keep_alive_timeout`. No new unbounded-growth
  path was introduced.

What this review did **not** do: fuzzing, extended soak testing, testing
against deliberately malformed/pipelined/slow-drip clients, or a second
independent reviewer. Treat the findings above as "no obvious bugs found by
one careful reading and 78 passing tests," not as "proven correct."

## Before enabling either flag in production

1. **Leave both off by default and stage the rollout.** Enable on a canary
   instance or a fraction of traffic first; compare error rates, p99 latency
   and RSS against the baseline before widening.
2. **`sendfile` only applies to plain HTTP with no virtual hosts.** If you
   terminate TLS at FlashLin or run multiple domains from one process, this
   flag currently does nothing for that traffic — there's no risk from
   enabling it, but also no benefit. It's meant for a plain-HTTP origin
   behind a TLS-terminating proxy/CDN, or a single-domain internal service.
3. **Only enable `direct_static_io` when the document root is fast/OS-cached
   local storage** (SSD with a resident working set, tmpfs). A slow read
   (cold disk, network filesystem, contended remote storage) blocks the
   worker thread serving it, stalling other connections sharing that thread.
4. **Soak test before trusting either flag under sustained load.** The
   benchmark evidence below is a handful of 5-second bursts per data point,
   not hours of realistic mixed traffic. Watch for file-descriptor growth,
   RSS drift and tail-latency creep over an extended run.
5. **Test with your actual traffic shape**, not just single-file-type static
   GETs: mixed content, real client `Accept-Encoding`/`Range`/keep-alive
   behavior, slow/flaky clients, and whatever sits in front of FlashLin
   (load balancer, CDN, health checker).
6. **Monitor `/metrics` (loopback-only Prometheus endpoint)** for status-code
   distribution and response counts; watch process RSS and open-fd count
   independently, since the fast path's memory profile (near-zero per-request
   buffering) is a meaningful behavioral change worth confirming in your
   environment.
7. **Know how to roll back**: both flags are plain config booleans. Setting
   either back to `false` and restarting (or reloading via `--watch`, for
   settings that support hot reload — check `src/server/reload.rs` for which
   fields apply without a restart) returns to the pre-existing, more
   battle-tested code path immediately.

## Benchmark evidence and its limits

A CPU-isolated Docker/Ubuntu harness (`benchmarks/ubuntu/`) ran nginx, Apache
and Flashlin under identical settings — same two pinned server CPUs, same
warm page cache, same plain-HTTP/1.1 keep-alive client, sendfile enabled on
nginx/Apache for a fair comparison. With both flags enabled, results in
`benchmarks/ubuntu-results/run8/REPORT.md` show 4 MiB static-file throughput
moving from roughly half of nginx/Apache's to matching or exceeding both
(+9.5% vs. nginx at concurrency 64), with peak RSS under concurrent large-file
load dropping from up to 775 MiB to 7.4 MiB. Small-file (1 KiB) throughput
still trails nginx by 37–46% at high concurrency.

This is real, reproducible evidence of the mechanism working — but per the
existing [benchmarks/README.md](../benchmarks/README.md), it is a closed-loop,
warmed-cache, single-machine, multi-hour-old-code result. It does not by
itself establish production capacity, and it says nothing about TLS traffic,
cold caches, sustained load, or behavior under real network conditions —
all of which are exactly what the steps above are for.
