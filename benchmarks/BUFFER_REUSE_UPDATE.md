# Buffer reuse and request allocation update

## Code changes

- `src/static_files/file_io.rs`: reuse full 1 MiB streaming buffers instead of
  allocating and zeroing a new large buffer for every disk read. `Bytes` owns
  each buffer until its last transport reference is released. Every byte must
  be overwritten by a successful exact-length read before reuse is exposed.
  Small reads and response-cache fills retain their existing allocation path.
- The pool retains at most 32 idle buffers (32 MiB). Active transfers still need
  their own buffers. Pool access uses a nonblocking lock attempt; contention
  falls back to allocation or disposal. This trades retained memory for less
  allocation work, with no new dependency or unsafe Rust.
- `src/http/router.rs`: single-host requests borrow the default state key and
  skip unnecessary Host parsing; rate-limiter hits use a read lookup instead
  of allocating a key and acquiring a map entry for each request.
- `src/static_files/path_security.rs`: check ASCII device names without making
  an uppercase string for each path segment. Non-ASCII names retain the
  existing Unicode case-mapping behavior.

File reads remain demand driven. File resolution, metadata validation, response
cache policy, ranges, conditional requests, compression and networking timeouts
continue through their existing paths. This is not a sendfile implementation.

## Verification

`cargo test --all-features --bin flashlin --target-dir target/v2-check`:
**68 passed, 0 failed**. New checks cover last-reference buffer ownership,
reuse, idle-memory bounds, truncation without returning stale data, and device
name boundaries. Existing integration checks cover HTTP/1, HTTP/2, TLS, HTTP/3,
ranges, parallel transfers, file changes, proxying and reloads.

The release build also passed. Existing unused-code/import warnings remain.

## Local measurements — Windows only

Both builds used two networking workers, concurrency 32, HTTP/1 keep-alive,
identity encoding, no response cache and no compression. Three alternating
repetitions per build used three seconds of warm-up and ten seconds of measured
load. The same load-generator executable verified status and full body length.
Server and client shared this Windows workstation; CPU placement was not isolated.

The initial all-file comparison experienced large variation even within the
unchanged build. Its results are retained here rather than treated as proof of
either improvement or regression:

| File | Before median req/s (range) | After median req/s (range) |
| --- | ---: | ---: |
| small.txt | 2,703.3 (1,831.6–5,209.1) | 1,962.1 (1,852.1–4,681.3) |
| medium.txt | 3,787.9 (1,893.5–4,210.4) | 1,798.2 (1,662.2–3,900.8) |
| large.bin | 362.3 (189.3–395.0) | 286.9 (218.3–387.4) |

[All-file raw reports](results/comparison-20260915-220434.json). Errors were zero.
Small- and medium-file gains are unproven; these measurements also cannot rule
out regressions. The code removes allocations, but that alone does not establish
an end-to-end throughput gain.

Because of that variation, a separate focused large-file comparison followed:

| Metric | Before | After |
| --- | ---: | ---: |
| Median req/s | 187.6 | 233.0 |
| Req/s range | 185.6–205.0 | 220.2–242.1 |
| Median per-run p99 ms | 289.14 | 227.99 |
| Peak sampled working set, MiB | 43.6 | 45.4 |
| Errors | 0 | 0 |

The focused throughput ratio was **1.24×**. This is a local observation from
three repetitions, not a Linux performance prediction or a cross-server result.
Working-set samples were taken every 250 ms and include warm-up; retained pool
memory is part of the tradeoff. [Focused raw reports](results/comparison-20260915-220630.json).

Reproduce the focused comparison with saved binaries:

```powershell
./benchmarks/compare-native.ps1 -BeforeDirectory target/perf-round2-before/release -AfterDirectory target/perf-check/release -Seconds 10 -Repeats 3 -Concurrency 32 -File large.bin
```

- Before server SHA-256: `CF7FFB07E8AA71D62A42394A5C9B932EFA03CBD26135CC27388E5C818AB0EBF3`.
- After server SHA-256: `6E390EC1CD71CC281D026BB670D005BCF24EA13C5B32C94E751A0D9692488F12`.
- Shared load-generator SHA-256: `27AB5A9B3C210BCA89D59C9C891767C536687C011037FEB9519EB2E2F20CD3B8`.
- Source did not change after these measurements. Benchmark processes exited.

## Build on Linux

Apply the updated source files to the matching Flashlin checkout, then build
and test before rerunning the same comparison:

```bash
cargo test --locked --all-features --bin flashlin
cargo build --release --locked --features acme,http3 --bin flashlin --example benchmark
```

Keep the old binary for an alternating old/new comparison. Use identical
configuration, worker counts, fixtures and load-generator executable. Report
medians, spread, errors and memory. Linux/nginx/Apache performance remains to
be measured on the user's host; no 5× cross-server advantage is claimed.
