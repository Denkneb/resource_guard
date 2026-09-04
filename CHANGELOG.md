# Changelog

All notable changes to Resource Guard are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.2] - 2026-09-04

### Fixed

- Separated critical-pressure reporting from automatic process termination so full swap and low-but-unstalled RAM no longer terminate allowlisted applications.
- Added configurable available-memory and PSI thresholds for automatic emergency actions, including status, notification, and log diagnostics.

## [0.2.1] - 2026-08-28

### Fixed

- Prevented stale high swap occupancy from keeping memory pressure critical and repeatedly terminating newly started allowlisted processes after RAM and PSI recover.

## [0.2.0] - 2026-08-27

### Added

- Added system-wide memory pressure monitoring based on available RAM, swap usage, and Linux PSI.
- Added adaptive pressure polling, warning/critical/recovery states, and pressure details in daemon status and desktop notifications.
- Added opt-in emergency policies for terminating allowlisted or largest unprotected current-user processes, with PID reuse protection, graceful termination, cooldowns, and separately enabled forceful fallback.
- Added unit and controlled child-process integration coverage for pressure evaluation and emergency termination.

## [0.1.1] - 2026-08-27

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

[Unreleased]: https://github.com/Denkneb/resource_guard/compare/v0.2.2...HEAD
[0.2.2]: https://github.com/Denkneb/resource_guard/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/Denkneb/resource_guard/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/Denkneb/resource_guard/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/Denkneb/resource_guard/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Denkneb/resource_guard/releases/tag/v0.1.0
