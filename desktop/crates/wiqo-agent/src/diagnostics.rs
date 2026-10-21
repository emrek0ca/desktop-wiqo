//! "Sorun bildir": one zip on the Desktop with what it takes to see what went wrong — the logs, the last crash reports, the connection, the
//! permissions and the recent commands — and nothing secret (no tokens, no keys; credentials in the log lines are masked).

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde_json::json;
use wiqo_core::storage::DataDir;

const CRASH_PREFIX: &str = "crash-";

/// Writes a crash report into the data folder when the app panics, then lets the default handler run.
pub fn install_crash_reporter() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(dir) = DataDir::default_location() {
            let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
            let thread = std::thread::current().name().unwrap_or("?").to_string();
            let report = format!(
                "Wiqo {} crashed at {}\nthread: {thread}\n{info}\n\n{}",
                env!("CARGO_PKG_VERSION"),
                chrono::Utc::now().to_rfc3339(),
                std::backtrace::Backtrace::force_capture()
            );
            let _ = std::fs::write(dir.file(&format!("{CRASH_PREFIX}{stamp}.txt")), report);
        }
        previous(info);
    }));
}

fn crash_reports(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|items| {
            items
                .filter_map(|item| item.ok().map(|item| item.path()))
                .filter(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with(CRASH_PREFIX))
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found.reverse();
    found.truncate(5);
    found
}

/// Crash reports written since the window last showed them (each is shown once).
pub fn unseen_crashes() -> usize {
    let Ok(dir) = DataDir::default_location() else {
        return 0;
    };
    let marker = dir.file("crashes-seen");
    let seen = std::fs::read_to_string(&marker).unwrap_or_default();
    let reports = crash_reports(&dir.file(""));
    let newest = reports
        .first()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let unseen = reports
        .iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().as_ref() > seen.as_str())
        })
        .count();
    if !newest.is_empty() {
        let _ = std::fs::write(marker, newest);
    }
    unseen
}

/// The diagnostics zip in memory: the summary, the person's note, the log tails and the latest crash reports, secrets masked.
pub fn build(summary: serde_json::Value, note: &str) -> Result<Vec<u8>> {
    let dir = DataDir::default_location().context("data folder")?;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut add = |name: &str, text: &str| -> Result<()> {
        zip.start_file(name, options)?;
        zip.write_all(wiqo_ops::redact_secrets(text).as_bytes())?;
        Ok(())
    };
    let system = json!({
        "app": env!("CARGO_PKG_VERSION"),
        "os": format!("{} {}", sysinfo::System::name().unwrap_or_default(), sysinfo::System::os_version().unwrap_or_default()),
        "arch": std::env::consts::ARCH,
        "createdAt": chrono::Utc::now().to_rfc3339(),
    });
    add(
        "summary.json",
        &serde_json::to_string_pretty(&json!({ "system": system, "desktop": summary }))?,
    )?;
    if !note.trim().is_empty() {
        add("note.txt", note)?;
    }
    for log in ["desktop.log", "desktop.old.log"] {
        if let Ok(text) = std::fs::read_to_string(dir.file(log)) {
            let lines: Vec<&str> = text.lines().collect();
            add(log, &lines[lines.len().saturating_sub(4_000)..].join("\n"))?;
        }
    }
    for report in crash_reports(&dir.file("")) {
        if let (Some(name), Ok(text)) = (report.file_name(), std::fs::read_to_string(&report)) {
            add(&format!("crashes/{}", name.to_string_lossy()), &text)?;
        }
    }
    Ok(zip.finish()?.into_inner())
}

/// Saves a built zip on the Desktop and returns where it is.
pub fn save(bytes: &[u8]) -> Result<PathBuf> {
    let desktop = dirs::desktop_dir()
        .or_else(dirs::home_dir)
        .context("no Desktop")?;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H.%M");
    let path = desktop.join(format!("Wiqo sorun raporu {stamp}.zip"));
    std::fs::write(&path, bytes)?;
    Ok(path)
}
