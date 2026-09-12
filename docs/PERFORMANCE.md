# Performance baseline

Resource Guard is measured as an optimized release binary running its daemon with the default five-second polling interval. The repeatable local acceptance thresholds are:

- peak resident memory no greater than 25 MiB;
- average CPU usage no greater than 1% of one logical core over the measurement window.

The CPU value includes process scans and control-socket work. Between polling cycles the daemon waits on asynchronous timers and socket events rather than busy-looping.

## Baseline result

The baseline was recorded on 2026-09-12 from the Resource Guard 0.4.0 background-workload change set:

| Measurement | Result |
| --- | ---: |
| Release binary size | 8,617,296 bytes (8.22 MiB) |
| Warm-up | 10 seconds |
| Measurement window | 60.333 seconds |
| Process CPU time | 0.470 seconds |
| Average CPU | 0.779% of one logical core |
| Peak RSS | 8,188 KiB (8.00 MiB) |
| Processes in final snapshot | 1,218 observed, 584 monitored |

Both acceptance thresholds passed. Across four consecutive standard runs of the same build, average CPU ranged from 0.779% to 0.912% and peak RSS from 8,124 KiB to 8,196 KiB. The measurements were taken with Rust 1.98.0 on Linux 6.8.0-139-generic x86_64. The default `notify_only` emergency policy and the five-second process polling interval were active; the host was in the warning memory-pressure state (about 11.75% available RAM) during the recorded run, which only affects the lightweight pressure poll. Background detection never terminates a process automatically. Results depend on the host, process count, pressure state, kernel, allocator, and build toolchain, so this is a baseline rather than a universal resource guarantee. Warning and critical pressure states poll the pressure source more frequently and may use more CPU while the system is under memory pressure.

## Reproducing the measurement

Run from the repository root:

```console
scripts/measure_resources.sh
```

The script:

1. builds `target/release/resource-guard` with `cargo build --release --locked`;
2. creates isolated configuration and runtime directories under the system temporary directory;
3. disables desktop notifications to measure the daemon independently of a D-Bus implementation;
4. warms up for 10 seconds, then samples `/proc` for 60 seconds;
5. prints binary size, CPU time, average CPU, peak RSS, process counts, and target results, returning a non-zero status if either target fails;
6. terminates the daemon and removes all temporary files.

It does not require root access and does not modify the user's normal Resource Guard configuration. Shorter diagnostic runs can override the timings:

```console
MEASURE_WARMUP_SECONDS=1 MEASURE_SECONDS=5 scripts/measure_resources.sh
```

The script requires Linux `/proc`, Bash, Cargo, and common GNU userland tools including `awk`, `date`, `getconf`, and `stat`.
