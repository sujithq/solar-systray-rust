# Solar Systray

A minimal Windows notification-area (system tray) app that shows the same solar-surplus
information as [Solar Agent](https://github.com/madebyq/solar-agent): whether it is a good
moment to start a big appliance, what the grid is doing right now, and whether this
quarter-hour is about to set a new monthly peak. Everything is written in Rust.

It runs in the background as one small executable (about 0.6 MB) with no runtime to install.
When Solar Agent is running it shows Solar Agent's data; otherwise it reads the meter itself,
so Solar Agent does not need to be running. See [Where the data comes from](#where-the-data-comes-from).

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

Hovering the icon shows a compact summary: the headline, the grid power (plus PV when an
inverter is configured), the appliance's start threshold, the quarter-hour forecast and today's
energy, for example `GO - START NOW` / `Export 2264 W | PV 3.1 kW`.

### Menu

Left- or right-click the icon:

- **Headline** with the reason, for example `GO IN 4:38` and `Stable for 0:23 of 5:00`.
- **Power now**: grid export and import; solar panels (PV), inverter output and household use
  when a Sungrow inverter is configured; and the night baseline learned from the meter.
- **Appliance**: pick the appliance to plan for; shows the export needed to start it.
- **Quarter-hour peak**: current demand, monthly peak, forecast, and how much import to cut.
- **Today**: energy imported and exported today, by the meter's clock.
- **Connections**: who reads the meter (`Solar Agent` or `Solar Systray`), sunrise and
  sunset, meter and inverter status.
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
