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

- Reported night-baseline alternative: only configured gridImport raw power
  kW/power/measurement history is eligible after atomic acquisition/coverage
  failure, not auth/transport failures. Use held start-state, fractional event
  ordering, a fully queried completed solar-night window, >=95% fully available
  minutes (exact integer comparison) and >=30 valid UTC-minute-start values.
  Exclude every minute touched by unknown/unavailable from the median, including
  a previously collected valid minute-start value; reject malformed inputs,
  never interpolate outages or substitute statistics/means. Label held-state
  median as a reported estimate, not physical acquisition. Cache identity must
  bind both entities/location/schedule/statistic/coverage policy and validated
  provenance, persist valid/total minutes and merged outages in version2 cache;
  prior valid results survive failed refresh. Bounds and live isolation match
  the atomic night path. Test constant held values, gaps, boundaries and cache.

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
  - Optional inverter AC (`activePower`) and DC (`dcPower`) are separate W-valued HA-reported displays, including initial/static zero. Age uses validated HA `last_updated` (which includes attribute changes), never receipt time or `last_reported`; no physical acquisition freshness is claimed. Preserve original source age plus monotonic aging across identical updates/reconnect snapshots; reject backwards timestamps and conflicting values at one source time. Values older than the independent 90-second default stay visibly stale; disconnect retains only visibly disconnected cached values and clears household use. Unknown/unavailable/removed/invalid values clear the affected display and estimate. Preserve bounded signed AC as-is; DC must be nonnegative and never substitutes for AC. Household use remains opt-in advisory, requiring supported topology/sign, fresh live atomic grid, valid AC HA-update age <=90s and `abs(atomic received_at - AC last_updated)` within configured <=30s skew, never meter-clock comparison. A recent snapshot may supply advisory AC but cannot renew its source time, provide atomic evidence or reset GO. Advancing `last_updated` with unchanged power supports only reported/advisory use, not physical acquisition proof. Test initial zero, static/stale/reconnect/disconnect/unavailable, age/skew and subsequent HA update recovery. Never change live atomic/paired-grid decision evidence as a side effect.
  - Demand, peak and four tariff counters come only from the same accepted atomic schema-v1 telegram, never separate entity mappings. Validate the six `optional_status` groups: available requires coherent values; missing/invalid requires explicit nulls (peak W/time/month all-or-none). Demand/peak are integer W in 0..1,000,000; counters integer Wh in 0..999,999,999. Peak UTC occurrence must not exceed the meter time and its exact YYYY-MM must match Europe/Brussels occurrence month; only a current-meter-month peak is eligible. Old grid-only publishers explicitly lack optional support. Advancing atomic acquisition supports unchanged numbers; snapshots/replays/heartbeats cannot renew them, missing fields never carry forward, and optional failure never rejects valid grid/GO. Use Brussels local day/month/minute and DST; label daily deltas partial, reject counter decreases and restart baselines honestly on day/session changes.
  - Preserve no-fallback startup/reload/tick routing and unverified household derivations. Cover protocol failures/reconnects with `crates/solar-core/tests/home_assistant.rs`, `crates/solar-core/tests/home_assistant_atomic.rs`, and HA settings/source-selection/native-smoke tests.
  - Recorded night baselines use a separate authenticated read-only WebSocket, never the live HaState. Retain the IP/TLS/frame bounds, 15-minute query chunks, 20-second command/180-second overall deadline and 32 MiB aggregate budget; require exact result IDs/entity and full compressed atomic attributes. Reject missing/unknown/unavailable/replayed samples or >=30-second coverage gaps; do not interpolate or substitute recorder statistics for the actual minute-sample median. Restore only bounded, validated, source/location/config-matching private caches; retain prior results visibly on refresh failure. Scheduling uses Brussels/DST and completed solar nights, never future/partial nights or PC-local dates. Never log tokens, raw history or private cache identity. Cover this in `tests/ha_baseline.rs` and fake-server history tests.
  - Recorded Today recovery reuses the bounded read-only history transport, with five-minute chunks, at most 128 MiB/100,000 samples/one 25-hour day and the same frame/command/overall deadlines. Require an exact Brussels-midnight atomic four-counter sample (or source/day-matching validated cached partial baseline), nondecreasing counters, consecutive sequences in one session, advancing meter/callback times and gaps <30s through an exact captured live anchor. No nearest-sample-as-midnight, interpolation, statistics or grid-only-history substitution. Report precise partial/full coverage and recorded failure reasons; reject cross-day/old-request races, bound concurrent live buffering, and never feed history into freshness/GO/forecast. Private bounded caches contain units/provenance/source/day only plus baseline/last counters, no credentials or raw history. Restore eligibility only after a recorded bridge to live; resets/session/day/identity changes cannot permit cross-boundary subtraction. Preserve the independent night-baseline policy. Cover cache/restart, gaps/reset, 23/25-hour DST days and history races in `tests/ha_daily.rs`, `ha_daily` unit tests and fake-server history tests.
  - Separately labeled HA reported daily energy may use original cumulative tariff recorder history with HA's documented held start-state at Brussels midnight; do not apply atomic same-telegram/physical freshness requirements to this different provenance or claim that projected boundary timestamp is an acquisition. Query only the four configured tariff entities over bounded read-only WS (same five-minute/1MiB/128MiB/100000-row/20s/180s limits). Require valid midnight/final states, boundary rows, kWh/energy/total_increasing metadata on numeric states, nonnegative whole-Wh values, ordered events (including fractional milliseconds) and consistent chunk boundaries. Intermediate unknown/unavailable rows may retain a nondecreasing cumulative reported endpoint delta only with explicit gap counts and reset-continuity-unverified labeling; never infer missing/reset energy or claim reset-corrected statistics. Reject malformed numeric values, observed resets/decreases and invalid endpoints rather than abs-normalizing. Sum import tariffs and export tariffs separately, never combine directions or overlap atomic/live/statistics periods. Label held boundary, exact query as-of and retained/error/stale age; reject old-day results. No historical state may renew grid/GO/AC/household/forecast. Preserve atomic coverage/cache and night statistics as separate features; test original-grid-only-era history, static inactive tariffs, availability gaps and invalid/reset inputs.
