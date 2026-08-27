# Performance baseline

Resource Guard is measured as an optimized release binary running its daemon with the default five-second polling interval. The repeatable local acceptance thresholds are:

- peak resident memory no greater than 25 MiB;
- average CPU usage no greater than 1% of one logical core over the measurement window.

The CPU value includes process scans and control-socket work. Between polling cycles the daemon waits on asynchronous timers and socket events rather than busy-looping.

## Baseline result

The baseline was recorded on 2026-08-27 from commit `ca52c0f` plus the measurement script and this documentation:

| Measurement | Result |
| --- | ---: |
| Release binary size | 7,470,440 bytes (7.12 MiB) |
| Warm-up | 10 seconds |
| Measurement window | 60.334 seconds |
| Process CPU time | 0.460 seconds |
| Average CPU | 0.762% of one logical core |
| Peak RSS | 7,104 KiB (6.94 MiB) |
| Processes in final snapshot | 1,342 observed, 721 monitored |

Both acceptance thresholds passed. The measurements were taken with Rust 1.94.1 on Linux 6.8.0-138-generic x86_64. Results depend on the host, process count, kernel, allocator, and build toolchain, so this is a baseline rather than a universal resource guarantee.

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
