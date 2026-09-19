# Medium-buffer reuse and path construction update

## Changes

`src/static_files/file_io.rs` now uses two buffer size classes for uncached
responses: 64 KiB for reads above 16 KiB and up to 64 KiB, and the existing
1 MiB buffers for full large-file chunks. Smaller reads and reads between
64 KiB and 1 MiB retain their direct allocation path. Response-cache fills
are unchanged. This avoids repeated allocation and zeroing for medium files
while avoiding oversized pooled buffers for tiny responses.

The medium pool retains at most 64 buffers, or 4 MiB. The large pool retains
at most 32 MiB, for a combined maximum of **36 MiB of idle buffers**, excluding
small bookkeeping allocations. Active responses need their own buffers.
Pool lock attempts do not wait; contention falls back to allocation or disposal.
Only bytes overwritten by a successful exact-length read are exposed, and a
buffer cannot be reused until its final response reference is released.

`src/static_files/path_security.rs` now appends ordinary relative path components
to the already-owned document root instead of allocating a joined path and
rebuilding all its ancestors. Parent segments, drive prefixes, backslashes and
other unusual paths retain the full normalization and containment checks.
The existing file opening and symlink policy are unchanged.

No new dependency or unsafe Rust was introduced. These are further allocation
optimizations, not a sendfile implementation or a guarantee of a speedup.

## Verification

`cargo test --locked --all-features --bin flashlin --target-dir target/v2-check`:
**70 passed, 0 failed**. Coverage includes buffer-size boundaries, exact response
length after reuse, outstanding response references, truncation, ranges, path
containment and drive prefixes, as well as the existing HTTP/1, HTTP/2, TLS,
HTTP/3, proxy and reload integration checks. Existing compiler warnings remain.

The release build passed. No source changes followed the measurements below.

## Windows comparison

Three alternating repetitions per build, each with three seconds of warm-up
and ten seconds of measured traffic. Both used the same load-generator binary,
two server workers, concurrency 32, HTTP/1 keep-alive, full-body status/length
checks, identity encoding, and disabled compression and response caching.
The server and client shared the workstation; CPU placement was not isolated.

| File | Before median req/s | After median req/s | Change | Before/after p99 ms |
| --- | ---: | ---: | ---: | ---: |
| small.txt | 7,034.4 | 7,230.7 | +2.8% | 6.85 / 6.08 |
| medium.txt | 6,541.8 | 6,844.3 | +4.6% | 7.41 / 6.72 |
| large.bin | 466.6 | 487.7 | +4.5% | 111.76 / 114.00 |

All errors were zero. p99 values are medians of per-run percentiles; large-file
p99 did not improve. The first baseline repetition was substantially slower
than subsequent ones. All samples are retained:

| File | Before req/s range | After req/s range | Peak sampled memory before/after, MiB |
| --- | ---: | ---: | ---: |
| small.txt | 2,873.2–7,074.3 | 7,079.2–7,610.4 | 13.5 / 13.1 |
| medium.txt | 3,155.5–6,674.4 | 6,645.3–6,977.9 | 15.8 / 15.1 |
| large.bin | 396.8–467.9 | 454.6–497.3 | 47.4 / 49.0 |

The gains are modest and some ranges overlap. These measurements do not
establish statistical confidence or predict Linux gains. Memory was sampled
every 250 ms and includes warm-up; it is not an exact allocation peak.

[Full comparison and raw reports](results/comparison-20260915-225545.json).

```powershell
./benchmarks/compare-native.ps1 -BeforeDirectory target/perf-round3-before/release -AfterDirectory target/perf-check/release -Seconds 10 -Repeats 3 -Concurrency 32
```

- Before executable SHA-256: `6E390EC1CD71CC281D026BB670D005BCF24EA13C5B32C94E751A0D9692488F12`.
- After executable SHA-256: `F61395AC35144EAB44D7F00DB40F1C1B84BFA7C8C743A12F47077EAA1D0D7F17`.
- Shared load-generator SHA-256: `27AB5A9B3C210BCA89D59C9C891767C536687C011037FEB9519EB2E2F20CD3B8`.
- The benchmark servers and load generators exited after the comparison.

## Linux context and reproduction

The user reported the preceding buffer-reuse version at 16,615.9 / 10,153.3 /
574.4 req/s for small, medium and large files, with zero errors. nginx in that
same run reached 31,077.7 / 23,259.2 / 1,462.6 req/s. Those results precede the
changes in this document; the remote raw JSON has not been inspected here.

Keep the previous binary, apply the source update to the existing checkout,
then build and repeat the same benchmark settings on Linux:

```bash
cargo test --locked --all-features --bin flashlin
cargo build --release --locked --features acme,http3 --bin flashlin --example benchmark
```

Use identical configuration, files, worker counts and load generator, alternating
the old/new build order. This workstation runs Windows; Linux gains and the
requested 5× advantage over nginx/Apache remain unverified.
