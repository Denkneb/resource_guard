# Changelog

All notable changes to Resource Guard are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Added typed desktop notification closure diagnostics and private D-Bus integration coverage for notification payloads, actions, replacement IDs, and closure signals.

### Fixed

- Removed hardening directives that prevented the service from starting under `systemd --user` when the unprivileged manager cannot adjust capability bounding sets.
- Added a stable desktop application identity and persistence hints so supported notification servers can retain and group Resource Guard warnings.
- Added safe in-place navigation from notification details back to the actionable summary.

## [0.1.0] - 2026-08-27

### Added

- Linux daemon for monitoring sustained per-process CPU and memory violations.
- Current-user ownership checks and PID reuse protection using process start time.
- Repeated-sample, minimum-duration, cooldown, protection, and ignore policies.
- Desktop notifications with stop, temporary-ignore, permanent-ignore, and details actions.
- CLI commands for daemon control, status, process inspection, configuration, and signalling.
- Graceful `SIGTERM` handling with separately confirmed, race-resistant `SIGKILL` fallback.
- Authenticated Unix control socket and hardened `systemd --user` service.
- Unit, adapter, CLI, process-signalling, packaging, and resource-baseline tests.
- CI, dependency policy checks, Dependabot, and local pre-commit/pre-push hooks.

[Unreleased]: https://github.com/Denkneb/resource_guard/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Denkneb/resource_guard/releases/tag/v0.1.0
