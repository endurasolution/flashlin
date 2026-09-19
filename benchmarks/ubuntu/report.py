"""Summarize raw samples without hiding errors or losing per-workload results."""
import collections
import json
import pathlib
import statistics
import sys


def summarize(output):
    output = pathlib.Path(output)
    raw = [json.loads(line) for line in (output / "samples.jsonl").read_text().splitlines() if line]
    groups = collections.defaultdict(list)
    for sample in raw:
        groups[(sample["profile"], sample["size"], sample["concurrency"], sample["server"])].append(sample)
    summary = []
    for (profile, size, concurrency, server), samples in groups.items():
        rates = [s["result"]["successful_requests_per_second"] for s in samples]
        summary.append({
            "profile": profile, "size": size, "concurrency": concurrency, "server": server,
            "samples": len(samples), "median_rps": statistics.median(rates),
            "min_rps": min(rates), "max_rps": max(rates),
            "median_p50_ms": statistics.median(s["result"]["p50_ms"] for s in samples),
            "median_p95_ms": statistics.median(s["result"]["p95_ms"] for s in samples),
            "median_p99_ms": statistics.median(s["result"]["p99_ms"] for s in samples),
            "errors": sum(s["result"]["errors"] for s in samples),
            "median_first_response_ms": statistics.median(s["startup"]["first_response_ms"] for s in samples),
            "median_startup_to_response_ms": statistics.median(s["startup"]["startup_to_response_ms"] for s in samples),
            "max_server_rss_mib": max(s["resource_usage"]["server_peak_sampled_rss_bytes"] for s in samples) / 1048576,
            "median_server_cpu_cores": statistics.median(s["resource_usage"]["server_cpu_cores"] for s in samples),
            "median_client_cpu_cores": statistics.median(s["resource_usage"]["client_cpu_cores"] for s in samples),
        })
    size_order = {"small": 0, "medium": 1, "large": 2}
    summary.sort(key=lambda s: (s["profile"], size_order[s["size"]], s["concurrency"], -s["median_rps"]))
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    lines = [
        "# Ubuntu HTTP/1.1 static-file comparison", "",
        "Median of repeated samples; the range shows run-to-run variation. All successful responses have the expected full body length. Each sample also checks its first complete response against the fixture SHA-256.", "",
        "The mandatory `uncached` profile disables Flashlin's application cache and nginx's open-file cache; the Linux filesystem page cache is warm for every measured capacity run. This is not a cold-disk benchmark. Optional `flashlin-cache-enabled` samples enable the Flashlin application cache with strict per-request revalidation. Optional `flashlin-revalidate` samples enable the updated Flashlin application's cache with a 1-second revalidation interval; nginx, Apache, and the preserved Flashlin remain configured as in the uncached profile. These are separate configuration comparisons, not like-for-like uncached results. The revalidation interval allows stale file contents for up to one second.", "",
        "One server runs at a time with the same CPU affinity. Load generation uses separate CPU cores. The container's memory limit includes both the active server and client. CPU/RSS observations cover the 3-second warm-up and measurement together and are sampled at 100 ms intervals. RSS totals may double-count shared pages in multiprocess servers.", "",
        "The client uses persistent HTTP/1.1 connections, identity encoding, full-body reads, and closed-loop concurrency. Latency has no coordinated-omission correction; these results do not establish open-loop capacity or internet/browser loading performance. First-response figures use a newly started process with warm filesystem pages, include a new connection, and exclude load-generator warm-up. Process-start-to-response timings include readiness polling and scheduler delay.", "",
        "| Profile | Size | Concurrency | Server | Median req/s | Min–max req/s | p50 ms | p95 ms | p99 ms | Errors |",
        "|---|---|---:|---|---:|---:|---:|---:|---:|---:|",
    ]
    if (output / "failure.json").exists():
        lines[2:2] = ["**INCOMPLETE RUN:** a sample failed. See `failure.json` and the logs. The table contains only completed clean samples and must not be used as a complete ranking.", ""]
    for s in summary:
        lines.append(f"| {s['profile']} | {s['size']} | {s['concurrency']} | {s['server']} | {s['median_rps']:,.0f} | {s['min_rps']:,.0f}–{s['max_rps']:,.0f} | {s['median_p50_ms']:.3f} | {s['median_p95_ms']:.3f} | {s['median_p99_ms']:.3f} | {s['errors']} |")
    lines += ["", "## Flashlin versus the competitors", "", "A positive percentage means Flashlin's median request rate is higher. Differences within the sample ranges may be measurement noise.", "", "| Profile | Size | Concurrency | Versus nginx | Versus Apache | Versus preserved Flashlin |", "|---|---|---:|---:|---:|---:|"]
    for current in [s for s in summary if s["server"] == "flashlin"]:
        def change(name):
            matches = [s for s in summary if s["profile"] == current["profile"] and s["size"] == current["size"] and s["concurrency"] == current["concurrency"] and s["server"] == name]
            return f"{(current['median_rps'] / matches[0]['median_rps'] - 1) * 100:+.1f}%" if matches and matches[0]["median_rps"] else "—"
        lines.append(f"| {current['profile']} | {current['size']} | {current['concurrency']} | {change('nginx')} | {change('apache')} | {change('flashlin-before')} |")
    lines += ["", "## Process startup and observed resources", "", "| Profile | Size | Concurrency | Server | First response ms | Start to response ms | Peak sampled RSS MiB | Server CPU cores | Client CPU cores |", "|---|---|---:|---|---:|---:|---:|---:|---:|"]
    for s in summary:
        lines.append(f"| {s['profile']} | {s['size']} | {s['concurrency']} | {s['server']} | {s['median_first_response_ms']:.2f} | {s['median_startup_to_response_ms']:.2f} | {s['max_server_rss_mib']:.1f} | {s['median_server_cpu_cores']:.2f} | {s['median_client_cpu_cores']:.2f} |")
    lines += ["", "Raw samples: `samples.jsonl`; aggregate values: `summary.json`; complete environment/configuration/build/binary identities: `metadata/`. Startup response headers and each measured client's original JSON are retained with each raw sample."]
    (output / "REPORT.md").write_text("\n".join(lines) + "\n")
    return summary


if __name__ == "__main__":
    summarize(sys.argv[1])
