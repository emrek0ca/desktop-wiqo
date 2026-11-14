# Helper for driving other apps (agent-desktop)

`computerDesktop` (crates/wiqo-ops/src/desktop_control.rs) reads other apps' controls through the operating system's accessibility tree with the
Apache-2.0 `agent-desktop` program, and acts on them by ref. Wiqo looks for it in `WIQO_AGENT_DESKTOP`, then next to its own executable. Without it the
operation answers `DESKTOP_ADAPTER_UNAVAILABLE`; nothing else changes.

- Build: `scripts/build-helper.sh <dir>` (pinned commit + `agent-desktop-macos-pid.patch`).
- Ship: macOS puts it in `Contents/MacOS` (`tauri.macos.conf.json`); Windows puts `agent-desktop.exe` next to `Wiqo.exe` (`tauri.windows.conf.json`, the installer job builds it into `app/binaries` first).
- Permissions: Accessibility for Wiqo (the helper runs as Wiqo's child, so the grant is Wiqo's).
- Verify on a real machine: `WIQO_AGENT_DESKTOP=<path> WIQO_LIVE_APP=<app name> cargo test -p wiqo-ops live_observation -- --ignored --nocapture`.
- Windows: the CI job `windows-uia` opens Notepad and reads its controls (reported, not blocking). Not yet run on a real Windows desktop by a person. Linux was not run; `moveCursor`/`clickScreen`-style raw coordinates are not offered (refs only).
