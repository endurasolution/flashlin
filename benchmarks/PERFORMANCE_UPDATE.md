# Static-file performance update — 2026-09-15

The measured Windows comparison improved uncached median throughput by 6–15% and cache-enabled 4 MiB throughput by 2.55× against the original Flashlin build. This does **not** establish a 5× advantage over nginx or Apache.

The subsequent [user-supplied Linux comparison](LINUX_FOLLOWUP.md) still shows nginx ahead by 1.96×, 2.28× and 3.60× for small, medium and large files. Windows gains do not establish Linux gains.

## Changes

- Batch static-file resolution, open, metadata, header preparation and the first body read in one blocking job. Networking workers remain available while disk operations run.
- Read uncached files on demand in chunks of at most 1 MiB. Transfer the read buffer directly into the response instead of copying through Tokio file buffers.
- Honor the configured cache file-size limit instead of imposing an additional 1 MiB cap. Both the entry limit and total cache budget still apply; no-store responses bypass storage.
- Serialize expired-entry removal with cache mutations and recheck expiry before removing a refreshed entry. Fresh cache hits do not acquire the mutation mutex.
- Preserve the existing path-resolution policy, HTTP ranges, conditional requests, HEAD handling and negotiated precompressed files. Exact-length reads report truncation as an error.

## Uncached results

Windows loopback, two networking workers, concurrency 32, HTTP/1 keep-alive, identity encoding, compression and application response cache disabled. Three alternating repetitions per build/file, each with 3 seconds of warm-up and 10 seconds of measured load. The same benchmark executable checks status and complete body length for both builds.

| File | Before req/s | After req/s | Ratio | Before p99 ms | After p99 ms | Errors |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| small.txt | 7,241.9 | 7,648.2 | 1.06× | 7.13 | 5.73 | 0 |
| medium.txt | 5,972.4 | 6,795.3 | 1.14× | 8.86 | 6.85 | 0 |
| large.bin | 407.8 | 468.1 | 1.15× | 114.00 | 114.00 | 0 |

Ratios divide the two throughput medians; p99 columns are medians of per-run p99 values. The small-file throughput ranges overlap, so its 6% median increase should be treated cautiously.

| File | Before req/s range | After req/s range | Peak sampled memory before/after, MiB |
| --- | ---: | ---: | ---: |
| small.txt | 7,217.9–8,633.0 | 7,502.9–7,719.9 | 13.0 / 13.0 |
| medium.txt | 5,952.1–6,223.2 | 6,582.3–7,004.6 | 15.2 / 14.3 |
| large.bin | 373.4–415.1 | 429.4–470.9 | 29.1 / 45.2 |

The larger stream chunk increased sampled large-transfer memory from 29.1 to 45.2 MiB in this run. Memory figures are process working-set samples every 250 ms, include warm-up, and are not exact peak allocation measurements.

## Cache-enabled large files — separate comparison

Both builds used the same profile: cache enabled, 128 MiB total cache, 10 MiB maximum entry, 300-second TTL. The original build bypassed its cache for the 4 MiB file because of the hard-coded 1 MiB cap; the new build can retain it. All other load parameters match the uncached comparison.

- Median throughput: **400.6 → 1,020.3 req/s (2.55×)**.
- Median p99: **118.60 → 62.94 ms**.
- Throughput ranges: 349.9–526.5 before; 993.8–1034.8 after.
- Peak sampled working set: 27.9 → 16.6 MiB. Zero errors.

## Verification and limits

- `cargo test --all-features --bin flashlin --target-dir target/v2-check`: 65 passed. Coverage includes HTTP/1, HTTP/2, HTTPS, HTTP/3, ranges, conditional requests, HEAD, compression, reloads, streaming and proxy behavior.
- Added byte-for-byte multi-chunk and offset checks, lazy-read/growth checks, truncation failures, configured cache limits/no-store, file replacement/deletion and concurrent cache accounting checks.
- Release build and the cache-enabled configuration check passed. Existing unused-code/import warnings remain.
- Windows 10.0.26200, 16 reported logical processors. Hardware inventory access was restricted; CPU model and total RAM were not available through the benchmark runner.
- Client and server share the machine. Results use a closed-loop load generator, without coordinated-omission correction. CPU placement is not isolated and results vary between runs.
- Docker was stopped and no general-purpose Linux WSL distribution was available. Linux/nginx/Apache comparisons were not run. The user-provided nginx/Apache rates cannot be directly compared with these Windows rates.
- Earlier exploratory comparisons used versions that were changed afterward, and one suffered background-search interference. They are retained in results but excluded from these tables.

## Reproduce

Preserve the original build in `target/perf-before/release`, build the updated source with `cargo build --release --bin flashlin --example benchmark --target-dir target/perf-check`, and use an identical `examples/benchmark.exe` in both directories.

```powershell
./benchmarks/compare-native.ps1 -BeforeDirectory target/perf-before/release -AfterDirectory target/perf-check/release -Seconds 10 -Repeats 3 -Concurrency 32
./benchmarks/compare-native.ps1 -BeforeDirectory target/perf-before/release -AfterDirectory target/perf-check/release -Seconds 10 -Repeats 3 -Concurrency 32 -ConfigurationFile benchmarks/native-cached.toml -File large.bin
```

## Measurement provenance

- [Uncached comparison and all raw reports](results/comparison-20260915-182940.json).
- [Cache-enabled comparison and all raw reports](results/comparison-20260915-183115.json).
- Before SHA-256: `0FB65EB37A446FB106EB6132D27D5D35A356174D8621EFD13A39802D412D7E25`.
- Measured after SHA-256: `F5681EE294AB3E29FE99AA0C6B6E4DBB105308BA289CA479BA4E13D3E729D10F`.
- The measured updated executable is retained in `target/perf-measured/release/flashlin.exe`.
- After measurement, review restored metadata-before-open checks for precompressed sidecars to avoid opening non-regular sidecars. The benchmark profiles disable compression, so that branch was not exercised by these measurements. The latest source is re-tested; performance was not remeasured for this branch-only follow-up.
- Final production executable after the sidecar review (built with cargo build --release --bin flashlin): CF7FFB07E8AA71D62A42394A5C9B932EFA03CBD26135CC27388E5C818AB0EBF3.
