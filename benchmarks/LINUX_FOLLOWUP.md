# Linux follow-up — 2026-09-15

The user supplied the following medians from three repetitions. The raw results
are on the Linux host at
`/root/flashlin/benchmarks/results/native-20260915-142150/raw.jsonl`;
they have not been inspected from this Windows workspace. Exact build hashes,
configuration and the comparison script are needed to reproduce the run.

| File | Apache req/s | Flashlin req/s | nginx req/s | nginx / Flashlin | Flashlin p99 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| small.txt | 16,634.7 | 15,718.0 | 30,807.3 | 1.96× | 4.34 |
| medium.txt | 12,886.1 | 9,906.4 | 22,578.7 | 2.28× | 6.72 |
| large.bin | 1,212.9 | 373.4 | 1,345.1 | 3.60× | 144.58 |

All reported errors were zero. Compared with the earlier Linux table, Flashlin's
throughput relative to nginx improved about 16% for small files and 17% for
medium files, and declined about 5% for large files. All three servers had lower
large-file throughput in the later run. Separate runs do not isolate the effect
of a code change; use alternating old/new builds under the same conditions.

A 5× advantage over this nginx run would require about 154,037, 112,894 and
6,726 req/s respectively: approximately 9.8×, 11.4× and 18.0× Flashlin's current
rates. That target has not been achieved.

## Measure the Linux transfer path

The [original Linux comparison](NATIVE_COMPARISON.md) enables sendfile in nginx
and Apache. Flashlin currently reads uncached files into application buffers and
passes them through Hyper. Linux's [sendfile system call](https://man7.org/linux/man-pages/man2/sendfile.2.html)
can avoid copying file bytes through user space. Copying and blocking-worker
handoffs are candidates for the large-file gap, not established causes. Adding
sendfile requires integration with the connection writer and HTTP framing;
changing the read chunk alone does not provide it.

The new helper records an untraced full-body sample, process/host snapshots,
configuration and executable hashes, and optional aggregate `strace` and `perf`
counters. It starts its own Flashlin process, refuses an occupied port, verifies
listener ownership and stops its own processes. It does not modify services or
system settings. Use an isolated benchmark configuration without production
listeners or external integrations.

Run from the repository root on Linux. Build before measuring:

```bash
cargo build --release --locked --features acme,http3 --bin flashlin --example benchmark
bash benchmarks/profile-linux.sh benchmarks/native.toml http://127.0.0.1:18082/large.bin 4194304 10
```

This example requires `target/benchmark-fixture/large.bin` to exist and contain
exactly 4,194,304 bytes. Substitute the configuration, URL and byte length used
in the original comparison if they differ. The supplied native profile disables
compression and response caching. Keep cache-enabled measurements separate.
`ss` (iproute2) is required; `strace` and `perf` are optional. Their availability
and attach failures are recorded; the helper does not change profiling permissions.
Set `FLASHLIN_BIN` and `BENCHMARK_BIN` to profile saved release executables.

Results go into `benchmarks/results/linux-profile-*`. Inspect `baseline.json`,
`environment.txt`, `strace-summary.txt`, `perf-stat.txt`, process snapshots and
any error logs. Tracing changes throughput; traced runs are diagnostic samples,
not capacity comparisons. Profiler counters include the runner's three-second
warm-up. Run on an otherwise idle host.

The helper passes Bash syntax validation on the Windows workstation. Linux
execution, attachment permissions and counter collection remain unverified.
No additional server throughput improvement is claimed for this diagnostic change.
