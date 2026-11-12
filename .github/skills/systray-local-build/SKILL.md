---
name: systray-local-build
description: Preflight Rust/MSVC, run clippy and tests, build Solar Systray locally, or launch/replace the Windows tray in HA mode without publishing a release.
---

# Local build and HA tray handoff

## Preflight

Work in the requested checkout, not another worktree. Read `AGENTS.md`, inspect
`git status --short` and `git branch --show-current`, and preserve unrelated edits.
Check `rustc --version`, `cargo --version` and `rustup show active-toolchain`
against `workspace.package.rust-version` in `Cargo.toml`. Native builds need
Windows, an MSVC Rust toolchain and Visual Studio C++ Build Tools with Windows SDK.
The HA launcher needs PowerShell 7. Missing prerequisites are a blocker, not
permission to install software or silently switch toolchains.

Do not install packages speculatively. If a necessary tool is missing, use only
approved feeds: Python `https://packagefeedproxy.microsoft.io/pypi/simple`, npm
`https://packagefeedproxy.microsoft.io/npm/`. No crates mirror is specified here;
do not invent one or change Cargo sources.

## Validate changes

Run from the repository root and record each exit status:

```powershell
cargo clippy --workspace --all-targets -- -D warnings -A clippy::approx_constant
cargo test --workspace
pwsh -NoProfile -File .\scripts\Test-HaTrayLauncher.ps1
```

The launcher check uses synthetic subprocesses, not the tray, credentials or HA.
Behavior changes require a regression in `crates/solar-core/tests/` that fails
without the change; native tray behavior also needs matching module tests.
Keep `Cargo.lock` and compatibility fixtures intact. Use `shipping-a-change`
for related documentation and registration updates. Report failed or skipped
checks rather than claiming completion.

## Build without publishing

When a local optimized executable is requested:

```powershell
cargo build --release -p solar-tray
```

The result is `target\release\solar-systray.exe`. There is no xtask build command:
`cargo xtask prepare` edits versions, lockfile and changelog, and `cargo xtask notes`
extracts release notes. Neither is needed for a local feature preview. Consult
README's Releases section only for explicitly requested release preparation.
Building never publishes a release; do not dispatch Release/Deploy, tag or push.

## Launch or replace only when requested

1. Confirm the request includes running/replacing the local tray. Replacing a
   running app interrupts monitoring; use its **Quit** menu and wait for exit.
   Never terminate processes by name, replace an installed executable, change
   autostart, or change the user's primary settings as part of this procedure.
2. Use an existing private absolute config path supplied by the user. It must
   select `home-assistant` and a nonempty `atomicGrid`. Never commit or print its
   contents, endpoint, live state or token. Do not invent configuration, provision
   credentials, or change HA. Let the user enter their existing token at the
   launcher's hidden prompt; do not collect it through chat or command arguments.
3. In an interactive PowerShell 7 terminal, run the launcher, substituting only
   the privately supplied config and the actual intended checkout branch:

   ```powershell
   pwsh -NoProfile -File .\scripts\Start-HaTray.ps1 -ConfigPath '<existing absolute private config path>' -ExpectedBranch '<intended checkout branch>'
   ```

   It checks duplicate instances/branch, builds this checkout before prompting,
   prints revision and binary hash, and injects credentials only into its child.
   It refuses build failure rather than launching a stale binary. Do not bypass
   its checks or run it unattended without access to the user's interactive prompt.
4. Verify the new process remains running, then inspect the native tray's
   Connections and requested feature. A PID or successful build is not HA
   connectivity, fresh samples, GO acceptance, or visual evidence. HA mode must
   remain WebSocket-only with no REST, Solar Agent or direct-device fallback.
   Report unobserved evidence honestly; do not label cached data as live.
5. The launcher returns without a timeout; the tray remains until **Quit**.
   Leave it running only when requested. A failed replacement is not success:
   report the failure and any monitoring gap without silently switching sources.
