## Description

<!-- What does this change do, and why? -->

## Changes

<!-- List the meaningful implementation changes. -->

## How to test

<!-- Include steps or inputs that verify the behavior. -->

## Checklist

- [ ] `cargo clippy --workspace --all-targets -- -D warnings -A clippy::approx_constant` passes.
- [ ] `cargo test --workspace` passes; behavior changes have a regression test.
- [ ] Shared compatibility fixtures are updated when expected cross-implementation behavior changes.
- [ ] User-visible changes have a line under `## [Unreleased]` in `CHANGELOG.md`.
- [ ] Documentation is updated when applicable.
