//! Taking a file change back. Every verified write, move, copy or delete leaves a small record here; one tap on the phone applies it, but only while the file is
//! still exactly as the operation left it (a file changed since is never overwritten) and only within a week.

use std::fs;
use std::path::PathBuf;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wiqo_core::engine::Context;

use crate::fsx;
use crate::op::{error, Result};

const MAX_BYTES: usize = 2_000_000;
const KEEP_DAYS: i64 = 7;
const KEEP_RECORDS: usize = 200;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    id: String,
    path: String,
    created_at: DateTime<Utc>,
    /// The content to put back, when the operation replaced a file's content.
    before: Option<String>,
    after_sha256: String,
    /// Where to move the file back to (a move or a delete), instead of restoring content.
    restore_path: Option<String>,
}

fn folder(ctx: &Context) -> std::io::Result<PathBuf> {
    let dir = ctx.services.data_dir.file("file-undo");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The file's SHA-256, read in pieces: a video must not be loaded whole just to be recorded (a move of a large file used to be refused for that).
fn sha_file(path: &std::path::Path) -> Option<String> {
    let mut hasher = Sha256::new();
    std::io::copy(&mut fs::File::open(path).ok()?, &mut hasher).ok()?;
    Some(hex::encode(hasher.finalize()))
}

/// Records how to undo a change to the regular file at `path`. `before` is its earlier content; `restore_path` is where a moved or trashed file came from.
/// Returns the undo id, or `None` when the change cannot be undone safely (then the caller must not offer an undo).
pub fn register(
    ctx: &Context,
    path: &str,
    before: Option<&[u8]>,
    restore_path: Option<&str>,
) -> Option<String> {
    let resolved = fsx::resolve(path, false)?;
    if !fsx::is_regular_file(&resolved) {
        return None;
    }
    // Only a record that stores content ("before") is bounded; a move or a copy is recorded by where the file is and its hash, whatever its size.
    if before.is_some_and(|bytes| bytes.len() > MAX_BYTES) {
        return None;
    }
    let after_sha256 = sha_file(&resolved)?;
    if let Some(restore) = restore_path {
        if fsx::resolve(restore, true).is_none() || fs::symlink_metadata(restore).is_ok() {
            return None;
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let record = Record {
        id: id.clone(),
        path: fsx::abbreviate(&resolved).replacen('~', &fsx::home().to_string_lossy(), 1),
        created_at: Utc::now(),
        before: before.map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes)),
        after_sha256,
        restore_path: restore_path.map(str::to_string),
    };
    let dir = folder(ctx).ok()?;
    let file = dir.join(format!("{id}.json"));
    fs::write(&file, serde_json::to_vec(&record).ok()?).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&file, fs::Permissions::from_mode(0o600));
    }
    prune(&dir);
    Some(id)
}

fn prune(dir: &PathBuf) {
    let Ok(read) = fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = read
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    files.sort();
    let excess = files.len().saturating_sub(KEEP_RECORDS);
    for (_, path) in files.into_iter().take(excess) {
        let _ = fs::remove_file(path);
    }
}

fn stale(message: &str) -> wiqo_core::wire::WireError {
    error("FILE_UNDO_STALE", message, "tool_failure")
}

/// Applies an undo record. Returns the sentence for the person.
pub fn undo_file(ctx: &Context, undo_id: &str) -> Result<String> {
    let invalid = || {
        error(
            "FILE_UNDO_INVALID",
            "Geri alma kaydı bulunamadı veya süresi doldu.",
            "tool_failure",
        )
    };
    if uuid::Uuid::parse_str(undo_id).is_err() {
        return Err(invalid());
    }
    let dir = folder(ctx).map_err(|_| invalid())?;
    let file = dir.join(format!("{}.json", undo_id.to_lowercase()));
    let record: Record =
        serde_json::from_slice(&fs::read(&file).map_err(|_| invalid())?).map_err(|_| invalid())?;
    if record.id != undo_id.to_lowercase()
        || (Utc::now() - record.created_at).num_days() >= KEEP_DAYS
    {
        return Err(invalid());
    }
    let current_path = fsx::resolve(&record.path, false)
        .ok_or_else(|| stale("Dosya işlemden sonra değişmiş; yeni içerik korundu."))?;
    let current = sha_file(&current_path)
        .ok_or_else(|| stale("Dosya işlemden sonra değişmiş; yeni içerik korundu."))?;
    if current != record.after_sha256 {
        return Err(stale("Dosya işlemden sonra değişmiş; yeni içerik korundu."));
    }
    let failed = || {
        error(
            "FILE_UNDO_FAILED",
            "Geri alma tamamlanamadı; dosyanın durumu doğrulanamadı.",
            "tool_failure",
        )
    };
    if let Some(restore) = &record.restore_path {
        let destination = fsx::resolve(restore, true)
            .filter(|path| fs::symlink_metadata(path).is_err())
            .ok_or_else(|| stale("Özgün konum dolu veya güvenli değil; dosya taşınmadı."))?;
        crate::files::move_across(&current_path, &destination).map_err(|_| failed())?;
    } else if let Some(before) = &record.before {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(before)
            .map_err(|_| failed())?;
        crate::files::write_atomic(&current_path, &bytes).map_err(|_| failed())?;
    } else {
        fs::remove_file(&current_path).map_err(|_| failed())?;
    }
    let _ = fs::remove_file(file);
    Ok("Dosya önceki haline döndürüldü.".to_string())
}
