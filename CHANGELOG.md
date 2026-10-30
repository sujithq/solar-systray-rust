# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-10-30

### Added

- Initial changelog.
- `solar-core`: Rust port of Solar Agent's logic: DSMR parsing, surplus decision, quarter-hour
  peak forecast, daily energy, night baseline, sun times, simulation, and SlimmeLezer and
  Sungrow clients.
- `solar-systray.exe`: Windows notification-area app with level and kW icon styles, an
  information menu, appliance and simulation controls, configurable notifications, start at
  sign-in, and a config file seeded from Solar Agent's companion settings.
- Solar Agent first: while Solar Agent runs in solar mode, the tray shows its data from the
  local status service and opens no meter connection, because a SlimmeLezer does not reliably
  serve two clients. Appliance and simulation choices are sent to Solar Agent. The tray reads
  the meter itself when Solar Agent is not running, or 10 seconds after its local port becomes
  unreachable, its settings info file disappears, or it leaves solar mode.
- README with setup, configuration and build instructions.
- Opt-in Rust desktop smoke test covering the actual shell icon and native menu, agent-first
  startup, authenticated status errors, direct DSMR/Sungrow fallback, agent restart and shutdown,
  using isolated settings and loopback devices.
- The open tray menu refreshes its displayed values every second as new readings arrive.
- Release flow. The manually started Release workflow bumps the version, dates the changelog,
  checks, builds and packages `solar-systray.exe` with a SHA-256 checksum, and prepares the
  release commit and tag. It then calls the Deploy workflow, which pushes them and publishes
  the GitHub release after approval of the `release` environment. The version and changelog
  steps are Rust (`cargo xtask prepare` and `cargo xtask notes`).

### Changed

- The default meter host is the documentation address `192.0.2.10` instead of a private LAN
  address. Set your SlimmeLezer's address when switching `energySource.type` to `slimmelezer`.
  Examples and test data use documentation addresses and synthetic readings.

### Fixed

- Keep direct connections disabled when Solar Agent accepts TCP but times out, has no solar
  snapshot yet, reports an unknown mode, or has invalid/unreadable discovery settings. These
  conditions do not establish that it has released the meter.
