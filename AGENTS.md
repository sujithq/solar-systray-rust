# Repository conventions

## Verify

- `cargo clippy --workspace --all-targets -- -D warnings -A clippy::approx_constant`
- `cargo test --workspace`
- Keep `Cargo.lock` committed; this workspace contains an executable.

## Working in the workspace

- Preserve the behavior of the Solar Agent compatibility port; its shared JSON fixtures pin cross-implementation behavior.
- Changes to behavior need a regression test in `crates/solar-core/tests/`.
- New core modules must be declared in `crates/solar-core/src/lib.rs`.
- Keep protocol input bounded and validate it before exposing readings; see the security skill when changing parsers or network responses.
- No PR review history is available in this checkout, so no review-derived conventions are recorded.

## Maintenance matrix

| Change | Also update |
|---|---|
| Add a `solar-core` source module | `crates/solar-core/src/lib.rs`; add a focused integration test under `crates/solar-core/tests/` |
| Change DSMR parsing or energy calculations | `crates/solar-core/src/dsmr.rs`; relevant tests in `crates/solar-core/tests/energy.rs` or `crates/solar-core/tests/peak_forecast.rs`; shared fixtures under `crates/solar-core/tests/fixtures/` when compatibility vectors change |
| Change inverter, service, or baseline behavior | Relevant source under `crates/solar-core/src/`; `crates/solar-core/tests/inverter_and_baseline.rs` or `crates/solar-core/tests/service.rs` |
| Change presentation or forecasts | `crates/solar-core/tests/present.rs` or `crates/solar-core/tests/peak_forecast.rs`; update shared fixtures if their expected outputs change |
| Change the Solar Agent status client or source switching | `crates/solar-core/src/agent.rs` and `crates/solar-tray/src/app.rs`; `crates/solar-core/tests/agent.rs`; sanitized fixtures `crates/solar-core/tests/fixtures/agent-status*.json` when Solar Agent's status format changes; "Where the data comes from" in `README.md`; `.github/skills/security-review/SKILL.md` |
| Change the tray icon, menu, notifications, or `tray` settings | Unit tests in the matching `crates/solar-tray/src/` module; `README.md` |
| Any user-visible change | A line under `## [Unreleased]` in `CHANGELOG.md`; the Release workflow refuses an empty Unreleased section and publishes it as the release notes |
| Change the release flow, versioning, or packaging | `xtask/src/release.rs` and its tests (or `xtask/src/main.rs`); `.github/workflows/release.yml` and `.github/workflows/deploy.yml`, checked with `actionlint`; "Releases" in `README.md` |

## Done means

- `cargo clippy --workspace --all-targets -- -D warnings -A clippy::approx_constant` and `cargo test --workspace` pass.
- Any behavior change has a regression test that fails without the change.

## Never merges without a human

A person has to have **read this diff** before it lands. Telling an agent "merge it when you're done" is approving a goal, not this change — so it does not count for anything on this list. Everywhere else it counts fine, which is the point of having a list.

- Publishing a release. The Release workflow's Deploy job pushes the release commit and tag to `main` and creates the GitHub release only after a required reviewer approves the `release` environment. The approver must read the release commit diff in the Prepare job summary before approving. Never remove the environment, its required-reviewer check, or the atomic fast-forward push.
- `.github/workflows/ci.yml` runs validation commands only.

## Documentation

[`README.md`](README.md) is the user-facing guide: icon styles, menu, notifications, where the data comes from, the config file, build steps, and releases. Update it when any of these change. [`CHANGELOG.md`](CHANGELOG.md) follows Keep a Changelog; its Unreleased section becomes the next release's notes. No documentation framework is configured.
