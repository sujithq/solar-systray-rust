---
name: shipping-a-change
description: What to update when changing solar-core behavior, and what done requires. Use before opening a pull request.
---

# Shipping a change

## Add a core module

1. Create the module under `crates/solar-core/src/`.
2. Declare it in `crates/solar-core/src/lib.rs`.
3. Add a focused integration test under `crates/solar-core/tests/`.
4. Run `cargo clippy --workspace --all-targets -- -D warnings -A clippy::approx_constant` and `cargo test --workspace`.

## When you change this, also change that

| Change | Also update |
|---|---|
| Add a `solar-core` source module | `crates/solar-core/src/lib.rs`; add a focused integration test under `crates/solar-core/tests/` |
| Change DSMR parsing or energy calculations | `crates/solar-core/src/dsmr.rs`; relevant tests in `crates/solar-core/tests/energy.rs` or `crates/solar-core/tests/peak_forecast.rs`; shared fixtures under `crates/solar-core/tests/fixtures/` when compatibility vectors change |
| Change inverter, service, or baseline behavior | Relevant source under `crates/solar-core/src/`; `crates/solar-core/tests/inverter_and_baseline.rs` or `crates/solar-core/tests/service.rs` |
| Change presentation or forecasts | `crates/solar-core/tests/present.rs` or `crates/solar-core/tests/peak_forecast.rs`; update shared fixtures if their expected outputs change |
| Change the tray icon, menu, notifications, or `tray` settings | Unit tests in the matching `crates/solar-tray/src/` module; `README.md` |
| Any user-visible change | A line under `## [Unreleased]` in `CHANGELOG.md`; the Release workflow refuses an empty Unreleased section and publishes it as the release notes |
| Change the release flow, versioning, or packaging | `xtask/src/release.rs` and its tests (or `xtask/src/main.rs`); `.github/workflows/release.yml` and `.github/workflows/deploy.yml`, checked with `actionlint`; "Releases" in `README.md` |

## Done

- `cargo clippy --workspace --all-targets -- -D warnings -A clippy::approx_constant` and `cargo test --workspace` pass.
- Any behavior change has a regression test that fails without the change.
