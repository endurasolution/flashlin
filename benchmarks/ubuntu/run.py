"""Sequential, CPU-isolated Linux comparison using the repository's HTTP client."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.request

from report import summarize

HERE = Path(__file__).resolve().parent
PORT = 18080
BASE_URL = f"http://127.0.0.1:{PORT}"


def cpus(value):
    result = set()
    for part in value.split(","):
        low, _, high = part.partition("-")
        result.update(range(int(low), int(high or low) + 1))
    if not result:
        raise ValueError("CPU list cannot be empty")
    return result


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def command_output(command):
    result = subprocess.run(command, capture_output=True, text=True, timeout=15)
    return {"command": command, "returncode": result.returncode,
            "stdout": result.stdout, "stderr": result.stderr}


def listening():
    with socket.socket() as sock:
        sock.settimeout(0.1)
        return sock.connect_ex(("127.0.0.1", PORT)) == 0


def stop_server(process):
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=5)
    except ProcessLookupError:
        pass
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=5)
    for _ in range(100):
        if not listening():
            return
        time.sleep(0.02)
    raise RuntimeError("The benchmark port stayed occupied after stopping its server")


def snapshot(groups):
    """Read process group CPU ticks/RSS without profiling syscalls or requests."""
    result = {group: {} for group in groups}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
            group = int(fields[2])
            if group in result:
                result[group][int(entry.name)] = {
                    "ticks": int(fields[11]) + int(fields[12]),
                    "rss": max(0, int(fields[21])) * os.sysconf("SC_PAGE_SIZE"),
                }
        except (OSError, ValueError, IndexError):
            continue
    return result


def first_response(process, started, fixture, expected):
    deadline = started + 10
    while not listening():
        if process.poll() is not None:
            raise RuntimeError(f"Server exited before readiness: {process.returncode}")
        if time.perf_counter() > deadline:
            raise RuntimeError("Server failed to listen within 10 seconds")
        time.sleep(0.02)
    request = urllib.request.Request(BASE_URL + "/" + fixture,
                                     headers={"Accept-Encoding": "identity"})
    first_started = time.perf_counter()
    with urllib.request.urlopen(request, timeout=10) as response:
        content = response.read()
        headers = dict(response.headers.items())
        status = response.status
    ended = time.perf_counter()
    if status != 200 or len(content) != expected["bytes"] or hashlib.sha256(content).hexdigest() != expected["sha256"]:
        raise RuntimeError("Preflight response status, length, or SHA-256 differs from fixture")
    if headers.get("Content-Encoding", "identity") != "identity":
        raise RuntimeError("Unexpected response compression")
    return {"first_response_ms": (ended - first_started) * 1000,
            "startup_to_response_ms": (ended - started) * 1000,
            "status": status, "headers": headers,
            "sha256": hashlib.sha256(content).hexdigest()}


def run_client(args, command, process, stdout_path, stderr_path):
    environment = os.environ.copy()
    environment["TOKIO_WORKER_THREADS"] = str(len(cpus(args.client_cpus)))
    initial = snapshot([process.pid])[process.pid]
    observed = {"server": {}, "client": {}}
    peak_rss = {"server": 0, "client": 0}
    began = time.perf_counter()
    with stdout_path.open("w") as stdout, stderr_path.open("w") as stderr:
        client = subprocess.Popen(command, stdout=stdout, stderr=stderr,
                                  env=environment, start_new_session=True)
        try:
            while True:
                current = snapshot([process.pid, client.pid])
                for name, group in (("server", process.pid), ("client", client.pid)):
                    values = current[group]
                    peak_rss[name] = max(peak_rss[name], sum(p["rss"] for p in values.values()))
                    for pid, data in values.items():
                        observed[name][pid] = max(observed[name].get(pid, 0), data["ticks"])
                if client.poll() is not None:
                    break
                if process.poll() is not None:
                    raise RuntimeError("Server exited during load generation")
                if time.perf_counter() - began > args.seconds + 35:
                    raise RuntimeError("Load generator exceeded its timeout")
                time.sleep(0.1)
        finally:
            if client.poll() is None:
                os.killpg(client.pid, signal.SIGKILL)
                client.wait()
    elapsed = time.perf_counter() - began
    ticks_per_second = os.sysconf("SC_CLK_TCK")
    server_ticks = sum(max(0, ticks - initial.get(pid, {}).get("ticks", 0))
                       for pid, ticks in observed["server"].items())
    usage = {"sampling_interval_seconds": 0.1, "including_warmup_seconds": elapsed,
             "server_cpu_cores": server_ticks / ticks_per_second / elapsed,
             "client_cpu_cores": sum(observed["client"].values()) / ticks_per_second / elapsed,
             "server_peak_sampled_rss_bytes": peak_rss["server"],
             "client_peak_sampled_rss_bytes": peak_rss["client"]}
    if client.returncode:
        raise RuntimeError(f"Load generator returned {client.returncode}; see {stderr_path}")
    result = json.loads(stdout_path.read_text())
    if result["errors"] or result["successes"] <= 0:
        raise RuntimeError("Load generator reported failed responses")
    return result, usage


def metadata(args, output, manifest):
    directory = output / "metadata"
    directory.mkdir()
    values = {"utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "arguments": vars(args), "python": platform.python_version(),
              "fixtures": manifest, "server_cpu_affinity": sorted(cpus(args.server_cpus)),
              "client_cpu_affinity": sorted(cpus(args.client_cpus)),
              "allowed_cpu_affinity": sorted(os.sched_getaffinity(0)),
              "memory_budget": "Shared container cgroup limit; one active server plus client",
              "cgroup": {}, "binaries": {}}
    for filename in ("/etc/os-release", "/proc/version", "/proc/meminfo", "/proc/loadavg",
                     "/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/memory.swap.max",
                     "/sys/fs/cgroup/cpu.max", "/sys/fs/cgroup/cpuset.cpus.effective"):
        path = Path(filename)
        if path.exists():
            values["cgroup"][filename] = path.read_text()
    for command in (["nginx", "-V"], ["apache2", "-V"], ["/opt/bin/flashlin", "--version"],
                    ["uname", "-a"], ["lscpu"],
                    ["dpkg-query", "-W", "nginx", "apache2", "libc6", "linux-libc-dev"]):
        values.setdefault("commands", []).append(command_output(command))
    binaries = {"flashlin": "/opt/bin/flashlin", "benchmark": args.benchmark,
                "nginx": shutil.which("nginx"), "apache": shutil.which("apache2")}
    if args.before:
        binaries["flashlin-before"] = args.before
    for name, filename in binaries.items():
        values["binaries"][name] = {"path": filename, "sha256": sha256(filename)}
    for path in Path("/opt/bin").glob("*.txt"):
        shutil.copy2(path, directory / path.name)
    for path in HERE.iterdir():
        if path.suffix in (".conf", ".toml", ".py") or path.name == "Dockerfile":
            shutil.copy2(path, directory / path.name)
    (directory / "environment.json").write_text(json.dumps(values, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", default="/results/run")
    parser.add_argument("--seconds", type=int, default=5)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--concurrency", default="1,32,128")
    parser.add_argument("--sizes", default="small,medium,large")
    parser.add_argument("--servers", default="nginx,apache,flashlin")
    parser.add_argument("--server-cpus", default="0,1")
    parser.add_argument("--client-cpus", default="2,3")
    parser.add_argument("--benchmark", default="/opt/bin/benchmark")
    parser.add_argument("--before", help="Preserved Flashlin binary; adds flashlin-before to the rotation")
    parser.add_argument("--cached", action="store_true", help="Also run separately labeled cache-enabled Flashlin comparisons")
    parser.add_argument("--revalidate", action="store_true", help="Also run separately labeled Flashlin cache with 1-second file revalidation")
    args = parser.parse_args()
    if not 1 <= args.seconds <= 3600 or not 1 <= args.rounds <= 100:
        parser.error("seconds must be 1..3600 and rounds 1..100")
    server_cpus, client_cpus = cpus(args.server_cpus), cpus(args.client_cpus)
    if len(server_cpus) != 2 or server_cpus & client_cpus:
        parser.error("Use exactly two server CPUs and disjoint client CPUs")
    if not (server_cpus | client_cpus) <= os.sched_getaffinity(0):
        parser.error("Selected CPUs are outside the container CPU affinity")
    for cpu in server_cpus:
        topology = Path(f"/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list")
        if topology.exists() and cpus(topology.read_text().strip()) & client_cpus:
            parser.error("Server/client CPUs share an SMT core; choose separate physical cores")
    concurrency = [int(c) for c in args.concurrency.split(",")]
    if any(c < 1 or c > 128 for c in concurrency):
        parser.error("concurrency must be 1..128 to stay within the configured Apache worker limit")
    sizes = args.sizes.split(",")
    if not sizes or set(sizes) - {"small", "medium", "large"}:
        parser.error("sizes must be small,medium,large or a subset")
    servers = args.servers.split(",")
    if not servers or set(servers) - {"nginx", "apache", "flashlin"}:
        parser.error("servers must be nginx,apache,flashlin or a subset")
    if args.before:
        if not os.access(args.before, os.X_OK):
            parser.error("--before must point to an executable preserved Linux Flashlin binary")
        servers.append("flashlin-before")
    if listening():
        parser.error("Port 18080 is already in use; refusing mixed-server measurements")
    output = Path(args.output)
    output.mkdir(parents=True, exist_ok=False)
    (output / "logs").mkdir()
    manifest = json.loads(Path("/srv/bench/manifest.json").read_text())
    metadata(args, output, manifest)
    commands = {
        "nginx": ["nginx", "-c", str(HERE / "nginx.conf"), "-g", "daemon off;"],
        "apache": ["apache2", "-f", str(HERE / "apache.conf"), "-DFOREGROUND"],
        "flashlin": ["/opt/bin/flashlin", str(HERE / "flashlin.toml")],
    }
    if args.before:
        commands["flashlin-before"] = [args.before, str(HERE / "flashlin.toml")]
    cached_config = output / "metadata/flashlin-cached.toml"
    cached_config.write_text((HERE / "flashlin.toml").read_text().replace("enabled = false", "enabled = true", 1))
    revalidate_config = output / "metadata/flashlin-revalidate.toml"
    revalidate_config.write_text(cached_config.read_text().replace('[cache]', '[cache]\nrevalidate_after = "1s"', 1))
    profiles = ["uncached"]
    if args.cached:
        profiles.append("flashlin-cache-enabled")
    if args.revalidate:
        profiles.append("flashlin-revalidate")
    count = 0
    try:
        for profile in profiles:
            for round_index in range(args.rounds):
                # Rotate BOTH server order and workload order to reduce time/order bias.
                workloads = [(size, c) for size in sizes for c in concurrency]
                shift = round_index % len(workloads)
                workloads = workloads[shift:] + workloads[:shift]
                for work_index, (size, concurrent) in enumerate(workloads):
                    shift = (round_index + work_index) % len(servers)
                    order = servers[shift:] + servers[:shift]
                    for server in order:
                        count += 1
                        fixture = size + ".bin"
                        stem = f"{count:04d}-{profile}-r{round_index + 1}-{size}-c{concurrent}-{server}"
                        server_command = list(commands[server])
                        if profile == "flashlin-cache-enabled" and server.startswith("flashlin"):
                            server_command[-1] = str(cached_config)
                        if profile == "flashlin-revalidate" and server == "flashlin":
                            server_command[-1] = str(revalidate_config)
                        command = ["taskset", "-c", args.server_cpus] + server_command
                        print(f"[{count}] {profile} round={round_index + 1} {size} c={concurrent} {server}", flush=True)
                        with (output / "logs" / f"{stem}-server.log").open("w") as log:
                            start = time.perf_counter()
                            process = subprocess.Popen(command, stdout=log, stderr=log,
                                                       start_new_session=True)
                            try:
                                startup = first_response(process, start, fixture, manifest[fixture])
                                client_command = ["taskset", "-c", args.client_cpus, args.benchmark,
                                                  BASE_URL + "/" + fixture, str(args.seconds),
                                                  str(concurrent), str(manifest[fixture]["bytes"])]
                                result, usage = run_client(args, client_command, process,
                                                          output / "logs" / f"{stem}.json",
                                                          output / "logs" / f"{stem}-client-error.log")
                            finally:
                                stop_server(process)
                        sample = {"sequence": count, "profile": profile, "round": round_index + 1,
                                  "server": server, "size": size, "concurrency": concurrent,
                                  "command": command, "client_command": client_command,
                                  "startup": startup, "resource_usage": usage, "result": result}
                        with (output / "samples.jsonl").open("a") as file:
                            file.write(json.dumps(sample) + "\n")
                        print(f"  {result['successful_requests_per_second']:,.0f} req/s; p99={result['p99_ms']:.3f} ms; errors={result['errors']}", flush=True)
    except BaseException as error:
        (output / "failure.json").write_text(json.dumps({"sample": count, "error": str(error)}, indent=2) + "\n")
        raise
    finally:
        if (output / "samples.jsonl").exists():
            summarize(output)
    print(f"Complete: {count} clean samples; report: {output / 'REPORT.md'}", flush=True)


if __name__ == "__main__":
    main()
