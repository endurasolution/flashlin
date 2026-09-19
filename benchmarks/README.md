# Reproducible baseline (not a production certification)

See the [recorded Windows baseline](LOCAL_BASELINE.md) for measured results and limitations.
See the [static-file performance update](PERFORMANCE_UPDATE.md) for the latest before/after measurements.
See the [Linux follow-up](LINUX_FOLLOWUP.md) for the latest user-supplied comparison and the Linux profiling helper.
See the [buffer reuse update](BUFFER_REUSE_UPDATE.md) for the subsequent source changes, validation and reproduction steps.
See the [medium-buffer and path update](SIZE_CLASS_UPDATE.md) for the next allocation optimizations.

Run from the repository root. Docker Desktop/Engine must already be running.

```powershell
./benchmarks/prepare.ps1
docker compose -f benchmarks/compose.yaml build flashlin
docker compose -f benchmarks/compose.yaml up -d flashlin nginx apache
docker compose -f benchmarks/compose.yaml run --rm load benchmark http://flashlin:8080/small.txt 30 32 1024
docker compose -f benchmarks/compose.yaml run --rm load benchmark http://nginx:8080/small.txt 30 32 1024
docker compose -f benchmarks/compose.yaml run --rm load benchmark http://apache:8080/small.txt 30 32 1024
```

Repeat each sample at least five times, rotating server order. Also run medium.txt
(65536 bytes), large.bin (4194304 bytes), and concurrency 1, 32, 128. Stop other
traffic and keep the load generator from competing with server CPU cores. Record
container image digests and exact server versions: image tags are mutable. Keep
raw JSON output and report medians plus spread, not the best run.

The profiles use two server CPUs, 512 MiB memory, plain HTTP/1 keep-alive,
identity encoding, no access logging and no application response cache. OS page
caches are warmed. Nginx/Apache may use sendfile; Flashlin uses its streaming path.
These are comparable static-service settings, not identical implementations or
fully optimized profiles. Apache's sample caps active requests at 128; do not
interpret saturation above that setting as an inherent server limitation.

The bundled load generator warms up for 3 seconds and measures the full body.
It verifies status and expected length, reports failures, throughput and
p50/p95/p99 latency (histogram upper bounds within 2%). Latencies include errors.
It uses a closed-loop load model, without coordinated-omission correction, so
these percentiles do not predict overload behavior under a fixed arrival rate.
The measured wall time includes draining requests started before the deadline.

For production acceptance, additionally test identical TLS certificates/protocols,
compression, cold/warm cache, application/proxy traffic, slow clients, sustained
load, error recovery, peak RSS and CPU. Capture `docker stats` during each run,
OS/CPU/RAM, network topology, configuration hashes and load-generator utilization.
Run a separate fixed-arrival-rate tool on another machine for overload testing.
Neither this harness nor a loopback result establishes a Nginx/Apache speedup.

Native Flashlin baseline: build with `cargo build --release --features acme --bin
flashlin --example benchmark`; use a profile pointing at target/benchmark-fixture,
then run `target/release/examples/benchmark http://127.0.0.1:8080/small.txt 30 32 1024`.
The runner intentionally supports HTTP only; it does not silently skip TLS checks.

On Windows, `./benchmarks/run-native.ps1` runs three repetitions for each fixture
at concurrency 32 and writes raw JSON plus a hardware/CPU/memory report under
`benchmarks/results/`. It uses `target/v2-check/release`, starts a loopback-only
server on port 18082 and stops only the processes it started. Adjust `-Seconds`,
`-Repeats` and `-Concurrency` as needed. CPU/RSS samples include warm-up; HTTP
latency and throughput exclude warm-up. Avoid builds/other load during measurement.

Cleanup only this comparison stack with `docker compose -f benchmarks/compose.yaml down`.

## Compare two native Flashlin builds

Keep the old release binary before building changes. Each directory needs
`flashlin.exe` and `examples/benchmark.exe`; use the same benchmark executable
in both directories. The comparison alternates build order between repetitions
and preserves all raw results, executable/config hashes, throughput spread,
latency and sampled memory usage.

```powershell
./benchmarks/compare-native.ps1 -BeforeDirectory target/perf-before/release -AfterDirectory target/perf-check/release -Seconds 10 -Repeats 3 -Concurrency 32
```

`run-native.ps1` also accepts `-BinaryDirectory` to select a release directory.
Both runners accept `-ConfigurationFile` and `-File` to run a specific fixture.
For a separate cache-enabled large-file comparison, add
`-ConfigurationFile benchmarks/native-cached.toml -File large.bin`.
Hardware inventory is recorded when Windows permits it; restricted inventory
access is recorded in the report and does not prevent measuring HTTP traffic.

The static path batches filesystem preparation and the first read in one
blocking job. Uncached bodies read on demand in chunks of at most 1 MiB; this
trades a larger per-transfer buffer for fewer worker handoffs. Response caching
respects `cache.max_file_size` and `cache.max_size`, including files larger than
1 MiB. The comparison configurations still disable response caching. Results
with caching enabled must be labeled separately.

Full 1 MiB stream buffers are now reusable after the transport releases its
last reference. A separate pool now reuses 64 KiB buffers for medium reads above
16 KiB and up to 64 KiB. The pools retain at most 36 MiB of idle buffers across
requests; active transfers still require their own buffers. Tiny reads and
response-cache fills use their existing allocation path.
