# Wiqo Desktop

The Wiqo app for macOS and Windows (Linux builds too): the computer side of Wiqo. Your phone gives a task, and this app carries it out on
the computer. It has its own chat, sign-in and permissions screens. Built with Rust and Tauri 2.

```
desktop/
  app/                 the Tauri app: window, tray, sign-in, chat UI (app/ui), installer hooks (app/windows)
  crates/wiqo-contract the wire contract: operations, error codes, canonical JSON, signatures
  crates/wiqo-core     relay, pairing, signature checks, execution gate, ledger, storage, vault
  crates/wiqo-ops      the operations the computer runs, one module per area:
                         files/    file operations, documents, search, transfers to and from the phone, one-sentence chores, conversion, file plans, undo
                         system/   status and ping, power, volume, clipboard, media, system chores
                         apps/ browser  terminal/  screen  desktop_control  personal (Messages, Mail, Notes, Calendar, Reminders, Shortcuts on macOS)
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
Windows installers as GitHub Actions artifacts. It publishes `desktop-v<version>` from `main` after both installers, the application and the
bundled helper pass timestamped Authenticode verification. The website reads the published installers from GitHub Releases.

For Windows release signing, configure a public-trust [Azure Artifact Signing certificate profile](https://v2.tauri.app/distribute/sign/windows/#azure-artifact-signing).
Set repository variables `WIQO_WINDOWS_SIGNING_ENDPOINT`, `WIQO_WINDOWS_SIGNING_ACCOUNT` and `WIQO_WINDOWS_SIGNING_PROFILE`, and Actions secrets
`AZURE_TENANT_ID`, `AZURE_CLIENT_ID` and `AZURE_CLIENT_SECRET` for the authorized signing identity. Set
`WIQO_WINDOWS_SIGNING_ENABLED=true` when the profile is ready. The existing `TAURI_SIGNING_PRIVATE_KEY` signs the updater files.

For a macOS release, run `desktop/scripts/release-mac.sh` with the Developer ID and Apple notary profile configured. It builds and verifies a
universal notarized DMG and a signed updater archive.

## Microsoft Store (MSIX)

`desktop/scripts/build-msix.ps1 -Target <triple>` packs `Wiqo.exe`, the helper and the browser worker with `desktop/app/windows/msix/AppxManifest.xml`
(identity from Partner Center: `OsmanEmreKoca.Wiqo`); `bundle-msix.ps1` joins x64 and ARM64 into one `.msixbundle`, which CI uploads as the
`wiqo-msixbundle` artifact. Upload that file in Partner Center. The package is unsigned (the Store signs it) and its version is
`<major+1>.<minor>.<patch>.0` (Wiqo 0.1.6 = 1.1.6.0), because the Store rejects a first part of 0. Inside the package the built-in updater is off and
"start with Windows" uses the package's startup task.
