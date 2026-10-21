//! `wiqo-agent`: the Wiqo desktop without a window.
//!
//!   wiqo-agent pair                   show a pairing code for a phone to scan and wait for the phone
//!   wiqo-agent run                    connect to the server and run the commands that arrive
//!   wiqo-agent status                 what this desktop is, who it is paired with, what it can do
//!   wiqo-agent full-access on|off     let the server's PC agent run commands here without asking the phone each time

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
    FullAccess { state: String },
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
