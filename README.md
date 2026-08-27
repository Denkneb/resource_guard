# Resource Guard

Resource Guard is a lightweight Linux daemon that watches processes owned by the current user and reports sustained CPU or memory limit violations. It provides desktop notifications with safe actions and a CLI for status, process inspection, configuration, and graceful termination.

The project is Linux-only. It does not require root privileges and is distributed as one Rust binary.

## Current features

- polling through `sysinfo`, with a five-second default interval;
- consecutive-sample, minimum-duration, and cooldown policies;
- protected, temporary-ignore, and permanent-ignore process rules;
- desktop notifications through `org.freedesktop.Notifications`;
- notification actions for `SIGTERM`, one-hour ignore, permanent ignore, and details;
- local authenticated control socket under `$XDG_RUNTIME_DIR/resource-guard`;
- `status` and daemon-backed `top` commands;
- PID reuse protection using PID, UID, and Linux process start time;
- foreground daemon suitable for a `systemd --user` service.

The CLI can send a separately confirmed `SIGKILL` only after `SIGTERM` fails. Notification actions never send `SIGKILL`.

## Build

Install a stable Rust toolchain, then build the release binary:

```console
cargo build --release --locked
```

Run the complete local checks with:

```console
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

## Install for the current user

Install the binary and user unit without `sudo`:

```console
install -Dm755 target/release/resource-guard "$HOME/.local/bin/resource-guard"
install -Dm644 packaging/resource-guard.service \
  "$HOME/.config/systemd/user/resource-guard.service"
systemctl --user daemon-reload
systemctl --user enable --now resource-guard.service
```

The packaged unit expects the binary at `~/.local/bin/resource-guard`. It creates the private runtime directory used by the control socket and restarts the daemon after unexpected failures.

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

The user service applies a restrictive umask, drops all capabilities, permits only Unix-domain sockets, and enables compatible systemd hardening options. It intentionally retains access to the current user's processes, configuration directory, and desktop session bus.

## Uninstall the user service

```console
systemctl --user disable --now resource-guard.service
rm "$HOME/.config/systemd/user/resource-guard.service"
systemctl --user daemon-reload
```

Remove `~/.local/bin/resource-guard` and the configuration separately if they are no longer needed.

## Contributing

Development setup and repository hooks are documented in [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

Resource Guard is licensed under the [MIT License](LICENSE).
