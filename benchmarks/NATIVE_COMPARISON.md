# Native FlashLin vs Nginx vs Apache comparison — 2026-09-15

Single-machine loopback comparison, not a production certification. This is the
first direct three-way comparison run for the project; prior recorded numbers
(`LOCAL_BASELINE.md`) covered FlashLin alone, on different (Windows) hardware.

Two FlashLin builds were measured on the same host in the same session: the
v2.0 source as it stood that morning ("before"), and the same source after a
round of static-path optimisation ("after"). Nginx and Apache numbers are from
the same runs and were not tuned between them.

## Result

Three FlashLin builds were measured across two runs. **Raw numbers are not
comparable between runs**: Nginx and Apache were unchanged and untouched
between run 1 and run 2, yet both measured almost exactly 2.0x faster in run 2,
so the host itself roughly doubled in throughput (VM contention or frequency
scaling — machine isolation was not controlled). Only same-run ratios mean
anything here.

### Run 1 — baseline vs first optimisation pass

| File | FlashLin v2.0 | FlashLin opt-1 | Nginx | Apache |
|---|---:|---:|---:|---:|
| 1 KiB | 2,948.7 | 5,292.3 | 16,116.0 | 8,722.2 |
| 64 KiB | 2,480.0 | 3,766.7 | 10,979.8 | 6,839.8 |
| 4 MiB | 155.0 | 241.6 | 645.5 | 621.2 |

### Run 2 — second optimisation pass (different host speed, see above)

| File | FlashLin opt-2 | Nginx | Apache |
|---|---:|---:|---:|
| 1 KiB | 14,316.6 | 32,487.0 | 17,531.2 |
| 64 KiB | 8,919.5 | 23,825.2 | 13,785.9 |
| 4 MiB | 441.0 | 1,509.2 | 1,305.8 |

p99 latency, opt-2: 4.80 ms / 7.56 ms / 125.86 ms. Zero request failures in
every reported measurement.

### FlashLin as a fraction of each competitor, within its own run

| File | vs Nginx (run 1 → run 2) | vs Apache (run 1 → run 2) |
|---|---|---|
| 1 KiB | 33% → 44% | 61% → **82%** |
| 64 KiB | 34% → 37% | 55% → 65% |
| 4 MiB | 37% → **29%** | 39% → **34%** |

Dividing out the ~2.0x host speedup, the second pass is worth roughly 1.34x on
1 KiB, 1.13x on 64 KiB, and nothing measurable on 4 MiB (nominally 0.82x, which
is within the noise of three repetitions but certainly not an improvement).

**FlashLin remains slower than both Nginx and Apache at every size tested.** It
is now within 18% of Apache on 1 KiB files, which is the closest it has come.
Large-file throughput did not benefit and lost ground relative to both.
No "faster than Nginx/Apache" claim is supported by any of this data.

## What changed between the FlashLin builds

The static path was doing roughly 20 filesystem operations per request, 7–8 of
them `tokio::fs` calls that each cost a blocking-pool thread hand-off. Nginx
issues about 4 syscalls inline for the same request.

- Canonical document roots memoised. `safe_join` ran a `realpath` walk on each
  of its ~5 calls per request.
- `.htaccess` / `nginx.conf` / `redirects.conf` lookups cached, absence
  included — previously three failed `open` calls on every request.
- Three redundant stat/canonicalize calls removed from `serve_static_cached`;
  metadata is now carried forward rather than re-read.
- Files up to 256 KiB are served as one buffered read instead of through a
  `ReaderStream`; the stream chunk for larger files went 64 KiB → 256 KiB,
  cutting blocking hand-offs on a 4 MiB file from ~64 to ~16.
- ETag switched from a per-request SHA-256 to hex size-mtime.
- The process-wide mutex was removed from response-cache reads, where it had
  been serialising every concurrent read and defeating the sharded map.
- Per-request allocations removed: UUIDv4 request id → atomic counter, two
  full `HeaderMap` clones, a `format!` per route in route matching, and a
  rate-limiter lookup that ran even when rate limiting was disabled.

Second pass (opt-2), targeting the syscalls the first pass left behind:

- Resolution cache: a path confirmed to be a symlink-free regular file inside
  its root is remembered for 5 seconds, skipping both the `stat` and the
  `realpath`. The `open` that follows remains authoritative, so a deleted file
  still 404s immediately. Paths that resolve through a symlink are never
  cached, but a real file *replaced* by an outward-pointing symlink could be
  followed for up to the TTL — that requires write access to the document
  root, which is already a compromise, but it is a reduction in the guarantee.
- The three remaining per-request stats (directory-index probe, trailing-slash
  redirect check, rewrite-rule directory walk) consult that cache and skip
  themselves for known files.
- Open path restructured to a single `open` + `fstat`, with compressed-sidecar
  selection moved after the open rather than before it.
- Content-type interned by extension; the cache key for the response cache is
  now built only when the cache is enabled; body-less requests no longer
  register a read timer; `is_sensitive_path` compares in place instead of
  lower-casing the path into a new allocation; percent-decoding reuses the
  existing string when the path contains no escapes.

## Method

- Host: remote Ubuntu 26.04 LTS VM, server and load generator on the same
  machine (loopback) — they compete for the same CPU cores. Exact CPU/RAM
  specs of this host were not captured.
- FlashLin built from source: `cargo build --release --locked --features acme,http3`.
- All three servers run as standalone (non-systemd) instances on adjacent
  ports (FlashLin :18082, Nginx :18083, Apache :18084), 2 worker
  processes/threads each, serving byte-identical fixture files from the same
  directory. Compression, response caching and access logging disabled on all
  three. TLS was not tested.
- Fixtures: deterministic 1 KiB / 64 KiB / 4 MiB files.
- Load generator: the project's own `examples/benchmark.rs` — closed-loop,
  HTTP/1.1 keep-alive, identity encoding, verifies status and exact body
  length, 3-second discarded warm-up, 10-second measurement, concurrency 32,
  3 repetitions per file/server. No coordinated-omission correction.
- Nginx `sendfile on`, no gzip. Apache `EnableSendfile On`, MPM event,
  `ServerLimit 2` / `MaxRequestWorkers 128`. Both serve static files through
  their C-level `sendfile(2)` path.

### Harness hazard found during this work

An earlier run of this comparison reported 70–97% FlashLin request failures.
That was **not** a server defect: stale FlashLin processes from previous runs
were still bound to the test port. FlashLin sets `SO_REUSEPORT`, so a second
process binds the same port silently and the kernel splits connections between
them, rather than failing with "address already in use" the way Nginx does.
The readiness check happened to land on the healthy process and passed.

The harness now sweeps stale processes and asserts a single listener per port
before measuring. This is also worth treating as an operational hazard in
FlashLin itself: an accidental second instance degrades service invisibly
instead of refusing to start.

## Why the remaining gap exists

Nginx and Apache land within 4% of each other on the 4 MiB fixture (645.5 vs
621.2), which suggests both are bounded by the same kernel/page-cache path
rather than by their own request handling. FlashLin at 241.6 still copies file
bytes through userspace. Closing that specific gap requires `sendfile(2)` or
`io_uring`; no amount of allocation tuning will substitute for zero-copy.

The small-file gap is different in character and is the more winnable one. This
comparison ran with FlashLin's response cache **disabled**, for parity with the
other two servers, so its in-memory cache — the feature most likely to beat an
`open`+`fstat`+`sendfile` sequence — was never exercised. A cache-enabled
profile is the obvious next measurement, though it would no longer be a
like-for-like static-serving comparison and must be reported as such.

## Caveats

- **Host throughput changed ~2x between run 1 and run 2.** Nothing in this
  report should be read as a before/after improvement across runs; use the
  within-run ratios. A valid A/B needs both builds measured back-to-back in
  one session, in rotating order, which has not been done.
- Single host, 3 repetitions, medians only; no cross-run variance reported.
- Server and load generator shared CPU cores. Real capacity numbers need a
  fixed-arrival-rate generator on a separate machine.
- No TLS, compression, proxy, PHP, sustained-load or cold-cache scenarios.
- Static-file serving only. Says nothing about FlashLin's reverse-proxy,
  WebSocket, QUERY or PHP-FPM performance relative to the other two.

Raw per-run JSON is on the test host at
`~/flashlin/benchmarks/results/native-20260915-103852/raw.jsonl`.
