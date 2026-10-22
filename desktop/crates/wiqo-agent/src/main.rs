//! `wiqo-agent`: the Wiqo desktop without a window.
//!
//!   wiqo-agent pair                   show a pairing code for a phone to scan and wait for the phone
//!   wiqo-agent run                    connect to the server and run the commands that arrive
//!   wiqo-agent status                 what this desktop is, who it is paired with, what it can do
//!   wiqo-agent full-access on|off     let the server's PC agent run commands here without asking the phone each time
//!   wiqo-agent selftest [--app NAME]  run every everyday tool for real on this computer (a scratch folder on the Desktop) and report each one

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use wiqo_agent::{Desktop, Event};

#[derive(Parser)]
#[command(
    name = "wiqo-agent",
    version,
    about = "The Wiqo desktop without a window"
)]
struct Cli {
    /// The Wiqo server (default: https://api.wiqoai.com, or WIQO_SERVER).
    #[arg(long, global = true)]
    server: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Pair,
    Run,
    Status,
    FullAccess {
        state: String,
    },
    /// Runs the everyday tools for real (files, search, apps, clipboard, screen, system) and prints one JSON line per step; exits 1 on a failure.
    Selftest {
        /// An application to open, find among the running ones and quit (default: Notepad on Windows, TextEdit on macOS).
        #[arg(long)]
        app: Option<String>,
        /// Leave out the steps that need a desktop session (screen capture, apps, clipboard), for a machine without one.
        #[arg(long)]
        headless: bool,
        /// Print what each tool returned (its data), shortened.
        #[arg(long)]
        verbose: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
    let desktop = Desktop::open(cli.server.clone())?;
    match &cli.command {
        Command::Status => {
            let info = desktop.snapshot();
            println!("device      {} ({})", info.device_name, info.device_id);
            println!("platform    {} · version {}", info.platform, info.version);
            println!("server      {}", info.server);
            println!("paired      {}", if info.paired { "yes" } else { "no" });
            for phone in &info.phones {
                println!(
                    "phone       {} ({}) {}",
                    phone.name, phone.platform, phone.pairing_id
                );
            }
            println!(
                "full access {}",
                if info.full_access { "on" } else { "off" }
            );
            println!(
                "abilities   {}",
                info.capabilities
                    .iter()
                    .map(|capability| capability.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Command::FullAccess { state } => {
            let on = match state.as_str() {
                "on" => true,
                "off" => false,
                _ => bail!("use `full-access on` or `full-access off`"),
            };
            desktop.set_full_access(on)?;
            println!("full access {}", if on { "on" } else { "off" });
        }
        Command::Pair => {
            let offer = desktop.start_pairing().await?;
            let qr = qrcode::QrCode::new(offer.code.as_bytes())?;
            println!(
                "{}",
                qr.render::<char>()
                    .quiet_zone(true)
                    .module_dimensions(2, 1)
                    .build()
            );
            println!(
                "Scan this code with the Wiqo app on your phone (valid until {}).",
                offer.expires_at
            );
            println!("{}\n", offer.code);
            let phone = desktop.finish_pairing(&offer).await?;
            desktop.disconnect();
            println!(
                "Paired with {} ({}). Start the desktop with `wiqo-agent run`.",
                phone.name, phone.platform
            );
        }
        Command::Selftest {
            app,
            headless,
            verbose,
        } => {
            let failures = selftest(&desktop, app.clone(), *headless, *verbose).await;
            if failures > 0 {
                bail!("{failures} step(s) failed");
            }
            println!("all steps passed");
        }
        Command::Run => {
            if !desktop.is_paired() {
                bail!("this desktop is not paired yet: run `wiqo-agent pair` first");
            }
            let mut events = desktop.events();
            desktop.connect();
            loop {
                tokio::select! {
                    event = events.recv() => match event {
                        Ok(Event::Activity { item }) => tracing::info!("{} {}: {}", if item.ok { "ok " } else { "err" }, item.kind, item.message),
                        Ok(_) => {}
                        Err(_) => {}
                    },
                    _ = tokio::signal::ctrl_c() => { tracing::info!("stopping"); break; }
                }
            }
        }
    }
    Ok(())
}

/// One tool step, as the server or the window's chat sends it (Origin::Local: the person at this computer asked).
async fn step(
    desktop: &Desktop,
    kind: &str,
    payload: serde_json::Value,
) -> wiqo_core::wire::CommandResult {
    let request = wiqo_core::wire::Request {
        id: uuid::Uuid::new_v4(),
        kind: kind.to_string(),
        payload,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        signature_version: None,
        created_at_epoch_ms: None,
        signature: None,
        nonce: None,
    };
    desktop
        .runtime
        .engine
        .execute(&request, &wiqo_core::engine::Origin::Local)
        .await
}

async fn selftest(desktop: &Desktop, app: Option<String>, headless: bool, verbose: bool) -> usize {
    use serde_json::json;
    let folder = format!("~/Desktop/wiqo-selftest-{}", std::process::id());
    let file = format!("{folder}/not.txt");
    let copy = format!("{folder}/kopya.txt");
    let moved = format!("{folder}/alt/tasindi.txt");
    let app = app.unwrap_or_else(|| {
        if cfg!(windows) {
            "Notepad".into()
        } else {
            "TextEdit".into()
        }
    });
    let op = |path: &str| json!({ "kind": "fileOperation", "value": { "path": path } });
    let mut steps: Vec<(&str, serde_json::Value)> = vec![
        ("ping", json!({ "kind": "none" })),
        ("getSystemStatus", json!({ "kind": "none" })),
        ("listFiles", op("~/Desktop")),
        ("listFiles", op("~/Masaüstü")),
        ("listFiles", op("~/Documents")),
        ("listFiles", op("~/Downloads")),
        (
            "writeFile",
            json!({ "kind": "fileOperation", "value": { "path": file, "content": "Wiqo self test: çalışıyor." } }),
        ),
        ("readFile", op(&file)),
        (
            "copyFile",
            json!({ "kind": "fileOperation", "value": { "path": file, "destination": copy } }),
        ),
        (
            "moveFile",
            json!({ "kind": "fileOperation", "value": { "path": copy, "destination": moved } }),
        ),
        ("listFiles", op(&folder)),
        (
            "searchFiles",
            json!({ "kind": "fileSearch", "value": { "query": "tasindi" } }),
        ),
        (
            "fileUtility",
            json!({ "kind": "fileUtility", "value": { "action": "inspect", "path": file } }),
        ),
        (
            "systemUtility",
            json!({ "kind": "systemUtility", "value": { "action": "battery" } }),
        ),
    ];
    if !headless {
        steps.extend([
            ("setClipboardText", json!({ "kind": "clipboardText", "value": "wiqo-selftest" })),
            ("getClipboardText", json!({ "kind": "none" })),
            ("captureScreen", json!({ "kind": "screenCapture", "value": { "maxWidth": 800 } })),
            ("listRunningApplications", json!({ "kind": "none" })),
            ("openApplicationByName", json!({ "kind": "applicationQuery", "value": app })),
            ("controlApplication", json!({ "kind": "applicationControl", "value": { "action": "quit", "target": app, "force": true } })),
        ]);
    }
    steps.push((
        "deleteFile",
        json!({ "kind": "fileOperation", "value": { "path": folder, "recursive": true } }),
    ));
    let mut failures = 0;
    for (kind, payload) in steps {
        let started = std::time::Instant::now();
        let result = step(desktop, kind, payload).await;
        let ok =
            result.status == wiqo_core::wire::CommandStatus::Completed && result.error.is_none();
        if !ok {
            failures += 1;
        }
        println!(
            "{}",
            json!({
                "step": kind,
                "ok": ok,
                "ms": started.elapsed().as_millis() as u64,
                "status": result.status,
                "code": result.error.as_ref().map(|error| error.code.clone()),
                "message": result.message.chars().take(300).collect::<String>(),
            })
        );
        if verbose {
            let data = serde_json::to_string(&result.data).unwrap_or_default();
            println!("    data: {}", data.chars().take(1_200).collect::<String>());
        }
    }
    failures
}
