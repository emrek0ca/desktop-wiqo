# Wiqo Desktop (Rust)

One desktop app for macOS, Windows and Linux (it replaced the earlier Swift and C# companions).

Windows: `cargo tauri build --bundles nsis` on Windows (CI: the `windows-package` job in `.github/workflows/desktop.yml`) gives a per-user installer that needs no admin; secrets are in Credential Manager (split into parts, 2560-byte limit per item) and child programs never open a console window (`wiqo_ops::process::command`).

Decisions: UI is **Tauri 2**; phones connect through the relay **and** directly over the LAN (TLS); Linux v1 covers files, apps, system, clipboard, code, documents and screenshots; `contracts/companion/operations.json` is the one hand-managed source of the wire contract.

```
crates/
  wiqo-contract/   the wire contract: operations, error codes, canonical JSON, signatures        (R1)
  wiqo-core/       relay, pairing, signature checks, execution gate, ledger, storage             (R2)
  wiqo-ops/        the operations a desktop runs: 32 of the 36 shared ones (R3)
  wiqo-agent/      the headless process: `pair`, `run`, `status`, `full-access on|off`           (R2)
```
The Tauri app joins in R6.

```bash
cargo build && (cd ../../rova-backend && npm run e2e:desktop)   # the agent against the real server: pairing, relay, signed commands, reconnect
```

```bash
cargo test            # the contract crate against contracts/companion/vectors.json
```
