---
name: security-review
description: Security checks for JSON configuration, DSMR telegrams, Sungrow Modbus TCP responses, and Solar Agent's local status service. Use when changing these trust boundaries.
---

# Security review

## Trust boundaries in this repo

- `crates/solar-core/src/config.rs` accepts JSON values and validates configuration before creating a `SolarConfig`.
- `crates/solar-core/src/service.rs` reads meter data from a configured TCP host.
- `crates/solar-core/src/dsmr.rs` frames and parses meter telegrams; the stream is size- and age-bounded, and readings require a verified CRC unless legacy mode is explicitly enabled.
- `crates/solar-core/src/sungrow.rs` reads Modbus TCP responses and checks transaction, unit, length, and power ranges before returning measurements.
- `crates/solar-core/src/agent.rs` reads Solar Agent's settings info file (port and access token) and talks HTTP to its local service. Both the file and every HTTP response are untrusted: any local process could write the file or listen on the port.
- `crates/solar-core/src/home_assistant.rs` validates HA endpoint/credential references and consumes untrusted WebSocket auth, snapshot and state events. HA mode in `service.rs` and `solar-tray/src/app.rs` must never create Agent or hardware fallback readers.

## Before merging a change to these paths

- Keep DSMR frame size and age limits, checksum policy, and field/range validation intact; cover changes with malformed-input tests.
- Keep Modbus response length, transaction, unit, function, register-count, and power-range checks before decoding measurements.
- Keep configuration host, port, appliance, coordinate, and numeric validation in `SolarConfig::from_json`; add tests for any changed accepted input.
- Solar Agent client:
  - Connect only to `127.0.0.1`, whatever the settings file or a response says.
  - Never log, display, or store the access token; `Endpoint`'s `Debug` output must keep redacting it.
  - Keep the size limits (settings file, headers, body, chunk-size lines), the connect/read/write timeouts and the overall response deadline.
  - Keep strict HTTP framing: reject conflicting `Content-Length`, unknown `Transfer-Encoding`, and `Content-Length` combined with chunked.
  - Deserialize into typed data, validate addresses before showing them, and keep error text short and single-line.
  - Cover changes with the fake-server tests in `crates/solar-core/tests/agent.rs`.
- Home Assistant client:
  - Native `SOLAR_SYSTRAY_HA_CONFIG` overrides must require an existing absolute file
    and explicit atomic HA mode on startup and reload. Never seed a missing override,
    change the primary settings, or expose autostart changes in isolated monitoring.
  - Require ws/wss IP endpoints on `/api/websocket`; reject embedded credentials, queries, fragments and control characters. Never follow redirects or disable TLS certificate validation.
  - Store only a validated environment-variable reference in configuration. Never expose the token or server-provided auth/error text, including through Debug or notifications.
  - Preserve connect, upgrade/TLS byte budget/deadline, read/write, command and heartbeat deadlines, cancellation slices, message/frame limits, aggregate initialization-buffer limits and bounded reconnect backoff.
  - Subscribe before requesting the WebSocket snapshot; correlate IDs, validate event entity identity and do not apply buffered events older than the snapshot.
  - Validate units, finite/ranged power and source timestamps before using values. Maintain independent grid/AC ages and bounded grid skew; invalid optional AC never invalidates valid grid.
  - Separate-entity snapshots/reconnects, repeated numeric powers, attributes, heartbeats and unrelated entities cannot renew evidence or restore GO. Source value changes remain explicitly provisional, not verified physical acquisition.
  - An explicit `atomicGrid` mapping exclusively selects same-telegram schema-v1 samples, not a clock attached to cached powers. Validate integer state/sequence agreement, bounded lifecycle session IDs, finite nonnegative W fields, timezone-aware UTC meter/callback times, age <30s and future <=5s; never weaken stale defaults.
  - Atomic live sequence, meter time and callback time must advance together, including unchanged zeros. Snapshots establish watermarks only; reconnect/session changes reset GO and require new live acquisition. Retired sessions/replays/nonadvancing time fail closed; keep replay cache bounded. An unavailable atomic feed never falls back to paired entities. Cover these guarantees in `crates/solar-core/tests/home_assistant_atomic.rs`.
  - Optional inverter AC (`activePower`) and DC (`dcPower`) are separate W-valued displays with independent 90-second last-known state-change age. Preserve bounded signed AC as-is; DC must be nonnegative. Household use is disabled by default; only the explicit supported topology/sign plus atomic-grid/AC freshness and provisional HA-event proximity gates may enable its estimate. Compare atomic `received_at` to AC `last_changed`, never meter-clock time; label this as provisional, not physical coherence. Do not take absolute AC or substitute DC. Do not treat `last_reported`, connectivity, heartbeats, unchanged states or reconnect snapshots as acquisition evidence. Invalid/out-of-range/wrong-unit values disable only the affected optional calculation/display, never grid.
  - Optional demand/current-month-peak and daily tariff counters must be individually mapped, unit/range/class validated, and accepted only after live value changes within independent bounded last-change age and event proximity to the atomic callback. This is provisional event coherence, not same-telegram evidence; do not renew it from snapshots, equal values, `last_updated`, `last_reported`, heartbeats or unrelated entities. Use the atomic UTC meter timestamp converted to Europe/Brussels local day/month/minute with DST handling. Mark daily output partial from the first observed baseline; reject counter decreases and day/month boundary mismatches. Missing/stale/invalid optional inputs disable only the affected feature, never grid/GO. True same-telegram feature parity requires acquisition-side atomic demand, peak/month identity and all tariff counters under the same advancing sequence.
  - Preserve no-fallback startup/reload/tick routing and unverified household derivations. Cover protocol failures/reconnects with `crates/solar-core/tests/home_assistant.rs`, `crates/solar-core/tests/home_assistant_atomic.rs`, and HA settings/source-selection/native-smoke tests.
