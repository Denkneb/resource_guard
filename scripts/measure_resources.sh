#!/usr/bin/env bash

set -euo pipefail
umask 077

export LC_ALL=C

measure_repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
measure_binary="$measure_repo_root/target/release/resource-guard"
measure_seconds=${MEASURE_SECONDS:-60}
measure_warmup_seconds=${MEASURE_WARMUP_SECONDS:-10}
measure_ram_limit_kib=$((25 * 1024))
measure_cpu_limit_percent=1.0
measure_temp_dir=
measure_daemon_pid=

if [[ ! $measure_seconds =~ ^[1-9][0-9]*$ ]]; then
    echo "MEASURE_SECONDS must be a positive integer" >&2
    exit 2
fi
if [[ ! $measure_warmup_seconds =~ ^[0-9]+$ ]]; then
    echo "MEASURE_WARMUP_SECONDS must be a non-negative integer" >&2
    exit 2
fi

cleanup() {
    if [[ -n $measure_daemon_pid ]] && kill -0 "$measure_daemon_pid" 2>/dev/null; then
        kill -TERM "$measure_daemon_pid" 2>/dev/null || true
        wait "$measure_daemon_pid" 2>/dev/null || true
    fi
    if [[ -n $measure_temp_dir && -d $measure_temp_dir ]]; then
        rm -rf -- "$measure_temp_dir"
    fi
}
handle_signal() {
    exit 130
}
trap cleanup EXIT
trap handle_signal INT TERM

cd -- "$measure_repo_root"
cargo build --release --locked

measure_temp_dir=$(mktemp -d -t resource-guard-measure.XXXXXXXXXX)
measure_config="$measure_temp_dir/config.toml"
measure_runtime="$measure_temp_dir/runtime"
measure_log="$measure_temp_dir/daemon.log"
mkdir -m 700 -- "$measure_runtime"

printf '%s\n' \
    '[monitor]' \
    'poll_interval_seconds = 5' \
    '' \
    '[notifications]' \
    'enabled = false' >"$measure_config"

RESOURCE_GUARD_CONFIG="$measure_config" \
RESOURCE_GUARD_RUNTIME_DIR="$measure_runtime" \
RUST_LOG=warn \
    "$measure_binary" daemon >"$measure_log" 2>&1 &
measure_daemon_pid=$!

measure_ready=false
for _ in {1..100}; do
    if RESOURCE_GUARD_CONFIG="$measure_config" \
        RESOURCE_GUARD_RUNTIME_DIR="$measure_runtime" \
        "$measure_binary" status >/dev/null 2>&1; then
        measure_ready=true
        break
    fi
    if ! kill -0 "$measure_daemon_pid" 2>/dev/null; then
        echo "resource-guard daemon exited during startup" >&2
        sed -n '1,120p' "$measure_log" >&2
        exit 1
    fi
    sleep 0.05
done
if [[ $measure_ready != true ]]; then
    echo "resource-guard daemon did not become ready" >&2
    sed -n '1,120p' "$measure_log" >&2
    exit 1
fi

sleep "$measure_warmup_seconds"

measure_clock_ticks=$(getconf CLK_TCK)
measure_start_ticks=$(awk '{ print $14 + $15 }' "/proc/$measure_daemon_pid/stat")
measure_start_ns=$(date +%s%N)
measure_peak_rss_kib=0

for ((measure_sample = 0; measure_sample < measure_seconds; measure_sample++)); do
    if ! kill -0 "$measure_daemon_pid" 2>/dev/null; then
        echo "resource-guard daemon exited during measurement" >&2
        sed -n '1,120p' "$measure_log" >&2
        exit 1
    fi
    measure_rss_kib=$(awk '/^VmRSS:/ { print $2 }' "/proc/$measure_daemon_pid/status")
    if ((measure_rss_kib > measure_peak_rss_kib)); then
        measure_peak_rss_kib=$measure_rss_kib
    fi
    sleep 1
done

measure_end_ns=$(date +%s%N)
measure_end_ticks=$(awk '{ print $14 + $15 }' "/proc/$measure_daemon_pid/stat")
measure_hwm_kib=$(awk '/^VmHWM:/ { print $2 }' "/proc/$measure_daemon_pid/status")
if ((measure_hwm_kib > measure_peak_rss_kib)); then
    measure_peak_rss_kib=$measure_hwm_kib
fi
measure_status=$(RESOURCE_GUARD_CONFIG="$measure_config" \
    RESOURCE_GUARD_RUNTIME_DIR="$measure_runtime" \
    "$measure_binary" status)

measure_elapsed_seconds=$(awk -v start="$measure_start_ns" -v end="$measure_end_ns" \
    'BEGIN { printf "%.3f", (end - start) / 1000000000 }')
measure_cpu_seconds=$(awk -v start="$measure_start_ticks" -v end="$measure_end_ticks" \
    -v ticks="$measure_clock_ticks" 'BEGIN { printf "%.3f", (end - start) / ticks }')
measure_cpu_percent=$(awk -v cpu="$measure_cpu_seconds" -v elapsed="$measure_elapsed_seconds" \
    'BEGIN { printf "%.3f", cpu * 100 / elapsed }')
measure_binary_bytes=$(stat -c '%s' "$measure_binary")
measure_processes=$(awk '/^processes:/ { print; exit }' <<<"$measure_status")

if ((measure_peak_rss_kib <= measure_ram_limit_kib)); then
    measure_ram_result=PASS
else
    measure_ram_result=FAIL
fi
measure_cpu_result=$(awk -v actual="$measure_cpu_percent" -v limit="$measure_cpu_limit_percent" \
    'BEGIN { print actual <= limit ? "PASS" : "FAIL" }')
measure_result=0
if [[ $measure_ram_result != PASS || $measure_cpu_result != PASS ]]; then
    measure_result=1
fi

printf '%s\n' \
    "binary_bytes=$measure_binary_bytes" \
    "warmup_seconds=$measure_warmup_seconds" \
    "measurement_seconds=$measure_elapsed_seconds" \
    "cpu_seconds=$measure_cpu_seconds" \
    "average_cpu_percent=$measure_cpu_percent" \
    "peak_rss_kib=$measure_peak_rss_kib" \
    "$measure_processes" \
    "ram_target_le_25_mib=$measure_ram_result" \
    "cpu_target_le_1_percent=$measure_cpu_result"

kill -TERM "$measure_daemon_pid"
wait "$measure_daemon_pid"
measure_daemon_pid=
exit "$measure_result"
