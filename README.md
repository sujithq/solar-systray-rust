# Solar Systray

![Solar Systray dashboard illustration: live solar export and a start-now signal](assets/solar-systray-cover.svg)

[![CI](https://github.com/sujithq/solar-systray-rust/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/sujithq/solar-systray-rust/actions/workflows/ci.yml)
[![Release](https://github.com/sujithq/solar-systray-rust/actions/workflows/release.yml/badge.svg?branch=main)](https://github.com/sujithq/solar-systray-rust/actions/workflows/release.yml)
[![Deploy](https://github.com/sujithq/solar-systray-rust/actions/workflows/deploy.yml/badge.svg?branch=main)](https://github.com/sujithq/solar-systray-rust/actions/workflows/deploy.yml)

[![Rust 1.88+](https://img.shields.io/badge/Rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Windows / Win32](https://img.shields.io/badge/Windows-Win32-0078D4)](https://microsoft.github.io/windows-docs-rs/)
[![Cargo](https://img.shields.io/badge/build-Cargo-A65E2E?logo=rust&logoColor=white)](https://doc.rust-lang.org/cargo/)
[![Serde / JSON](https://img.shields.io/badge/Serde-JSON-5B8C3E)](https://serde.rs/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A minimal Windows notification-area (system tray) app that shows the same solar-surplus
information as [Solar Agent](https://github.com/madebyq/solar-agent): whether it is a good
moment to start a big appliance, what the grid is doing right now, and whether this
quarter-hour is about to set a new monthly peak. Everything is written in Rust.

It runs in the background as one small executable (about 1.1 MB) with no runtime to install.
Explicit Home Assistant mode uses HA WebSockets only. In legacy modes, when Solar Agent
is running it shows Solar Agent's data; otherwise it reads the meter itself, so Solar Agent
does not need to be running. See [Where the data comes from](#where-the-data-comes-from).

## What it shows

### Tray icon

Choose between two icon styles under **Icon style**:

| Level symbol (default) | Power in kW |
|---|---|
| Grey ⊘: no meter reading | `--` on grey |
| Indigo moon: night | |
| Red arrow: importing from the grid | Import in kW on red, for example `0.7` |
| Amber sun: producing, little or no surplus | Export in kW on the level colour, for example `4.2`; `10`–`99` above 9.9 kW |
| Light green sun: surplus | |
| Dark green sun: maximum surplus | |

On top of either style:

- **Green dot / bottom bar**: GO. Export has been stable above the selected appliance's
  start threshold for the configured time, so it is safe to start the appliance now.
- **Orange-red dot / top bar**: peak warning. The quarter-hour forecast will exceed this
  month's peak (or the 2.5 kW capacity-tariff minimum), or the peak has already been reached.

Hovering the icon shows a compact summary: the headline, grid power (plus PV for Sungrow
or separately labeled AC/DC for HA when mapped), the appliance's start threshold,
quarter-hour forecast and today's energy.

### Menu

Left- or right-click the icon:

- **Headline** with the reason, for example `GO IN 4:38` and `Stable for 0:23 of 5:00`.
- **Power now**: grid export/import; Sungrow values or separately mapped HA inverter AC and DC;
  household use only when its derivation is available; and the night baseline.
- **Appliance**: pick the appliance to plan for; shows the export needed to start it.
- **Quarter-hour peak**: current demand, monthly peak, forecast, and how much import to cut.
- **Today**: energy imported and exported today, by the meter's clock.
- **Connections**: who reads the meter (`Solar Agent`, `Solar Systray` or HA), sunrise and
  sunset, meter/inverter state and the HA optional-inverter age limitations.
- **Simulation** (when simulating): switch scenarios or set a grid power.
- **Icon style**, **Notifications**, **Start at sign-in**, **Open config file**,
  **Reload config** and **Quit**.

The open menu refreshes its displayed values every second; new Solar Agent snapshots arrive
every two seconds. You can leave the menu open to watch readings change without reopening it.
If a data-source or menu layout change occurs while the menu is open, close and reopen it to
see the new options.

### Notifications

Each can be switched off under **Notifications**, or all at once. They are standard Windows
notifications, so Do not disturb applies to them.

| Notification | When |
|---|---|
| GO gained | It became safe to start the selected appliance |
| GO lost | It is no longer safe |
| Peak forecast | This quarter-hour is on course to exceed the monthly peak; says how much import to cut and how long is left |
| Monthly peak | The quarter-hour demand reached this month's peak |
| Meter offline | No meter reading for `offlineNotifyAfterSeconds` (default 2 minutes), and again when it comes back |

When simulating, the titles start with `[SIM]`.

## Where the data comes from

**Explicit Home Assistant mode** (`solar.energySource.type: "home-assistant"`) is
independent of the legacy chooser described below. HA is the sole hardware reader;
the tray authenticates directly to HA's `/api/websocket` and runs its own existing
decision engine and appliance controls. It starts no Solar Agent client, meter
link or inverter poller, including after settings reload and during reconnects.
There is **no REST, Solar Agent API or direct-device fallback** when HA is unavailable.
Connections shows `Read by Home Assistant`, transport/initialization state,
per-entity evidence ages and sanitized failure codes.

The first implementation uses the standard WebSocket `subscribe_events`
(`state_changed`) command followed by WebSocket `get_states`. Subscribing before
the snapshot, buffering relevant events and discarding events older than that
snapshot avoids the initialization race. This deliberately uses full-state
messages rather than `subscribe_entities`' compressed deltas so timestamp and
unit validation use one representation. Commands, messages, initialization,
heartbeat responses and reconnect backoff are bounded. Each reconnect reauthenticates
and resubscribes; it discards eligible readings and resets GO stability.
Command IDs strictly increase per connection, including every heartbeat; subscription
events retain their original subscription ID. Only the pending heartbeat's matching
pong clears its deadline. Duplicate/unmatched pongs and unexpected results remain
protocol errors; explicit heartbeat rejection reports `ha-heartbeat-rejected`.

### HA configuration and acceptance limits

Merge these sections into the `solar` object of your existing config:

```json
{
  "energySource": { "type": "home-assistant" },
  "homeAssistant": {
    "endpoint": "ws://127.0.0.1:8123/api/websocket",
    "tokenEnv": "SOLAR_HA_TOKEN",
    "atomicGrid": "sensor.dsmr_atomic_grid_sample",
    "gridImport": "sensor.electricity_meter_power_consumption",
    "gridExport": "sensor.electricity_meter_power_production",
    "activePower": "sensor.sg5_0rs_local_total_active_power",
    "dcPower": "sensor.sg5_0rs_local_total_dc_power",
    "inverterStaleSeconds": 90,
    "gridSkewSeconds": 5,
    "currentDemand": "sensor.electricity_meter_current_average_demand",
    "monthlyPeak": "sensor.electricity_meter_maximum_demand_current_month",
    "energyImportTariff1": "sensor.electricity_meter_energy_consumption_tarif_1",
    "energyImportTariff2": "sensor.electricity_meter_energy_consumption_tarif_2",
    "energyExportTariff1": "sensor.electricity_meter_energy_production_tarif_1",
    "energyExportTariff2": "sensor.electricity_meter_energy_production_tarif_2",
    "meterDataStaleSeconds": 90,
    "meterDataMaxEventSkewSeconds": 30,
    "householdUse": {
      "enabled": false,
      "topology": "",
      "generationSign": 0,
      "maxEventSkewSeconds": 5
    }
  },
  "decision": { "staleAfterSeconds": 30, "stableDurationSeconds": 300 }
}
```

`tokenEnv` names a process environment variable containing an **existing** HA token;
the credential itself is never written to the config, logs, diagnostics or error
messages. Provision it privately to the launched process using your credential
manager/launcher, not a source-controlled file or shared command. This change
does not create credentials or configure Windows to persist them.

The endpoint must use an IP literal and the exact `/api/websocket` path, without
credentials, queries or fragments. IP-only addressing avoids unbounded OS DNS
resolution; hostname URLs are not supported in this slice. Use `wss` with a
trusted certificate valid for that IP across untrusted networks. `ws` transmits
the token unencrypted and is appropriate only on a trusted local transport.
TLS validation is never disabled, and redirects are not followed.

#### Decision-grade atomic grid samples

`atomicGrid` selects one atomic DSMR sample entity **exclusively**. The example ID
is configurable: HA's registry may suffix it (for example
`sensor.dsmr_atomic_grid_sample_2`). Configure the actual publisher entity after
its separately approved setup; this client does not install a publisher. A missing,
invalid or unavailable atomic entity never falls back to the separate power
entities, REST, Solar Agent or hardware.

The acquisition-side publisher uses HA's existing complete, checksum-accepted
DSMR callback, not a second hardware reader. It must emit one entity update per
accepted telegram containing both validated powers, even when both are unchanged.
The numeric entity state is the positive integer `sample_sequence`. Its attributes
must contain:

| Attribute | Required contract |
| --- | --- |
| `schema_version` | Integer `1` |
| `session` | New random lifecycle identifier on publisher restart, 1-128 ASCII letters/digits/`-`/`_` |
| `sample_sequence` | Positive integer equal to state, at most 9007199254740991 |
| `meter_timestamp` | Timezone-aware UTC RFC 3339 meter message time |
| `received_at` | Timezone-aware UTC RFC 3339 acquisition callback arrival time |
| `import_w`, `export_w` | Same-telegram finite nonnegative powers, already **W**, at most 1000000 W |

The publisher reports `unavailable` on missing/invalid fields or acquisition
silence and omits powers rather than substituting zero. The client also rejects
an explicit false availability attribute if provided. It independently validates
schema, both powers, state/sequence agreement, UTC times, age **less than 30 s**
and future skew at most **5 s**. Future-tolerated samples are held unavailable
until both clocks are no longer in the future. An advancing sequence must also
advance meter time and callback time; arrival alone cannot conceal replay.

Atomic freshness uses the older of meter/callback time and a monotonic client
silence clock. `decision.staleAfterSeconds` may impose a stricter cutoff but cannot
weaken the publisher's 30-second bound. Every accepted new live sequence advances
the decision sample even when import is zero and export is constant. This permits
the unchanged surplus to earn the unchanged **300-second** sustained-GO requirement
without lengthening freshness or shortening stability.

Snapshots establish a replay watermark but never enable GO or renew sample age.
Reconnect requires a newer live sample and resets GO stability. A publisher session
change also resets GO; its sequence may restart, but its meter/callback times must
still advance. Replayed/nonadvancing sequences and retired sessions fail closed.
Retired-session tracking is bounded to 64 lifecycle changes in one client runtime;
overflow stays unavailable until a deliberate client restart. Diagnostics distinguish
atomic telegram evidence from provisional HA value-change evidence and expose the
sequence, meter/callback times and sample age separately from HA metadata.

The publisher has been deployed separately, and user-run provider monitoring
passed the 300-second freshness/unchanged-sequence gates. Measurement correctness
remains the user working assumption, not verified physical calibration. Do not turn an advancing
built-in DSMR timestamp into a freshness signal: it can precede power updates or
come from a telegram missing required powers. Timestamp-only signals, arbitrary
debounce and heartbeat/report times do not establish an atomic sample.

#### Recorded HA night baseline

In explicit HA mode, the native tray restores its private last-valid baseline on
startup and catches up from recorder history when a newer completed night exists.
While running it checks a daily **Europe/Brussels** refresh time (default **06:00**):

```json
{
  "homeAssistant": {
    "nightBaseline": { "enabled": true, "refreshAt": "06:00" }
  }
}
```

`refreshAt` is a validated 24-hour `HH:MM`. A missed refresh is caught up at the next
launch; no OS task or HA automation is created. On a spring clock jump a skipped
refresh time runs at the first valid later tick; the repeated autumn hour cannot
run the same daily refresh twice.

The window is the most recent **completed sunset-to-sunrise night**, calculated
from the configured location, never the PC timezone. Both rounded solar boundary
minutes are excluded to avoid including daytime. In winter, 06:00 can precede
sunrise: that refresh uses the preceding completed night rather than claiming the
current partial night is complete. Choose a later refresh time if the latest
night should be available on the same morning. Missing location or polar nights
without supported crossings produce an explicit unavailable status.

History is fetched using a **separate authenticated WebSocket** connection and
HA's [`history/history_during_period` command](https://github.com/home-assistant/core/blob/dev/homeassistant/components/history/websocket_api.py).
The preferred source is the configured atomic grid entity, with full attributes,
non-minimal responses and attribute changes included. The recorder must retain
the schema-v1 sequence/session, both powers and acquisition timestamps. Merely
having 10 days of recorder retention does not prove that these atomic attributes
were recorded. No REST,
database reads, direct hardware or Solar Agent fallback is used.

The statistic preserves the existing night learner: **median of the first real
grid-import reading in each UTC minute**, in W, with at least 30 minute samples.
Nighttime grid import remains the existing household-baseline proxy; this does
not validate AC sign/topology or equate it with measured household consumption.
Atomic sequence and both timestamps must advance, remain internally valid, and
cover the completed window without 30-second-or-longer acquisition gaps.
Unknown/unavailable, missing attributes, replay, incomplete coverage and errors
reject the candidate, rather than filling gaps or treating them as zero.
Five-minute/hourly statistics or means are **not** substitutes for this median.

When atomic acquisition attributes or coverage are unavailable, a separate
query can use the configured **gridImport** power entity (`kW`, device class
`power`, state class `measurement`). This is explicitly a **reported-history
estimate**, not the atomic/acquired baseline. HA's held start-state is requested
at each chunk boundary; each UTC minute samples the recorded state held at its
start. Thus a constant recorded value contributes one value per minute, without
requiring a numerical change or pretending the held state was newly acquired.
The median still uses minute values, never hourly/five-minute means. Duration
influences the number of repeated minute samples, not an event-count median.

The reported estimate requires **100% recorded availability** over the same
completed sunset-to-sunrise observation window and at least 30 minute samples.
Every chunk needs a valid held boundary and ordered events (fractional
milliseconds accepted). Missing history, any explicit `unknown`/`unavailable`
interval, malformed/negative/out-of-range power, wrong metadata, inconsistent
chunk boundaries or incomplete coverage rejects it; outages are not bridged or
filled with zero. Silence while a valid state is held is allowed only as HA's
reported-state semantics: unrecorded physical outages/cache cannot be detected,
so no physical freshness/continuity claim is made. Authentication, transport and
protocol failures do not trigger this alternative source. Actual overnight
legacy power availability remains a live acceptance requirement.

Requests use 15-minute chunks, the existing 1 MiB message bound, at most 32 MiB
aggregate response data, a 20-second response timeout and a 180-second overall
retrieval deadline per source attempt. Failure affects only the baseline, never live grid/GO.
Historical data is never submitted as a live measurement.

The cache lives beside the selected tray config, with its extension replaced by
`.night-baseline.json` (including isolated HA configs). It contains only the
baseline, W unit, observation window, calculation time, sample count, provenance
and source/config identity, never the token or raw history. Atomic and reported
provenance are distinct; the source identity includes both selected entities
and the statistic policy (older-policy caches are invalidated once). Source/entity,
location or refresh-time changes invalidate a mismatching cache. A failed
refresh preserves a prior matching result and displays the error; the menu shows
the recorded night, age and sample count, with results older than 36 hours
explicitly marked stale/retained. Without a valid result, 200 W remains visibly
**default**, not a learned value. `enabled: false` retains the previous volatile
live-night learner. Library callers must supply a private cache path with
`SolarService::start_with_baseline_store` to enable recorded-history retrieval.

#### Atomic HA demand, peak and daily-energy features

The additive contract is documented in solar-home's
[entity mapping](https://github.com/sujithq/solar-home/blob/bc779f2e18b3c6018840f3804c90fab0fcd67d3b/docs/entity-mapping.md).
The configured `atomicGrid` entity can include an additive schema-v1 extension:
`demand_w`, `monthly_peak_w`, `monthly_peak_at`, `monthly_peak_month`,
`import_tariff_1_wh`, `import_tariff_2_wh`, `export_tariff_1_wh` and
`export_tariff_2_wh`. Demand/peak are nonnegative **integer W**, bounded at
1,000,000; counters are **integer Wh**, bounded at 999,999,999. No HA display-unit
conversion is applied to these unit-suffixed attributes. `monthly_peak_at` is
the UTC peak occurrence time, no later than the sample meter timestamp.
`monthly_peak_month` must exactly match that occurrence's Brussels-local
`YYYY-MM`; a previous-month peak is never treated as the current month's peak.

The extension's `optional_status` object declares six groups: `demand`,
`monthly_peak`, `import_tariff_1`, `import_tariff_2`, `export_tariff_1` and
`export_tariff_2`. Each is `available`, `missing` or `invalid`. Available requires
all values in the group; missing/invalid requires explicit null values (the
peak's W/time/month tuple is all-or-none). An absent extension on an older
schema-v1 grid publisher explicitly leaves these features unavailable without
affecting grid/GO. Malformed optional groups disable only their feature.

The clock comes only from `atomicGrid.meter_timestamp` in UTC. The client
converts it to **Europe/Brussels** meter-local date, month, minute and CET/CEST
offset using the EU DST transition rules; it never uses the PC's timezone or
the separate HA timestamp entity. Quarter calculations use the same meter
instant.

Only the current accepted atomic telegram supplies optional data. Advancing
session/sequence/meter/callback provenance supports **unchanged** demand, static
monthly peaks and idle tariff counters; there is no numerical-change requirement.
Optional data shares the atomic grid's strictly-under-30-second age, replay and
reconnect gates. A snapshot only establishes a watermark, and a new live atomic
sample is required after reconnect. Missing fields are never carried forward,
and separate HA entities, heartbeats or cached states cannot fill them.

The earlier six separate mappings (`currentDemand`, `monthlyPeak`,
`energyImportTariff1/2`, `energyExportTariff1/2`) and
`meterDataStaleSeconds`/`meterDataMaxEventSkewSeconds` remain accepted and
round-tripped for configuration compatibility but are not subscribed to for
live atomic features. The four `energy*Tariff*` mappings select the separate
reported-energy history source below; `currentDemand`, `monthlyPeak` and the
old event-age/skew settings do not supply atomic optional data.
No new mappings or private-config changes are needed.

The quarter-hour engine uses the atomic grid import, validated current-demand
and current-month-peak states, and the meter clock. **Today** uses differences
of the four cumulative counters, with recorded baseline recovery described
below. A decreasing/reset counter invalidates the live result until the next
Brussels meter day. No inverter yield or instantaneous-power estimate
substitutes for tariff counters.

This is client support, not evidence that the publisher extension is deployed.
Live acceptance still requires observing extended telegrams, unchanged optional
values across advancing sequences, unavailable/recovery behavior, month identity
and partial counter deltas. Physical measurement correctness remains unverified.

#### HA reported daily energy from original tariff history

The primary **Today** display can recover the older original HA tariff recorder
history, independently of when the additive atomic publisher was installed.
It queries the configured four `energy*Tariff*` entities via authenticated
`history/history_during_period` WebSockets with `include_start_time_state:true`,
`significant_changes_only:false`, `minimal_response:false`, and full attributes.
HA's recorder supplies the last known state at/before the requested boundary,
projected to that boundary. An event exactly at midnight is **not** required for
this reported-state calculation. See the upstream
[history WebSocket API](https://github.com/home-assistant/core/blob/dev/homeassistant/components/history/websocket_api.py)
and [recorder boundary-state implementation](https://github.com/home-assistant/core/blob/dev/homeassistant/components/recorder/history/__init__.py).

This view is labeled **HA recorded reported energy; held midnight boundary**,
not exact atomic midnight acquisition. It subtracts the held Brussels-midnight
value from the last recorded value before a fixed query cutoff, separately for
each `kWh`/`energy`/`total_increasing` entity (whole-Wh precision), then adds
**import tariff 1 + import tariff 2** for imported energy and **export tariff 1 +
export tariff 2** for exported energy. Inactive day/night tariffs with no changes
are valid zero deltas. Chart-negative production does not establish a negative
raw counter convention: negative counters and decreases/resets are rejected,
never absolute-valued. Adding import and export magnitudes would be grid
throughput, not net import or household consumption.

The query starts at Brussels local midnight and ends ten seconds before its
request time, including recorded changes in the current incomplete hour; it
does not round to hours, mix statistics with raw history, or add overlapping
atomic/live deltas. The display shows the exact period/as-of time, whole-Wh
precision, provenance and refresh age. Recorded held values are not proof of
physical freshness, same-telegram coherence or uninterrupted acquisition.
Wrong units/classes, malformed numeric values, missing boundary/entity rows,
unordered events, inconsistent chunk boundaries or a counter decrease prevent
publishing a new reported total. Midnight and final held states must be valid.
Intermediate `unknown`/`unavailable` states do not erase cumulative endpoint
deltas when subsequent valid totals remain nondecreasing. The menu counts those
rows (including held repeats) as **Recorded gaps**, explicitly warning that reset
continuity across gaps is unverified. No missing/reset energy is inferred; a
reset that recovers above its earlier value inside an unobserved gap cannot be
detected from these endpoints, so this is not a reset-corrected statistics total.
Recorder event timestamps may have sub-millisecond precision; ordering retains
that precision even though displayed timestamps use milliseconds.
Missing recorder events/physical outages that
HA never recorded cannot be detected from held history; this is explicitly a
reported-state view, not a reconstruction of missing physical energy.

Queries refresh no more often than once per minute (plus query duration) on the
independent history worker, bounded to one 23/24/25-hour local day, five-minute
chunks, 1 MiB messages, 128 MiB aggregate text, 100,000 rows, 20-second command
and 180-second overall deadlines. No REST, database, hardware read or HA write
is used. A same-day prior result is retained with explicit refresh error/as-of
age (stale after 120 seconds); old-day results are discarded. Restart requeries
the original recorder rather than depending on new atomic history. This does
not modify the private atomic/night caches, settings, or night-baseline logic.
If reported history is unavailable, the separately labeled atomic partial/exact
view below remains available; errors do not change grid/GO/household/forecast.
While that fallback is displayed, **HA reported energy** also shows pending
transport/worker initialization, loading (up to the 180-second query deadline),
or the last query error. Small partial-run totals are not a completed reported
full-day calculation. A refresh in progress is also labeled on a retained result.
The first reported query polls initialized HA transport every 200 ms and runs
before atomic baseline recovery, rather than waiting for its 30-second loop or
a potentially long atomic-history bridge. This does not bypass authentication
or count transport readiness as physical measurement freshness.

**History query** shows the current phase, validated chunks/total, and elapsed
query time. With a private baseline store, a sibling
`.night-baseline.daily-energy.reported-history-status.json` file (relative to
the tray config's stem) preserves sanitized run/query/update timestamps,
elapsed milliseconds, chunk counts, fixed client error codes and the last
terminal result even while a later refresh runs. It contains no endpoint,
entity IDs, credentials, counters, readings or location. Status writes do not
change the private energy/night caches; a write failure is shown explicitly.
After a forced process exit an in-progress phase means only that no terminal
outcome was persisted, not proof of a timeout. The query's 180-second deadline
is unchanged; a launcher with a six-minute lifetime must capture this report
instead of inferring success/failure from a screenshot or restarting repeatedly.
History existence in an HA graph does not itself verify these four entities'
coverage or reset convention, and this implementation makes no live access claim.

#### Atomic Today baseline and exact coverage

On startup, day rollover and recovery, the client tries to recover recorded
atomic counters through an independent authenticated **WebSocket-only**
`history/history_during_period` reader. This runs once fresh live counters
provide an anchor, checked every 30 seconds; unsuccessful attempts retry no more
than once per 15 minutes while valid live counters are available. No REST,
recorder statistics, direct database/device read or HA write is used.
The reader allows a cancellable ten-second recorder-commit grace period after
capturing the anchor while buffering live updates; it still requires that exact
anchor in the returned history. The grace period itself proves nothing.

The narrow full-day policy requires a real atomic counter sample with
`meter_timestamp` **exactly at Europe/Brussels local midnight**, followed by a
continuous recorded/live counter chain through the captured live sample. All
four counters must be valid in each sample, nondecreasing, in one publisher
session with consecutive sequences, advancing meter/callback times and gaps
strictly below 30 seconds. A later/nearest sample is **not** called midnight;
counters are not interpolated across gaps. The actual midnight and next
midnight are computed in Brussels time, so DST days span 23 or 25 hours rather
than a fixed 24 hours.

When that evidence exists, coverage says **Today from validated Brussels
midnight**. Otherwise it remains **Partial today; exact observed baseline**,
with the actual baseline and last-covered meter timestamps (date, seconds,
milliseconds, Brussels timezone and UTC offset), plus the recorded-history
reason. This can stay partial indefinitely when no telegram lands exactly at
midnight, history is absent, recorder rows lack full attributes, sequences/gaps
are missing or a publisher session changes. In particular, grid-only history
from before the optional-field extension cannot reconstruct this day's energy.
Relaunching does not manufacture missing midnight/counter evidence.

The native app privately saves source/day-bound counter baselines and their
last validated point, units and provenance next to its night-baseline cache,
with suffix `.daily-energy.json`. It retains no raw history or token in that
cache. Restart/reconnect restores earlier totals only after recorded continuity
bridges the saved point to a matching live anchor; saved values alone cannot
make the current result available. Source/config identity changes, day rollover
and counter resets cannot produce cross-source/day/reset subtraction. The
partial baseline may also survive a restart when its bridge is valid. Without
a configured private store (library use), recovery works but is not persistent.

Daily queries stream five-minute chunks, with the existing 1 MiB message limit,
20-second command and 180-second overall deadlines, at most 128 MiB of response
text and 100,000 samples for at most one 25-hour day. Oversized/late/malformed
responses fail explicitly rather than weakening coverage. Results must match
the requested day and exact live acquisition anchor; bounded concurrent live
updates are joined before applying a result. Historical readings never advance
live grid freshness, GO, household use or forecast inputs. History errors leave
valid independent live calculations and an honestly labeled partial day intact.
Night-baseline scheduling/statistics remain unchanged.

#### Conservative separate-entity mode

For existing HA configurations, `atomicGrid` defaults to `""`, preserving the
previous conservative pair mode. This is an explicit configuration choice, not
an automatic recovery path from an unavailable atomic entity.
In this mode import/export are independently nonnegative **kW** values, multiplied by 1000
to watts; net import is import minus export. Both members must be valid and
independently younger than `decision.staleAfterSeconds` (default **30 s**), with
sample skew at most `gridSkewSeconds` (default **5 s**, configurable 0-30 s).
A coherent pair advances the decision sample only when a member has a newer
accepted value change, never from unrelated entities or display refreshes.
In either grid mode, optional `activePower` and `dcPower` may map independently to
`sensor.sg5_0rs_local_total_active_power` (AC) and `sensor.sg5_0rs_local_total_dc_power`
(DC), both in **W**. The Power now menu shows **Inverter output (AC)** and
**Solar panels (DC)** as distinct values. Either may be left empty to disable that
display. Each reading expires independently after `inverterStaleSeconds` (default
**90 s**); missing, stale, unavailable, malformed, non-finite, out-of-range or
wrong-unit values affect only that optional display, never atomic grid freshness or
GO. Bounded signed AC is displayed with its sign unchanged; it is not absolutized
or used for household consumption unless the separate gated calculation below is
explicitly enabled.

HA exposes no acquisition timestamp/sequence for these optional inverter entities
in this slice. Their age is therefore only **last-known HA state-change age**,
not proof that the inverter was polled or that an unchanged zero was freshly
acquired. The client preserves that original age across equal-value/attribute-only
updates and reconnect snapshots, and does not use `last_reported`, connectivity,
or heartbeat activity to renew it. The Connections menu labels this limitation
explicitly. Physical AC sign, DC interpretation and topology remain unvalidated;
household derivation is disabled by default.

### Optional gated household-use estimate

The candidate formula is **`import_W - export_W + AC_W * generationSign`**.
`generationSign` must be explicitly set to **1** or **-1** so the inverter's
generation contribution is positive; AC is not absolutized and DC is never a
substitute. The only currently supported topology identifier is
`single_grid_connected_inverter`. To enable the estimate, configure
`householdUse` with `enabled: true`, that topology identifier, and the validated
sign, while keeping both `atomicGrid` and `activePower` mapped. The default is
disabled with empty topology and sign 0. Do not opt in until the owner confirms
the supported physical topology and daytime observations validate the AC sign.
This explicit configuration is an owner assertion, not automatic physical
validation.

After those acceptance steps, the opt-in shape is:

```json
{
  "householdUse": {
    "enabled": true,
    "topology": "single_grid_connected_inverter",
    "generationSign": -1,
    "maxEventSkewSeconds": 5
  }
}
```

The sign shown is illustrative only; choose `1` or `-1` from validated daytime
observations. Configuration validation rejects unsupported topology values,
missing atomic-grid/AC mappings, missing sign, and an event-skew limit outside
0-30 seconds.

The calculation also requires a fresh atomic grid sample (the existing maximum
30-second grid age), a live AC value-change sample younger than the independent
`inverterStaleSeconds` limit (default **90 s**), and
`abs(atomic received_at - AC last_changed)` no greater than
`householdUse.maxEventSkewSeconds` (default **5 s**, configurable 0-30 s).
This last comparison is only provisional HA-event proximity: `received_at` and
`last_changed` are HA-side event times, not proof of simultaneous physical
measurements. It must not be compared to the DSMR `meter_timestamp` or described
as physical sample skew. AC `last_changed` is not an acquisition timestamp and
unchanged values expire even if the inverter may still be polling. Snapshots,
`last_updated`, `last_reported`, connectivity, heartbeats and cached data cannot
renew the AC age or satisfy the post-reconnect live-change gate.

Missing, invalid, stale, skewed or out-of-range AC suppresses only this optional
calculation; valid atomic grid decisions continue independently. Household use
is unavailable when disabled or when any gate fails. This feature does not
establish physical sign/topology correctness, daytime behavior or acquisition
freshness.

For the separate-entity mode, diagnostics label HA value-change times as provisional evidence, preserve HA
`last_changed`/`last_updated`/`last_reported` separately, and track receive age
with a monotonic clock. The snapshot alone cannot enable recommendations:
each required entity must supply new live value-change evidence after connection.
Equal numeric states, attribute-only updates, cached reconnect snapshots and
heartbeats retain the original evidence time and eventually expire. A steady
zero is not acquisition evidence. This is deliberately fail-safe: a sustained
zero import/export may prevent GO, and the default 300-second stability interval
cannot be earned from unchanged entities under the provisional 30-second limit.
Do not increase stale limits or shorten stability merely to hide this limitation;
use the approved atomic acquisition interface for unchanged-reading evidence
instead of using standalone meter clocks or unrelated report times.

Missing/removed entities, `unknown`, `unavailable`, malformed values/timestamps,
wrong units, negative grid power, future/out-of-order samples, stale data and
incoherent pairs clear recommendations. Transport/authentication failures clear
them immediately; recovery must earn a new stability interval. The existing
offline/recovery notification preferences remain available.

AC and DC are displayed as distinct optional readings, but their meaning and
physical freshness are not verified. This slice does **not** map Sungrow yield
counters or treat connectivity as inverter-data freshness. Household use remains
off unless its explicit topology/sign and freshness/event-proximity gates are
configured; enabling them does not establish physical validation. Daily energy
and quarter-hour forecast require the additive atomic fields above; old grid-only
publishers leave them unavailable. No instantaneous-power or
inverter-yield approximation substitutes for them.

Provider grid-freshness monitoring and a user-observed native grid/error/recovery
smoke have passed. Optional AC/DC display with these mappings still needs live
observation. Daytime PV/sign/topology, inverter acquisition freshness and physical
measurement calibration remain pending; a changing or zero display is not proof
of a successful acquisition. These checks do not authorize deployment/cutover.

### Opt-in monitoring acceptance harness

`cargo build -p solar-core --example ha_monitor` builds a bounded, monitoring-only
provider harness. It requires an existing token in the launched process's
`SOLAR_HA_TOKEN` and `SOLAR_HA_MONITOR_APPROVED=1`; never put a token in a command,
repository file or chat. A private local launcher can prompt with hidden input,
pass the credential process-locally and clear it afterward. This is not persistent
credential storage or a token-creation flow.

The harness uses only the loopback HA WebSocket and explicit atomic entity,
with direct readers disabled. It observes 310 seconds, stops only its own client
for a 31-second stale check, then starts a new client for 15 seconds to check
recovery/stability reset. It never changes HA, appliances or meter acquisition.
Output is a sanitized JSON report of durations/counts and safety outcomes, not
power readings, absolute timestamps, sessions, tokens or device identifiers.
For each configured AC/DC sensor, it separately counts fresh last-state-change,
stale, unavailable and invalid polls, plus maximum last-known state-change age;
these observations are informational and cannot affect atomic-grid acceptance.
It includes reconnect counts, allowlisted client-code/predicate polling histograms,
longest observed continuous fresh/candidate intervals and maximum engine stability.
`protocol_errors` is retained for old report readers but means aggregate grid/client
transport, protocol and grid-validation errors; `client_errors` names it accurately.
Optional inverter validation failures stay in their own per-entity diagnostics and
do not fail or refresh the atomic grid.
Error deltas are attributed to the latest polled code, not an exact event log:
intermediate errors can be coalesced or superseded. Continuity is sampled every
200 ms and resets on gaps over 500 ms; it cannot prove unsampled transitions.
Acceptance requires at least 300 seconds of observed continuous freshness, not
merely 300 seconds between the first and last accepted samples.
Exit zero means these bounded acceptance checks passed; nonzero means incomplete
or failed acceptance. GO is not forced or required when existing predicates
(for example grid import) prevent it. Synthetic tests are not live acceptance.

### Legacy source selection

A SlimmeLezer does not reliably serve two clients: with two connections open, one of them
stops receiving telegrams. So the app prefers Solar Agent's status instead of keeping a
second meter connection open.

- **Solar Agent is running in solar mode**: the app reads Solar Agent's status from its local
  service every 2 seconds and opens no meter or inverter connection of its own. It finds the
  service through `%LOCALAPPDATA%\Solar Agent Data\settings.json`, which holds the port and
  the access token; set `SOLAR_AGENT_SETTINGS_INFO` to use another file. **Connections**
  shows `Read by Solar Agent`. Picking an appliance, a simulation scenario or a grid power
  changes it in Solar Agent, as if you had picked it there.
- **Otherwise**: the app connects to the SlimmeLezer and the inverter itself, using the `solar`
  settings in its own config file. **Connections** shows `Read by Solar Systray`.

When a poll detects Solar Agent, the app switches to its data and stops its own device
connections. A brief overlap is possible while a newly started agent is being detected;
this polling handoff is not a device lock.

The app switches back to its own connections after Solar Agent's settings info file has
been missing, its local port has been unreachable, or its mode has been `agents` (not solar)
for 10 seconds; until then the icon is grey. At startup, these conditions allow direct mode
immediately.

If Solar Agent accepts a connection but times out, rejects the token, or sends incomplete
or unreadable data, the icon turns grey and the menu shows `NO DATA FROM SOLAR AGENT` with
the reason. The app stays off the meter, because Solar Agent may still hold it. An unreadable
or invalid settings info file also keeps the app off the meter until corrected. Missing
solar readings alone do not trigger a competing connection.

The app only talks to Solar Agent on `127.0.0.1`, checks the size and format of every answer,
and never shows or stores the access token.

## Install and run

1. Download `solar-systray-X.Y.Z-windows-x64.zip` from the GitHub release page, or build it
   (see below). Copy `solar-systray.exe` to a permanent folder, for example
   `%LOCALAPPDATA%\Programs\Solar Systray\`. The executable is not code-signed, so Windows
   SmartScreen may warn the first time it runs.
2. Run `solar-systray.exe`. On Windows 11 the icon may start in the hidden-icons flyout (`^`);
   drag it onto the taskbar to keep it visible.
3. Optional: tick **Start at sign-in**. This adds a `Solar Systray` value under
   `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` pointing at the current executable,
   so move the executable first.

Only one copy runs per signed-in session.

## Configuration

Settings live in `%LOCALAPPDATA%\Solar Systray\config.json`. On first run the app creates it:

- If Solar Agent's companion settings exist (`%LOCALAPPDATA%\Solar Agent Data\companion.json`),
  their meter, inverter, threshold, location and appliance settings are copied.
- Otherwise it starts in **simulation mode** with default settings, so you can try it out.

For an isolated HA monitoring instance, set the process-only
`SOLAR_SYSTRAY_HA_CONFIG` environment variable to an **existing absolute** config
file path before launching. This override requires explicit `home-assistant` mode
and a nonempty `atomicGrid`; it never seeds from Solar Agent or defaults, and
rejects non-HA settings on reload. Tray preference edits affect only this file.
Autostart is unavailable in this instance. The normal single-instance rule still
applies: exit another tray instance yourself first. Keep the existing HA token in
the configured process environment reference, never in the JSON. This is a config
override, not proof of native UI or live surplus acceptance.

Edit the file with **Open config file**, then choose **Reload config**. If the file is
invalid, the icon turns grey with a warning dot and the menu shows the error.

```jsonc
{
  "solar": {
    // Use "slimmelezer" for a real meter (set host to its address), "mock" for simulation.
    "energySource": { "type": "slimmelezer", "host": "192.0.2.10", "port": 23, "allowLegacyWithoutCrc": false },
    // Optional Sungrow inverter over Modbus TCP; leave host empty to disable.
    "inverterSource": { "host": "", "port": 502, "unitId": 1 },
    "decision": { "safetyMarginW": 300, "stableDurationSeconds": 300, "importPeakW": 2500, "defaultAppliance": "dishwasher" },
    "location": { "latitude": 50.85, "longitude": 4.35 },
    "appliances": { "dishwasher": { "name": "Dishwasher", "peakW": 2000 } }
  },
  "tray": {
    "iconStyle": "level",          // or "power"
    "appliance": null,             // null uses decision.defaultAppliance
    "notifications": {
      "enabled": true,
      "goGained": true,
      "goLost": true,
      "peakForecast": true,
      "monthlyPeak": true,
      "meterOffline": true
    },
    "offlineNotifyAfterSeconds": 120
  }
}
```

The `solar` section uses the same format and validation as Solar Agent; the first run writes
it out in full with every setting. The `tray` section is written by the menu, so you rarely
need to edit it.

While the app shows Solar Agent's data, only the `tray` section applies, except
`tray.appliance`: the appliance is Solar Agent's.

When it reads the meter itself, the app is a read-only client:

- **SlimmeLezer / esp-link**: a TCP connection to the meter's telnet port (DSMR telegrams,
  CRC-checked). Do not also connect Solar Agent or Home Assistant to the same SlimmeLezer
  port; see [Where the data comes from](#where-the-data-comes-from).
- **Sungrow**: Modbus TCP reads for PV power, inverter output and household use.

## Build

Requires Windows, the Rust MSVC toolchain (1.88 or newer) and the Visual Studio Build Tools.

```powershell
cargo build --release
# target\release\solar-systray.exe
```

The build script embeds the executable icon and version information in plain Rust, so no
resource compiler is needed.

Run the checks that CI runs:

```powershell
cargo clippy --workspace --all-targets -- -D warnings -A clippy::approx_constant
cargo test --workspace
```

For an end-to-end check on an interactive Windows desktop with Explorer running, quit any
existing Solar Systray instance and run:

```powershell
cargo test --release -p solar-tray --test tray_smoke -- --ignored --nocapture --test-threads=1
```

This Rust test launches the actual executable with isolated temporary settings and loopback
Solar Agent, DSMR and Sungrow servers. It checks the registered shell icon, native menu values,
no direct device connections in agent mode (including an authentication failure), the fallback
grace period, direct readings, connection release when the agent returns, and clean exit.
It briefly opens menus, never uses your live devices or settings, and cleans up its own process
and temporary files. It is opt-in because it requires a desktop.

## Releases

Releases are made by GitHub Actions in two steps: **Prepare** builds everything and stores it,
then **Deploy** publishes it once someone approves. Add every user-visible change under
`## [Unreleased]` in `CHANGELOG.md`; those lines become the release notes.

1. Open **Actions → Release → Run workflow** on `main` and enter `patch`, `minor`, `major` or
   an exact version. The workspace already says 0.1.0, so enter `0.1.0` for the first release.
2. **Prepare** (Windows) bumps the version in `Cargo.toml` and `Cargo.lock`, and moves the
   Unreleased notes under a dated version heading. It then runs clippy and the tests, builds
   `solar-systray.exe`, and checks that the executable reports the new version. It packages
   `solar-systray-X.Y.Z-windows-x64.zip` (executable, README, LICENSE, CHANGELOG) and a
   `.sha256` file, then creates the `Release vX.Y.Z` commit and the `vX.Y.Z` tag without pushing
   them. Everything is kept as the run's `release` artifact. The run summary shows the release
   notes, the checksum and the full diff of the release commit.
3. **Deploy** waits for approval of the `release` environment. Once approved, it pushes the
   release commit and tag to `main` in one atomic, fast-forward-only push, then creates the
   GitHub release with the zip and its checksum. If `main` changed after Prepare, nothing is
   pushed or published; run Release again.

Prepare refuses a version lower than the current one, a version that is already tagged or
already has a changelog section, and an empty Unreleased section.

To see locally what Prepare would change, without committing or tagging:

```powershell
cargo xtask prepare patch    # or minor, major, 0.2.0
git diff
git restore CHANGELOG.md Cargo.toml Cargo.lock
```

One-time GitHub setup:

- Under **Settings → Environments**, create an environment named `release`. Add the people
  allowed to publish as **required reviewers**, and optionally allow deployments from `main`
  only. Deploy stops before pushing if this environment has no required reviewers, because
  GitHub would otherwise create the environment unprotected and skip the approval. Private
  repositories get required reviewers only on GitHub Enterprise; public repositories have
  them on every plan.
- The push to `main` uses the workflow's `GITHUB_TOKEN`. If `main` requires pull requests or
  status checks, the push is rejected and nothing is published. Releasing then needs a push
  identity that is allowed to bypass that rule.
- Pushes made by the workflow do not start the CI workflow. Prepare has already run the same
  checks on the release commit's contents.

## Layout

- `crates/solar-core`: a Rust port of Solar Agent's logic (DSMR parsing, surplus decision,
  quarter-hour peak forecast, daily energy, night baseline, sun times, simulation, SlimmeLezer
  and Sungrow clients), plus a client for Solar Agent's local status service. Shared JSON
  fixtures in `crates/solar-core/tests/fixtures` pin its behaviour to the original.
- `crates/solar-tray`: the Windows app (`solar-systray.exe`) built on the raw Win32 API
  through the [`windows`](https://crates.io/crates/windows) crate: tray icon, menu,
  notifications, settings and sign-in startup.
- `xtask`: release tooling run as `cargo xtask`. It bumps the version, dates the changelog and
  extracts release notes. It is not part of `cargo build`.
- `.github/workflows`: CI (`ci.yml`), plus Release (`release.yml`), which calls Deploy (`deploy.yml`).

## Credits and license

The logic in `solar-core` is ported from [madebyq/solar-agent](https://github.com/madebyq/solar-agent).
Released under the [MIT License](LICENSE), keeping Solar Agent's copyright notices.
