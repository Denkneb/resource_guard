# Performance baseline

Resource Guard is measured as an optimized release binary running its daemon with the default five-second polling interval. The repeatable local acceptance thresholds are:

- peak resident memory no greater than 25 MiB;
- average CPU usage no greater than 1% of one logical core over the measurement window.

The CPU value includes process scans and control-socket work. Between polling cycles the daemon waits on asynchronous timers and socket events rather than busy-looping.

## Baseline result

The baseline was recorded on 2026-08-27 from the Resource Guard 0.2.0 emergency-mode change set:

| Measurement | Result |
| --- | ---: |
| Release binary size | 8,133,776 bytes (7.76 MiB) |
| Warm-up | 10 seconds |
| Measurement window | 60.252 seconds |
| Process CPU time | 0.300 seconds |
| Average CPU | 0.498% of one logical core |
| Peak RSS | 6,132 KiB (5.99 MiB) |
| Processes in final snapshot | 779 observed, 214 monitored |

Both acceptance thresholds passed. The measurements were taken with Rust 1.98.0 on Linux 6.8.0-138-generic x86_64. The default `notify_only` emergency policy and normal five-second pressure interval were active. Results depend on the host, process count, pressure state, kernel, allocator, and build toolchain, so this is a baseline rather than a universal resource guarantee. Warning and critical modes intentionally poll more frequently and may use more CPU while the system is under memory pressure.

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
