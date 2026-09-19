# Windows local baseline — 2026-09-14

This is a measured Flashlin v2 development baseline, **not** a production
benchmark or Nginx/Apache comparison. All nine measured runs completed with zero
request failures (HTTP 200 and expected response length).

## Measured build results

These results identify the measured binary by SHA-256 below. Subsequent protocol
changes, including QUERY support, have not been re-benchmarked in this report.

| File | Median requests/s | Requests/s range (3 runs) | Median run p99 (ms) | Peak sampled working set (MiB) | Median server CPU (one core = 100%) |
|---|---:|---:|---:|---:|---:|
| 1 KiB | 629.3 | 582.0–634.9 | 84.7 | 12.8 | 274.2% |
| 64 KiB | 527.8 | 490.5–588.7 | 105.3 | 14.7 | 278.8% |
| 4 MiB | 131.4 | 107.2–159.0 | 366.7 | 16.5 | 291.9% |

“Median run p99” is the median of three independently reported p99 values, not a
pooled percentile. Histogram values are upper bounds with up to 2% relative error.
CPU and 250 ms working-set samples include warm-up and teardown sampling overhead;
request latency and throughput exclude warm-up. Sampled memory can miss short peaks.
Two Tokio workers are configured, but blocking filesystem workers can consume
additional CPU; the native process is not pinned or limited to two cores.

## Conditions and evidence

- Microsoft Windows NT 10.0.26200.0; 12th Gen Intel(R) Core(TM) i5-1240P; 16 logical processors;
  23.7 GiB physical RAM.
- Server and generator on the same Windows workstation, using loopback.
- Release build, all features compiled, but TLS, compression and application RAM
  caching disabled for this profile. TCP_NODELAY and vectored-write forwarding enabled.
- Three repetitions of each fixture, 32 concurrent requests, 3-second warm-up and
  10-second measurement per run. OS page cache is warmed; machine isolation and
  cold-cache state were not controlled. Fixture creation repeats before each suite.
- HTTP/1.1 keep-alive, identity encoding, closed-loop request generation and full
  response body timing. No coordinated-omission correction.
- Command: `./benchmarks/run-native.ps1 -Seconds 10 -Repeats 3 -Concurrency 32`.
- [Native configuration](native.toml), [raw measurement report](baselines/windows-20260914.json),
  [reproduction instructions](README.md).
- Binary SHA-256: `03CE9D45C454913AE3866678EEA1CD05CB68D1EC8BF5014227F0BDB60345617E`.
- Configuration SHA-256: `DC96277033EE37CC244CE4191DD10176B98BC6E0C8D598B8500A5F87A3B1A40A`.

An earlier exploratory run, before the TCP_NODELAY/vectored-write changes, is
retained locally at `results/native-20260914-142248/report.json`. This was sequential,
not an interleaved controlled A/B experiment. Run-to-run variation and workstation
effects mean the difference cannot be attributed solely to those code changes.

## Release interpretation

Throughput and tail latency still need investigation on representative deployment
hardware; these results do not justify a “faster than Nginx/Apache” claim. The
Docker comparison configuration validates, but Docker Engine was not running,
so neither those container builds nor the cross-server workload ran here.

Before release: use isolated server/load-generator machines, record image digests,
repeat at least five times in rotating order, test TLS/compression and realistic
proxy/application traffic, measure sustained load and cold/warm behavior, and
check errors, CPU, memory and latency under fixed-arrival-rate overload.

Feature verification on this Windows workspace: **50 tests passed with all
features, 48 with manual TLS, 47 with HTTP-only**. Fresh public ACME issuance and
renewal were not tested; the offline test verifies cached-certificate loading and
a trusted TLS/HTTP2-ALPN handshake. Linux CI and production acceptance remain pending.
