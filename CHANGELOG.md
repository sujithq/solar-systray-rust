# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- HA Today now queries the original cumulative day/night tariff recorder
  entities for a separately labeled reported-energy total using HA's held
  midnight start state. Older history no longer unnecessarily depends on the
  recently added atomic counter schema or an event exactly at midnight.
  Import/export tariff sums remain directional, with reset checks and explicit
  source/as-of/stale labels; live grid freshness is unchanged.
- HA heartbeat commands now use strictly increasing IDs instead of reusing ID 3.
  Reusing the second heartbeat ID caused HA to reject the command and the client
  to reconnect. Pongs remain strictly correlated; invalid responses still fail closed.
- HA peak/daily features now consume same-telegram optional fields from the
  additive atomic schema-v1 extension, including unchanged demand/static peaks
  and idle tariff counters. Null/status/range and peak occurrence-month checks
  isolate optional failures from grid/GO; old grid-only publishers report the
  missing extension explicitly, with no separate-entity fallback.

### Added

- HA Today energy can recover a validated Brussels-midnight four-counter
  baseline from bounded WebSocket recorder history, persist source/day-bound
  provenance privately, and bridge restart/reconnect to live counters.
  Exact midnight, continuous sequences and nondecreasing counters are required;
  missing/pre-extension/gapped history stays explicitly partial with exact
  sample coverage, never interpolated or counted as live freshness.
- HA night baseline restored from a private source-bound cache at startup and
  recalculated from recorded atomic samples at a configurable Brussels-local
  daily time (default 06:00). Only completed nights with real minute-sample
  coverage qualify; stale/retained/default/error states are visible and history
  never contributes to live grid freshness or GO.
- Optional Home Assistant inverter AC and DC displays from separate configurable
  W-valued entities. Each expires independently after 90 seconds using only
  last-known state-change age; neither can renew grid freshness. A separate
  household-use estimate remains disabled unless explicitly configured with a
  supported topology, AC generation sign, and bounded HA-event proximity.
- HA quarter-hour peak forecasting and partial daily-energy monitoring use
  same-telegram demand, peak occurrence/month and four tariff counters, the
  atomic UTC meter clock with Brussels DST rules, and labeled partial-day
  baselines. Optional data never affects grid/GO; publisher deployment and live
  acceptance remain separate from client readiness.
- Isolated native HA monitoring via an existing absolute
  `SOLAR_SYSTRAY_HA_CONFIG` file, with HA-only atomic startup/reload validation,
  no primary settings seeding and no autostart changes.

- Opt-in bounded HA-only monitoring harness with sanitized acceptance reporting,
  client-only stale/recovery checks and unchanged 30-second/300-second safety limits.
  Reports include sampled error/reconnect and decision-predicate diagnostics;
  acceptance requires 300 seconds of observed continuous freshness.

- Explicit Home Assistant WebSocket-only source with independent authentication,
  race-safe subscription/snapshot initialization, bounded reconnects, per-entity
  diagnostics and fail-safe provisional freshness. HA mode bypasses Solar Agent
  and all direct hardware readers at startup, reload and every tick.
- Configurable grid import/export mappings (kW to W), optional distinct AC display,
  preserved 30-second meter/90-second inverter defaults and bounded grid skew.
  Cached/unchanged states cannot renew recommendations; daily/quarter-hour and
  household derivation is disabled by default and remains gated pending topology/sign
  confirmation and live acceptance.
- Configurable atomic DSMR grid-sample feed with same-telegram W values, session/sequence
  replay checks and independent meter/callback times. Advancing acquisition sequences
  keep unchanged power fresh and earn the full 300-second GO interval within the unchanged
  30-second stale bound. Snapshot/reconnect/session changes reset eligibility; unavailable
  atomic samples never fall back to separate HA powers. The publisher was deployed
  separately; physical calibration and household/topology acceptance remain pending.

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
