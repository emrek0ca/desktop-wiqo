//! One-sentence chores on files (`fileUtility`): a new folder, a zip, merged or split PDFs, a smaller picture, duplicates, disk usage, a tidy folder, a batch
//! rename, documents moved as one all-or-nothing step, the recurring charges in bank statements and e-invoices (subscriptions.rs). Every path goes through the same resolution as the other file commands (no symlink targets, Turkish
//! folder names translated), and nothing is overwritten: a taken name gets " (2)".

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use wiqo_contract::is_valid_file_name;
use wiqo_contract::names::{normalize, score};
use wiqo_core::engine::{Context, Lane};
use wiqo_core::wire::WireError;

use crate::fsx::{self, entry, free_path, human, is_dir, is_regular_file, is_symlink};
use crate::op::{data, error, payload, permission, tool_failure, Blocking, Done, Result};
use crate::process::run_with_timeout;
use crate::Op;

const REPORT_CHARS: usize = 20_000;
const DOCUMENT_TYPES: [&str; 8] = ["pdf", "doc", "docx", "rtf", "odt", "pages", "txt", "md"];

mod archive;
mod finder;
mod folders;
mod pdf;
mod pictures;
mod reports;
mod subscriptions;
mod tidy;

use archive::*;
use finder::*;
use folders::*;
use pdf::*;
use pictures::*;
use reports::*;
use subscriptions::*;
use tidy::*;

pub(crate) fn operations() -> Vec<Op> {
    vec![Arc::new(Blocking {
        name: "fileUtility",
        lane: Lane::SideEffect,
        run: file_utility,
    })]
}

/// `FileUtilityPayload`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Request {
    action: String,
    path: Option<String>,
    paths: Option<Vec<String>>,
    destination: Option<String>,
    name: Option<String>,
    tags: Option<Vec<String>>,
    mode: Option<String>,
    limit: usize,
    /// A batch (organize, batchRename, moveDocuments) only says what it would do and touches nothing: the desktop asks it before the person's
    /// yes, so the question lists the files and where each goes ("Önce prova").
    preview: bool,
}

impl Default for Request {
    fn default() -> Self {
        Request {
            action: String::new(),
            path: None,
            paths: None,
            destination: None,
            name: None,
            tags: None,
            mode: None,
            limit: 50,
            preview: false,
        }
    }
}

/// What a batch (organize, batchRename, moveDocuments) would do, asked by the desktop before the person's yes so the question can list it;
/// `None` for any other step or when it cannot say. Nothing on disk is touched.
pub fn batch_preview(ctx: &Context, step_payload: &Value) -> Option<Value> {
    let mut request: Request = payload(step_payload, "fileUtility", "dosya işlemi").ok()?;
    let limit = request.limit.clamp(1, 500);
    request.preview = true;
    let done = match request.action.as_str() {
        "organize" => organize(ctx, &request, limit),
        "batchRename" => batch_rename(ctx, &request, limit),
        "moveDocuments" => move_documents(ctx, &request, limit),
        _ => return None,
    }
    .ok()?;
    done.data.map(|data| data["value"].clone())
}

/// What a batch would do, as the desktop's question shows it: every file and where it goes, nothing touched.
fn preview_of(moves: &[(PathBuf, PathBuf)], made: &[PathBuf]) -> Done {
    let lines: Vec<Value> = moves
        .iter()
        .map(|(from, to)| json!({ "from": fsx::abbreviate(from), "to": fsx::abbreviate(to) }))
        .collect();
    Done::new(
        format!("{} dosya değişecek; hiçbirine dokunulmadı.", moves.len()),
        Some(data("filePreview", json!({ "moves": lines, "count": moves.len(), "newFolders": made.iter().map(|path| fsx::abbreviate(path)).collect::<Vec<_>>() }))),
    )
}

fn file_utility(ctx: &Context, value: &Value) -> Result<Done> {
    let request: Request = payload(value, "fileUtility", "dosya işlemi")?;
    let limit = request.limit.clamp(1, 500);
    match request.action.as_str() {
        "inspect" => inspect(&request),
        "moveDocuments" => move_documents(ctx, &request, limit),
        "createFolder" => create_folder(&request),
        "archive" => archive(ctx, &request),
        "extract" => extract(&request),
        "mergePDF" => merge_pdf(&request),
        "splitPDF" => split_pdf(&request, limit),
        "compressImage" => compress_image(&request),
        "findDuplicates" => find_duplicates(&request, limit),
        "diskUsage" => disk_usage(&request, limit),
        "recentFiles" => recent_files(limit),
        "restoreFromTrash" => restore_from_trash(&request),
        "reveal" => reveal(&request),
        "openWith" => open_with(&request),
        "print" => print(&request),
        "ocr" => ocr(&request),
        "setTags" => set_tags(&request),
        "organize" => organize(ctx, &request, limit),
        "batchRename" => batch_rename(ctx, &request, limit),
        "findSubscriptions" => find_subscriptions(&request),
        _ => Err(error(
            "INVALID_PAYLOAD",
            "Bu dosya işlemi tanınmıyor.",
            "validation",
        )),
    }
}

// ----- shared helpers -----

fn fail(code: &str, message: impl Into<String>) -> WireError {
    tool_failure(code, message)
}

fn existing(raw: &Option<String>) -> Result<PathBuf> {
    let text = raw.clone().unwrap_or_default();
    fsx::resolve(&text, false)
        .filter(|path| path.exists())
        .ok_or_else(|| fsx::missing_path_error(&text, &format!("\"{text}\" bulunamadı.")))
}

fn existing_folder(raw: &Option<String>) -> Result<PathBuf> {
    let folder = existing(raw)?;
    if !is_dir(&folder) {
        return Err(fail(
            "NOT_A_FOLDER",
            format!("{} bir klasör değil.", name_of(&folder)),
        ));
    }
    Ok(folder)
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Where a result goes: the fallback's name inside a destination folder, the destination itself, or the fallback; always a free name.
fn target(raw: &Option<String>, fallback: &Path) -> Result<PathBuf> {
    let Some(raw) = raw else {
        return Ok(free_path(fallback));
    };
    if let Some(folder) = fsx::resolve(raw, false).filter(|path| is_dir(path)) {
        return Ok(free_path(&folder.join(name_of(fallback))));
    }
    fsx::resolve(raw, true)
        .map(|path| free_path(&path))
        .ok_or_else(|| {
            error(
                "INVALID_PATH",
                "Hedef yer geçersiz; hedef klasörün var olduğunu kontrol et.",
                "validation",
            )
        })
}

fn regular_files(folder: &Path, recursive: bool, cap: usize) -> Vec<PathBuf> {
    let hidden = |path: &Path| name_of(path).starts_with('.');
    if recursive {
        walkdir::WalkDir::new(folder)
            .follow_links(false)
            .into_iter()
            .filter_entry(|item| item.depth() == 0 || !hidden(item.path()))
            .filter_map(|item| item.ok())
            .filter(|item| item.file_type().is_file())
            .map(|item| item.into_path())
            .take(cap)
            .collect()
    } else {
        let mut found: Vec<PathBuf> = fs::read_dir(folder)
            .map(|items| {
                items
                    .filter_map(|item| item.ok())
                    .map(|item| item.path())
                    .filter(|path| !hidden(path) && is_regular_file(path))
                    .collect()
            })
            .unwrap_or_default();
        found.sort_by(|a, b| fsx::natural_cmp(&name_of(a), &name_of(b)));
        found.truncate(cap);
        found
    }
}

fn size(path: &Path) -> u64 {
    fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn modified(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn short(path: &Path) -> String {
    fsx::abbreviate(path)
}

fn listing(paths: &[PathBuf]) -> Value {
    data(
        "files",
        paths
            .iter()
            .filter_map(|path| entry(path))
            .collect::<Vec<_>>(),
    )
}

/// A listing whose entries carry the batch's one undo id: the phone offers a single tap that puts every moved file back (`moves`: now, was) and
/// removes the folders the batch made (`made`) when they are left empty.
fn undoable(
    ctx: &Context,
    shown: &[PathBuf],
    moves: &[(PathBuf, PathBuf)],
    made: &[PathBuf],
) -> Value {
    let undo_id = crate::files::undo::register_moves(ctx, moves, made);
    data(
        "files",
        shown
            .iter()
            .filter_map(|path| entry(path))
            .map(|mut item| {
                item.undo_id = undo_id.clone();
                item
            })
            .collect::<Vec<_>>(),
    )
}

fn report(path: &str, text: &str) -> Value {
    let truncated = text.chars().count() > REPORT_CHARS;
    let text: String = text.chars().take(REPORT_CHARS).collect();
    data(
        "fileData",
        json!({ "path": path, "mimeType": "text/plain", "text": text, "truncated": truncated }),
    )
}

fn date(time: SystemTime, format: &str) -> String {
    chrono::DateTime::<chrono::Local>::from(time)
        .format(format)
        .to_string()
}

fn run(program: &str, args: &[&str], seconds: u64) -> Option<std::process::Output> {
    let mut command = crate::process::command(program);
    command.args(args);
    run_with_timeout(command, Duration::from_secs(seconds))
}

fn succeeded(output: &Option<std::process::Output>) -> bool {
    output.as_ref().is_some_and(|out| out.status.success())
}

#[cfg(not(target_os = "macos"))]
fn set_tags(_request: &Request) -> Result<Done> {
    Err(fail(
        "TAG_UNSUPPORTED",
        "Dosya etiketleri yalnızca Mac'te var.",
    ))
}

#[cfg(target_os = "macos")]
fn set_tags(request: &Request) -> Result<Done> {
    let path = existing(&request.path)?;
    let tags = request.tags.clone().unwrap_or_default();
    {
        // Finder keeps tags as a binary property list of "name\n0" strings in one extended attribute.
        let list = plist::Value::Array(
            tags.iter()
                .map(|tag| plist::Value::String(format!("{tag}\n0")))
                .collect(),
        );
        let mut bytes = Vec::new();
        plist::to_writer_binary(&mut bytes, &list)
            .map_err(|_| fail("TAG_FAILED", "Etiketler yazılamadı."))?;
        let out = run(
            "/usr/bin/xattr",
            &[
                "-wx",
                "com.apple.metadata:_kMDItemUserTags",
                &hex::encode(bytes),
                &path.to_string_lossy(),
            ],
            10,
        );
        if !succeeded(&out) {
            return Err(fail("TAG_FAILED", "Etiketler yazılamadı."));
        }
    }
    Ok(Done::new(
        format!("{} etiketlendi: {}.", name_of(&path), tags.join(", ")),
        Some(listing(&[path])),
    ))
}

#[cfg(test)]
mod tests;
