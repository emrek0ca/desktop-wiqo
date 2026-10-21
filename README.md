# Wiqo Desktop

The Wiqo app for macOS and Windows (Linux builds too): the computer side of Wiqo. Your phone gives a task, and this app carries it out on
the computer. It has its own chat, sign-in and permissions screens. Built with Rust and Tauri 2.

```
desktop/
  app/                 the Tauri app: window, tray, sign-in, chat UI (app/ui), installer hooks (app/windows)
  crates/wiqo-contract the wire contract: operations, error codes, canonical JSON, signatures
  crates/wiqo-core     relay, pairing, signature checks, execution gate, ledger, storage, vault
  crates/wiqo-ops      the operations the computer runs (files, apps, terminal, screen, documents, ...)
  crates/wiqo-agent    the runtime the window wraps (also a headless `wiqo-agent` command)
contracts/companion/   the shared contract files the phones and the server use (kept in sync with the Wiqo app repo)
```

## Build

```bash
cd desktop && cargo test
cd desktop/app && cargo tauri build --bundles app        # macOS (signed with the Developer ID in tauri.conf.json)
cd desktop/app && cargo tauri build --bundles nsis       # Windows installer, on Windows
```

## Releases

Raise `version` in `desktop/app/tauri.conf.json` (and `desktop/Cargo.toml`) and push. CI (`.github/workflows/desktop.yml`) builds the x64 and ARM64
Windows installers and publishes them as the pre-release `desktop-v<version>`. wiqoai.com reads this repository's releases and offers the newest
Windows installer by itself. The repository secret `TAURI_SIGNING_PRIVATE_KEY` signs the updater files.
