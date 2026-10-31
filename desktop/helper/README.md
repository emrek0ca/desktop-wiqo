# Helper for driving other apps (agent-desktop)

`computerDesktop` (crates/wiqo-ops/src/desktop_control.rs) reads other apps' controls through the operating system's accessibility tree with the
Apache-2.0 `agent-desktop` program, and acts on them by ref. Wiqo looks for it in `WIQO_AGENT_DESKTOP`, then next to its own executable. Without it the
operation answers `DESKTOP_ADAPTER_UNAVAILABLE`; nothing else changes.

- Build: `scripts/build-helper.sh <dir>` (pinned commit + `agent-desktop-macos-pid.patch`).
- Ship: put the result next to the Wiqo executable in the release bundle (not wired into tauri.conf yet).
- Permissions: Accessibility for Wiqo (the helper runs as Wiqo's child, so the grant is Wiqo's).
- Verify on a real machine: `WIQO_AGENT_DESKTOP=<path> WIQO_LIVE_APP=<app name> cargo test -p wiqo-ops live_observation -- --ignored --nocapture`.
- Not covered: Windows and Linux were not run; `moveCursor`/`clickScreen`-style raw coordinates are not offered (refs only).
