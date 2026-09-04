# Resource Guard

[![CI](https://github.com/Denkneb/resource_guard/actions/workflows/ci.yml/badge.svg)](https://github.com/Denkneb/resource_guard/actions/workflows/ci.yml)

Resource Guard is a lightweight Linux daemon that watches processes owned by the current user and reports sustained CPU or memory limit violations. It provides desktop notifications with safe actions and a CLI for status, process inspection, configuration, and graceful termination.

The project is Linux-only. It does not require root privileges and is distributed as one Rust binary.

## Current features

- polling through `sysinfo`, with a five-second default interval;
- lightweight system memory pressure monitoring through available RAM, swap, and Linux PSI;
- consecutive-sample, minimum-duration, and cooldown policies;
- protected, temporary-ignore, and permanent-ignore process rules;
- desktop notifications through `org.freedesktop.Notifications`;
- notification actions for `SIGTERM`, one-hour ignore, permanent ignore, and details;
- local authenticated control socket under `$XDG_RUNTIME_DIR/resource-guard`;
- `status` and daemon-backed `top` commands;
- PID reuse protection using PID, UID, and Linux process start time;
- foreground daemon suitable for a `systemd --user` service.
- opt-in emergency termination of allowlisted or largest unprotected current-user processes.

The CLI can send a separately confirmed `SIGKILL` only after `SIGTERM` fails. Notification actions never send `SIGKILL`.

## Build

Install Rust 1.95 or newer, then build the release binary:

```console
cargo build --release --locked
```

Run the complete local checks with:

```console
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

The measured release daemon uses 5.99 MiB peak RSS and averages 0.498% of one logical CPU core in the current 60-second normal-pressure baseline. See [`docs/PERFORMANCE.md`](docs/PERFORMANCE.md) for the environment, methodology, limitations, and reproduction script.

## Install a published release

Release archives currently target 64-bit glibc-based Linux (`x86_64-unknown-linux-gnu`). Download the archive and its `.sha256` file from the corresponding GitHub release, then verify and extract it:

```console
sha256sum --check resource-guard-0.2.1-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf resource-guard-0.2.1-x86_64-unknown-linux-gnu.tar.gz
cd resource-guard-0.2.1-x86_64-unknown-linux-gnu
```

Install the extracted binary and user service without `sudo`:

```console
install -Dm755 bin/resource-guard "$HOME/.local/bin/resource-guard"
install -Dm644 systemd/resource-guard.service \
  "$HOME/.config/systemd/user/resource-guard.service"
install -Dm644 applications/io.github.denkneb.ResourceGuard.desktop \
  "$HOME/.local/share/applications/io.github.denkneb.ResourceGuard.desktop"
systemctl --user daemon-reload
systemctl --user enable --now resource-guard.service
```

## Install a source build for the current user

Install the binary and user unit without `sudo`:

```console
install -Dm755 target/release/resource-guard "$HOME/.local/bin/resource-guard"
install -Dm644 packaging/resource-guard.service \
  "$HOME/.config/systemd/user/resource-guard.service"
install -Dm644 packaging/io.github.denkneb.ResourceGuard.desktop \
  "$HOME/.local/share/applications/io.github.denkneb.ResourceGuard.desktop"
systemctl --user daemon-reload
systemctl --user enable --now resource-guard.service
```

The packaged unit expects the binary at `~/.local/bin/resource-guard`. It creates the private runtime directory used by the control socket and restarts the daemon after unexpected failures. The desktop entry gives notifications a stable application identity so compatible desktop environments can group them and retain them in notification history.

Inspect the service and its logs with:

```console
systemctl --user status resource-guard.service
journalctl --user -u resource-guard.service
```

The repository does not automatically copy files into your home directory or invoke `systemctl`.

## Configuration

The default configuration path is:

```text
$XDG_CONFIG_HOME/resource-guard/config.toml
```

When `XDG_CONFIG_HOME` is unset, Resource Guard uses `~/.config/resource-guard/config.toml`.

Create and validate a configuration with:

```console
resource-guard config init
resource-guard config check
resource-guard config
```

See [`config.example.toml`](config.example.toml) for all available settings. Explicit executable paths in protected and ignored lists must be absolute.

The “Always ignore” notification action prefers the process executable path and falls back to its exact name. It atomically writes a complete normalized TOML document; comments in an existing file are therefore not preserved.

Set `notifications.enabled = false` for a headless environment. If notifications are enabled but the desktop D-Bus service is unavailable, monitoring and CLI access continue normally, the error appears in `status`, and the daemon retries the connection after a cooldown.

## Memory pressure and emergency mode

Resource Guard separately monitors system-wide memory availability, swap usage, and pressure stall information from `/proc/pressure/memory`. Swap occupancy is treated as supporting evidence only when available RAM is also critically low; stale pages in a full swap do not independently create pressure or prevent recovery after RAM and PSI normalize. Normal pressure sampling follows the regular five-second interval. Warning and critical states use the shorter intervals configured in `[memory_pressure]` without continuously scanning every process at the fastest rate.

The default policy only reports pressure and never terminates a process automatically:

```toml
[emergency]
action = "notify_only"
allow_sigkill = false
```

To terminate only explicitly approved applications during persistent critical pressure, use exact process names or preferably absolute executable paths:

```toml
[emergency]
action = "terminate_allowlisted"
allow_sigkill = false
action_available_mib = 1024
action_psi_full_avg10 = 5.0
allowed_names = []
allowed_executables = ["/absolute/path/to/a/restartable-worker"]
exempt_names = ["resource-guard"]
exempt_executables = []
```

`terminate_largest_unprotected` is a more aggressive explicit opt-in. It selects the largest eligible process owned by the current user. Protected and emergency-exempt processes are never candidates. Ordinary ignored processes remain eligible because ignoring routine notifications is not an emergency safety guarantee.

Critical pressure and permission to terminate are separate decisions. Full swap combined with RAM below `critical_available_percent` can produce a critical warning and faster polling, but automatic action is permitted only when available RAM reaches `action_available_mib`, or when RAM is critically low and PSI full avg10 reaches `action_psi_full_avg10`. Both automatic-action thresholds are configurable under `[emergency]`.

Emergency actions revalidate PID, UID, and process start time before every signal. Resource Guard first sends `SIGTERM`, waits for the configured grace period, and checks that the automatic-action condition still holds. It can send `SIGKILL` only when `allow_sigkill = true`; this is disabled by default. Only one process is handled at a time, followed by an action cooldown.

The emergency floor can enter the critical state immediately. Other critical conditions require the configured number of consecutive samples. Recovery uses a higher available-memory threshold to prevent rapid state oscillation. See [`config.example.toml`](config.example.toml) for all pressure thresholds and intervals.

Userspace polling cannot guarantee recovery from every sudden allocation spike. Swap, cgroup memory controls, and a system OOM service remain useful independent safeguards.

## CLI

Run the daemon in the foreground:

```console
resource-guard daemon
```

Inspect daemon and system state:

```console
resource-guard status
```

Show the latest monitored process snapshot, once or continuously:

```console
resource-guard top
resource-guard top --watch
```

Gracefully stop a process:

```console
resource-guard stop PID
```

Request a forceful fallback if the process survives its configured grace period:

```console
resource-guard stop PID --kill
```

The interactive command requires typing the exact PID before `SIGKILL` is sent. For explicit non-interactive automation, use `resource-guard stop PID --kill --yes`. If the process exits after `SIGTERM`, no confirmation is requested and no `SIGKILL` is sent.

Before each signal, Resource Guard verifies that the process still has the expected PID, UID, and start time and is not protected. The pidfd adapter repeats the identity check immediately before signalling. Only processes owned by the current user are eligible.

## Runtime and security model

The daemon exposes `$XDG_RUNTIME_DIR/resource-guard/control.sock`. The directory mode is `0700`, the socket mode is `0600`, and every accepted connection is checked with Linux peer credentials.

Resource spikes do not immediately trigger notifications: the configured number of consecutive samples and minimum duration must both be reached. Notification IDs are mapped to full process identities, so an action cannot silently follow a reused PID.

The user service applies a restrictive umask, prevents privilege escalation, permits only Unix-domain sockets, and enables compatible systemd hardening options. It runs as the current unprivileged user, whose process starts without Linux capabilities. It intentionally retains access to the current user's processes, configuration directory, and desktop session bus.

## Uninstall the user service

```console
systemctl --user disable --now resource-guard.service
rm "$HOME/.config/systemd/user/resource-guard.service"
rm "$HOME/.local/share/applications/io.github.denkneb.ResourceGuard.desktop"
systemctl --user daemon-reload
```

Remove `~/.local/bin/resource-guard` and the configuration separately if they are no longer needed.

## Contributing

Development setup and repository hooks are documented in [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Security

Please report vulnerabilities privately as described in [`SECURITY.md`](SECURITY.md). Do not include vulnerability details in public issues or pull requests.

## License

Resource Guard is licensed under the [MIT License](LICENSE).
