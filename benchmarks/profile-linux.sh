#!/usr/bin/env bash
# Profile one isolated Flashlin instance without changing system services.
set -euo pipefail

usage() {
    echo "Usage: bash benchmarks/profile-linux.sh CONFIG HTTP_LOOPBACK_URL EXPECTED_BYTES [SECONDS]" >&2
    echo "Example URL: http://127.0.0.1:18082/large.bin" >&2
    exit 2
}
[[ $# -ge 3 && $# -le 4 ]] || usage
[[ $(uname -s) == Linux ]] || { echo 'This helper requires Linux.' >&2; exit 2; }
config=$1
url=$2
expected=$3
seconds=${4:-10}
[[ -f $config && $expected =~ ^[0-9]+$ && $seconds =~ ^[1-9][0-9]?$ ]] || usage
[[ $url =~ ^http://(127\.0\.0\.1|localhost):([0-9]{1,5})/ ]] || usage
port=${BASH_REMATCH[2]}
(( 10#$port > 0 && 10#$port <= 65535 )) || usage
command -v ss >/dev/null || { echo 'ss (iproute2) is required to check listener ownership.' >&2; exit 2; }
server_bin=${FLASHLIN_BIN:-./target/release/flashlin}
runner=${BENCHMARK_BIN:-./target/release/examples/benchmark}
[[ -x $server_bin && -x $runner ]] || {
    echo 'Build the release server and benchmark example first.' >&2; exit 2;
}
"$server_bin" --check "$config"
[[ -z $(ss -H -ltn "sport = :$port") ]] || {
    echo "Port $port is already occupied. Choose a dedicated benchmark port." >&2; exit 2;
}
mkdir -p benchmarks/results
out=$(mktemp -d "benchmarks/results/linux-profile-$(date -u +%Y%m%d-%H%M%S)-XXXXXX")
server_pid=''
probe_pid=''
stop_child() {
    local pid=$1
    # Signal only an active background child started by this shell.
    if [[ -n $pid ]] && jobs -pr | grep -qx "$pid"; then
        kill -TERM "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
}
cleanup() { stop_child "$probe_pid"; stop_child "$server_pid"; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

{
    date -u
    uname -a
    command -v lscpu >/dev/null && lscpu
    printf '\nConfiguration/binary/load-generator hashes:\n'
    sha256sum "$config" "$server_bin" "$runner"
    printf '\nWorkload: %s; bytes=%s; seconds=%s; concurrency=32\n' "$url" "$expected" "$seconds"
    printf 'Traced runs are diagnostics, not capacity results. Warm-up is included in profiler counters.\n'
} > "$out/environment.txt"
"$server_bin" "$config" > "$out/server.log" 2> "$out/server-error.log" &
server_pid=$!
ready=false
for _ in {1..100}; do
    kill -0 "$server_pid" 2>/dev/null || { echo "Server exited; see $out/server-error.log" >&2; exit 1; }
    listeners=$(ss -H -ltnp "sport = :$port")
    if [[ -n $listeners ]]; then
        [[ $(printf '%s\n' "$listeners" | wc -l) == 1 && $listeners == *"pid=$server_pid,"* ]] || {
            echo "Listener ownership is ambiguous; refusing a mixed-server measurement." >&2; exit 1;
        }
        ready=true
        break
    fi
    sleep 0.1
done
$ready || { echo 'Server did not bind the URL port within 10 seconds.' >&2; exit 1; }

snapshot() {
    local label=$1
    {
        date -u
        for name in status stat io schedstat; do
            printf '\n/proc/PID/%s\n' "$name"
            cat "/proc/$server_pid/$name" 2>/dev/null || true
        done
        printf '\nHost load and memory:\n'
        cat /proc/loadavg
        head -n 5 /proc/meminfo
    } > "$out/$label.txt"
}
load() {
    local label=$1
    snapshot "$label-before"
    "$runner" "$url" "$seconds" 32 "$expected" > "$out/$label.json" 2> "$out/$label-error.log"
    snapshot "$label-after"
}

echo 'Running an untraced sample...'
load baseline
if command -v strace >/dev/null; then
    echo 'Collecting aggregate system-call counts (no request or file contents)...'
    strace -f -c -p "$server_pid" -o "$out/strace-summary.txt" \
        -e trace=read,pread64,write,writev,sendto,sendmsg,sendfile,openat,close,fstat,newfstatat,statx,futex,epoll_wait,epoll_pwait \
        2> "$out/strace-error.log" &
    probe_pid=$!
    sleep 0.2
    if kill -0 "$probe_pid" 2>/dev/null; then
        load strace
        stop_child "$probe_pid"
    else
        wait "$probe_pid" 2>/dev/null || true
        echo 'strace attachment failed; see strace-error.log.' >&2
    fi
    probe_pid=''
else
    echo 'strace unavailable; system-call profiling skipped.' > "$out/strace-error.log"
fi
if command -v perf >/dev/null; then
    echo 'Collecting CPU and scheduling counters...'
    perf stat -p "$server_pid" -o "$out/perf-stat.txt" \
        -e task-clock,context-switches,cpu-migrations,page-faults,cycles,instructions \
        -- sleep "$((seconds + 4))" 2> "$out/perf-error.log" &
    probe_pid=$!
    sleep 0.2
    if kill -0 "$probe_pid" 2>/dev/null; then
        load perf
        wait "$probe_pid" || echo 'Some perf counters were unavailable; inspect perf-stat.txt.' >&2
    else
        wait "$probe_pid" 2>/dev/null || true
        echo 'perf attachment failed; see perf-error.log.' >&2
    fi
    probe_pid=''
else
    echo 'perf unavailable; CPU profiling skipped.' > "$out/perf-error.log"
fi
echo "Diagnostics saved: $out"
