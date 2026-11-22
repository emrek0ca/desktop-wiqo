//! Files and folders: list, read, write, move, copy, delete. Every change is read back before it is reported, a replaced file is
//! backed up first, and a verified change leaves an undo record (undo.rs). This folder is the whole file family: file operations (here), `documents`,
//! `search`, `transfer` (to and from the phone), `utility` (one-sentence chores), `convert` and `plan` (several file steps in one call).

mod convert;
mod documents;
mod plan;
mod search;
mod transfer;
pub(crate) mod undo;
mod utility;
pub use documents::installed_browsers;
pub use utility::batch_preview;

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use wiqo_contract::FilePolicy;
use wiqo_core::engine::{Context, Lane};
use wiqo_core::wire::WireError;

use crate::fsx::{self, entry, is_dir, is_regular_file, is_symlink, Entry};
use crate::op::{data, error, payload, permission, tool_failure, Blocking, Done, Result};
use crate::Op;

const MAX_LOCAL_READ_BYTES: u64 = 512 * 1024;
const MAX_TEXT_CHARS: usize = 200_000;
const BACKUPS_KEPT_PER_FILE: usize = 10;

/// Every operation of the file family: the plain file operations here, then documents, search, transfers, chores, conversion and file plans.
pub(crate) fn operations() -> Vec<Op> {
    [
        own_operations(),
        search::operations(),
        transfer::operations(),
        utility::operations(),
        convert::operations(),
        plan::operations(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn own_operations() -> Vec<Op> {
    let read = Lane::ReadOnly;
    let write = Lane::SideEffect;
    vec![
        Arc::new(Blocking {
            name: "listFiles",
            lane: read,
            run: list_files,
        }),
        Arc::new(Blocking {
            name: "readFile",
            lane: read,
            run: read_file,
        }),
        Arc::new(Blocking {
            name: "writeFile",
            lane: write,
            run: write_file,
        }),
        Arc::new(Blocking {
            name: "moveFile",
            lane: write,
            run: |ctx, value| transfer_op(ctx, value, false),
        }),
        Arc::new(Blocking {
            name: "copyFile",
            lane: write,
            run: |ctx, value| transfer_op(ctx, value, true),
        }),
        Arc::new(Blocking {
            name: "deleteFile",
            lane: write,
            run: delete_file,
        }),
    ]
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum CollisionPolicy {
    Rename,
    Replace,
    Fail,
}

/// `LocalFileOperationPayload`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct FileOperation {
    path: String,
    destination: Option<String>,
    content: Option<String>,
    overwrite: bool,
    collision_policy: Option<CollisionPolicy>,
    recursive: bool,
    page: i64,
    page_size: i64,
}

impl Default for FileOperation {
    fn default() -> Self {
        FileOperation {
            path: String::new(),
            destination: None,
            content: None,
            overwrite: false,
            collision_policy: None,
            recursive: false,
            page: 0,
            page_size: 100,
        }
    }
}

fn operation(value: &Value) -> Result<FileOperation> {
    payload(value, "fileOperation", "dosya")
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

// ----- shared helpers (also used by documents, transfer, undo) -----

/// Writes next to the target and renames over it, so a crash never leaves half a file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_file_name(format!(
        ".{}.wiqo-{}",
        path.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
        uuid::Uuid::new_v4()
    ));
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

/// Moves a file or folder; across volumes (where rename cannot work) it copies and then removes the original.
pub(crate) fn move_across(from: &Path, to: &Path) -> std::io::Result<()> {
    move_tracked(from, to, &mut |_| {})
}

/// A move that tells `copied` how many bytes it has copied when the move crosses disks (a rename on the same disk copies nothing).
fn move_tracked(from: &Path, to: &Path, copied: &mut dyn FnMut(u64)) -> std::io::Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_tracked(from, to, copied)?;
    if from.is_dir() {
        fs::remove_dir_all(from)
    } else {
        fs::remove_file(from)
    }
}

/// Files above this are copied in pieces so the copy can say how far it is; smaller ones keep the system's own fast copy.
const TRACKED_COPY_FROM: u64 = 64 * 1024 * 1024;

fn copy_tracked(from: &Path, to: &Path, copied: &mut dyn FnMut(u64)) -> std::io::Result<()> {
    if from.is_dir() {
        fs::create_dir_all(to)?;
        for child in fs::read_dir(from)? {
            let child = child?;
            copy_tracked(&child.path(), &to.join(child.file_name()), copied)?;
        }
        return Ok(());
    }
    let length = fs::metadata(from)?.len();
    if length < TRACKED_COPY_FROM {
        fs::copy(from, to)?;
        copied(length);
        return Ok(());
    }
    let (mut source, mut target) = (fs::File::open(from)?, fs::File::create(to)?);
    let mut buffer = vec![0; 8 * 1024 * 1024];
    loop {
        let read = std::io::Read::read(&mut source, &mut buffer)?;
        if read == 0 {
            break;
        }
        std::io::Write::write_all(&mut target, &buffer[..read])?;
        copied(read as u64);
    }
    target.sync_all()?;
    fs::set_permissions(to, fs::metadata(from)?.permissions())
}

/// The bytes under a path: what a copy of it has to move.
pub(crate) fn tree_bytes(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|item| item.ok())
        .filter(|item| item.file_type().is_file())
        .filter_map(|item| item.metadata().ok())
        .map(|meta| meta.len())
        .sum()
}

/// Where the previous version of a file goes before Wiqo replaces it, so an edit can be undone by hand. A failed backup stops the overwrite.
pub(crate) fn back_up(ctx: &Context, file: &Path) -> Result<()> {
    let failed = |source: &std::io::Error| {
        fsx::local_file_error(
            "BACKUP_FAILED",
            "Eski sürümün yedeği alınamadı, dosya değiştirilmedi.",
            source,
        )
    };
    let identity = sha(file.to_string_lossy().as_bytes());
    let directory = ctx.services.data_dir.file("Backups").join(identity);
    fs::create_dir_all(&directory).map_err(|source| failed(&source))?;
    let stamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%SZ");
    let name: String = file
        .file_name()
        .map(|name| name.to_string_lossy().chars().take(80).collect())
        .unwrap_or_default();
    let target = directory.join(format!("{stamp}-{}-{name}", uuid::Uuid::new_v4()));
    fs::copy(file, &target).map_err(|source| failed(&source))?;
    let mut siblings: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(&directory)
        .map_err(|source| failed(&source))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path() != target)
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    siblings.sort();
    let excess = siblings.len().saturating_sub(BACKUPS_KEPT_PER_FILE - 1);
    for (_, old) in siblings.into_iter().take(excess) {
        let _ = fs::remove_file(old);
    }
    Ok(())
}

/// Reads the committed file back before reporting success; the evidence is metadata only. `requested` is the path the call named: when the name was
/// taken and the file landed beside it, the receipt carries both (`requested_path`), the same way a copy or move that was renamed does.
fn verified_file_result(
    path: &Path,
    requested: &Path,
    expected_content: Option<&[u8]>,
    undo_id: Option<String>,
) -> Result<Done> {
    let failure = || {
        tool_failure(
            "FILE_VERIFICATION_FAILED",
            "İşlem uygulandı ancak dosya sonucu doğrulanamadı; otomatik tekrar yapılmadı.",
        )
    };
    let resolved = fsx::resolve(&path.to_string_lossy(), false)
        .filter(|resolved| is_regular_file(resolved))
        .ok_or_else(failure)?;
    if let Some(expected) = expected_content {
        if fs::read(&resolved).map_err(|_| failure())? != expected {
            return Err(failure());
        }
    }
    let mut item = entry(&resolved).ok_or_else(failure)?;
    item.undo_id = undo_id;
    if requested != path {
        item.requested_path = Some(file_path_text(requested));
    }
    let message = format!("Doğrulandı: {} · {} bayt", item.path, item.byte_count);
    Ok(Done::new(message, Some(data("files", vec![item]))))
}

// ----- listFiles / readFile / writeFile -----

pub(crate) fn list_files(_ctx: &Context, value: &Value) -> Result<Done> {
    let request = operation(value)?;
    let directory = fsx::resolve(&request.path, false)
        .filter(|path| path.exists())
        .ok_or_else(|| {
            fsx::missing_path_error(
                &request.path,
                &format!("\"{}\" klasörü bulunamadı.", request.path),
            )
        })?;
    if FilePolicy::get().is_sensitive(&directory.to_string_lossy()) {
        return Err(permission(
            "SENSITIVE_FILE_BLOCKED",
            "Kimlik bilgisi içerebilecek klasörler Wiqo tarafından listelenemez.",
        ));
    }
    if !is_dir(&directory) {
        return Err(tool_failure(
            "NOT_A_DIRECTORY",
            "İstenen yol bir klasör değil.",
        ));
    }
    let mut names: Vec<PathBuf> = fs::read_dir(&directory)
        .map_err(|source| {
            fsx::local_file_error("FILE_LIST_FAILED", "Klasör içeriği listelenemedi.", &source)
        })?
        .filter_map(|item| item.ok())
        .map(|item| item.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                !fsx::is_hidden(path) && !FilePolicy::get().is_sensitive(&name.to_string_lossy())
            })
        })
        .collect();
    names.sort_by(|a, b| {
        fsx::natural_cmp(
            &a.file_name().unwrap_or_default().to_string_lossy(),
            &b.file_name().unwrap_or_default().to_string_lossy(),
        )
    });
    let size = request.page_size.max(1) as usize;
    let start = ((request.page.max(0) as usize).saturating_mul(size)).min(names.len());
    let page: Vec<Entry> = names
        .iter()
        .skip(start)
        .take(size)
        .filter_map(|path| {
            let mut item = entry(path)?;
            if item.is_directory {
                item.item_count = fsx::visible_count(path);
            }
            Some(item)
        })
        .collect();
    // "24 öğe: 7 klasör, 17 dosya (12 MB)", and which part of the folder this page is.
    let folders = page.iter().filter(|item| item.is_directory).count();
    let files = page.len() - folders;
    let bytes: i64 = page.iter().map(|item| item.byte_count).sum();
    let mut message = if page.is_empty() {
        "Klasör boş.".to_string()
    } else {
        let mut parts = Vec::new();
        if folders > 0 {
            parts.push(format!("{folders} klasör"));
        }
        if files > 0 {
            parts.push(format!(
                "{files} dosya ({})",
                crate::fsx::human(bytes.max(0) as u64)
            ));
        }
        format!("{} öğe: {}", page.len(), parts.join(", "))
    };
    if names.len() > page.len() {
        message.push_str(&format!(
            "; toplam {} öğeden {}–{} gösteriliyor",
            names.len(),
            start + 1,
            start + page.len()
        ));
    }
    Ok(Done::new(message + ".", Some(data("files", page))))
}

pub(crate) fn read_file(_ctx: &Context, value: &Value) -> Result<Done> {
    let request = operation(value)?;
    let policy = FilePolicy::get();
    if policy.is_sensitive(&request.path) {
        return Err(fsx::sensitive_blocked("dosyalar"));
    }
    let file = fsx::resolve(&request.path, false).ok_or_else(|| {
        fsx::missing_path_error(&request.path, &format!("\"{}\" bulunamadı.", request.path))
    })?;
    if is_dir(&file) || !is_regular_file(&file) {
        return Err(tool_failure(
            "FILE_NOT_REGULAR",
            "Yalnızca normal dosyalar okunabilir.",
        ));
    }
    if policy.is_sensitive(&file.to_string_lossy()) {
        return Err(fsx::sensitive_blocked("dosyalar"));
    }
    let mime = fsx::mime_type(&file);
    // PDF, Word and Excel come back as base64, which the server drops before the model sees it: the text is extracted instead.
    if documents::is_document(&file) {
        let extracted = documents::extract_text(&file)?;
        let truncated = extracted.chars().count() > MAX_TEXT_CHARS;
        let text: String = extracted.chars().take(MAX_TEXT_CHARS).collect();
        return Ok(Done::new(
            "Dosya okundu.",
            Some(data(
                "fileData",
                json!({ "path": file_path_text(&file), "mimeType": mime, "text": text, "truncated": truncated }),
            )),
        ));
    }
    // The byte limit is on what goes back raw: a document above comes back as its text whatever the size of the file, and a long text file as its beginning.
    let read_failed = |source: std::io::Error| {
        fsx::local_file_error("FILE_READ_FAILED", "Dosya okunamadı.", &source)
    };
    let size = fs::metadata(&file).map_err(read_failed)?.len();
    if size > MAX_LOCAL_READ_BYTES {
        if !fsx::is_likely_text(&file) {
            return Err(tool_failure("FILE_TOO_LARGE", "Bu dosya metin değil ve tek yanıt için çok büyük; okumak yerine telefona gönderilebilir."));
        }
        let mut head = Vec::new();
        fs::File::open(&file)
            .and_then(|opened| opened.take(MAX_LOCAL_READ_BYTES).read_to_end(&mut head))
            .map_err(read_failed)?;
        // The cut can fall inside a letter; left there, the whole text would read as not UTF-8.
        if let Err(cut) = std::str::from_utf8(&head) {
            if cut.error_len().is_none() {
                head.truncate(cut.valid_up_to());
            }
        }
        return Ok(Done::new(
            "Dosyanın başı okundu; dosya bundan daha uzun.",
            Some(data(
                "fileData",
                json!({ "path": file_path_text(&file), "mimeType": mime, "text": fsx::decode_text(&head), "truncated": true }),
            )),
        ));
    }
    let bytes = fs::read(&file).map_err(read_failed)?;
    let content = match fsx::text_if_likely(&file, &bytes) {
        Some(text) => {
            json!({ "path": file_path_text(&file), "mimeType": mime, "text": text, "truncated": false })
        }
        None => {
            json!({ "path": file_path_text(&file), "mimeType": mime, "base64": base64_of(&bytes), "truncated": false })
        }
    };
    Ok(Done::new("Dosya okundu.", Some(data("fileData", content))))
}

pub(crate) fn file_path_text(path: &Path) -> String {
    dunce::simplified(path).to_string_lossy().replace('\\', "/")
}

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn write_file(ctx: &Context, value: &Value) -> Result<Done> {
    let request = operation(value)?;
    let invalid = || {
        tool_failure(
            "INVALID_FILE_PATH",
            "Dosya yolu veya hedef klasör geçersiz.",
        )
    };
    let content = request.content.as_deref().ok_or_else(invalid)?;
    fsx::ensure_parent(&request.path);
    let destination = fsx::resolve(&request.path, true).ok_or_else(invalid)?;
    let parent = destination.parent().ok_or_else(invalid)?;
    if !is_dir(parent) {
        return Err(invalid());
    }
    if is_symlink(&destination) {
        return Err(permission(
            "SYMLINK_REJECTED",
            "Sembolik bağlantının üzerine yazılmadı.",
        ));
    }
    let (target, replacing) = settle_collision(&request, &destination)?;
    // A .docx, .xlsx or .pdf path gets a real document made from the text; any other path gets the text itself.
    let bytes = documents::bytes_for(&target, content)?;
    let before = if replacing {
        fs::read(&target)
            .ok()
            .filter(|bytes| bytes.len() <= 2_000_000)
    } else {
        None
    };
    if replacing {
        back_up(ctx, &target)?;
    }
    write_atomic(&target, &bytes).map_err(|source| {
        fsx::local_file_error("FILE_WRITE_FAILED", "Dosya yazılamadı.", &source)
    })?;
    let undo_id = if !replacing || before.is_some() {
        undo::register(ctx, &target.to_string_lossy(), before.as_deref(), None)
    } else {
        None
    };
    verified_file_result(&target, &destination, Some(&bytes), undo_id)
}

/// What an operation that makes a file does when the name is taken. One rule for every such operation: the policy the call names, otherwise both files
/// stay (the new one lands under the first free "ad (2)") unless `overwrite` says replace. Returns where to put the file and whether an existing file is
/// being replaced; a taken name is never a dead end.
fn settle_collision(request: &FileOperation, destination: &Path) -> Result<(PathBuf, bool)> {
    let policy = request.collision_policy.unwrap_or(if request.overwrite {
        CollisionPolicy::Replace
    } else {
        CollisionPolicy::Rename
    });
    if policy == CollisionPolicy::Replace && !request.overwrite {
        return Err(error(
            "OVERWRITE_REQUIRED",
            "Mevcut dosyayı değiştirmek için overwrite=true gerekir.",
            "validation",
        ));
    }
    if !destination.exists() {
        return Ok((destination.to_path_buf(), false));
    }
    match policy {
        CollisionPolicy::Fail => Err(error(
            "FILE_EXISTS",
            "Hedefte aynı adlı dosya var; hiçbir dosya değiştirilmedi.",
            "validation",
        )),
        CollisionPolicy::Rename => fsx::free_name(destination)
            .map(|target| (target, false))
            .ok_or_else(|| tool_failure("FILE_EXISTS", "Hedefte boş dosya adı bulunamadı.")),
        CollisionPolicy::Replace => Ok((destination.to_path_buf(), true)),
    }
}

// ----- move / copy -----

fn requested_target(request: &FileOperation) -> Option<PathBuf> {
    let destination = request
        .destination
        .as_deref()
        .and_then(|raw| fsx::resolve(raw, true))?;
    let source = fsx::resolve(&request.path, false)?;
    Some(if is_dir(&destination) {
        destination.join(source.file_name()?)
    } else {
        destination
    })
}

fn transfer(ctx: &Context, request: &FileOperation, copy: bool) -> Result<PathBuf> {
    // The kinds of "invalid" are told apart, so the next call fixes the right path instead of guessing which one was wrong.
    let source = fsx::resolve(&request.path, false)
        .filter(|path| path.exists())
        .ok_or_else(|| {
            tool_failure(
                "INVALID_FILE_PATH",
                "Taşınacak kaynak bulunamadı; önce arama veya listeleme ile gerçek yolu doğrula.",
            )
        })?;
    // A credential file is not copied or moved under another name (it would then pass the read and send checks).
    if FilePolicy::get().is_sensitive(&source.to_string_lossy()) || FilePolicy::get().is_sensitive(&request.path) {
        return Err(permission(
            "SENSITIVE_FILE_BLOCKED",
            "Kimlik bilgisi içerebilecek dosyalar Wiqo tarafından kopyalanamaz ya da taşınamaz.",
        ));
    }
    // "Move it into ~/Desktop/Arşiv/2026/" works when that folder does not exist yet.
    if let Some(raw) = request.destination.as_deref() {
        fsx::ensure_parent(raw);
    }
    let destination = requested_target(request).ok_or_else(|| {
        tool_failure(
            "INVALID_FILE_PATH",
            "Hedef yol geçersiz; hedef klasörü ya da yeni dosya adını belirt.",
        )
    })?;
    let parent = destination
        .parent()
        .filter(|parent| is_dir(parent))
        .ok_or_else(|| {
            tool_failure(
                "INVALID_FILE_PATH",
                "Hedef klasör oluşturulamadı; yolu ve izinleri kontrol et.",
            )
        })?;
    let _ = parent;
    if is_symlink(&source) || is_symlink(&destination) {
        return Err(permission(
            "SYMLINK_REJECTED",
            "Sembolik bağlantılar üzerinden dosya işlemi yapılmadı.",
        ));
    }
    if source == destination {
        return if copy {
            Err(error(
                "INVALID_FILE_PATH",
                "Kaynak ve hedef aynı dosya olamaz.",
                "validation",
            ))
        } else {
            Ok(source)
        };
    }
    if is_dir(&source) && destination.starts_with(&source) {
        return Err(error(
            "NESTED_FOLDER_MOVE",
            "Klasör kendi altına taşınamaz.",
            "validation",
        ));
    }
    if is_dir(&source) && !request.recursive {
        return Err(permission(
            "RECURSIVE_REQUIRED",
            "Klasör işlemi için recursive=true açıkça belirtilmeli.",
        ));
    }
    let (target, replacing) = settle_collision(request, &destination)?;
    if replacing {
        if !is_dir(&destination) {
            back_up(ctx, &destination)?;
        }
        let removal = if is_dir(&destination) {
            fs::remove_dir_all(&destination)
        } else {
            fs::remove_file(&destination)
        };
        removal.map_err(|source| {
            fsx::local_file_error(
                "FILE_REPLACE_FAILED",
                "Mevcut hedef değiştirilemedi.",
                &source,
            )
        })?;
    }
    let total = tree_bytes(&source);
    let mut done = 0;
    let mut copied = |bytes: u64| {
        done += bytes;
        ctx.progress(done, total);
    };
    let step = if copy {
        copy_tracked(&source, &target, &mut copied)
    } else {
        move_tracked(&source, &target, &mut copied)
    };
    step.map_err(|source_error| {
        fsx::local_file_error(
            if copy {
                "FILE_COPY_FAILED"
            } else {
                "FILE_MOVE_FAILED"
            },
            if copy {
                "Dosya kopyalanamadı."
            } else {
                "Dosya taşınamadı."
            },
            &source_error,
        )
    })?;
    if !target.exists() || (!copy && source.exists()) {
        return Err(tool_failure(
            "OUTCOME_UNKNOWN",
            "Dosya taşıma sonucu doğrulanamadı; otomatik tekrar yapılmadı.",
        ));
    }
    Ok(target)
}

pub(crate) fn transfer_op(ctx: &Context, value: &Value, copy: bool) -> Result<Done> {
    let request = operation(value)?;
    let source = fsx::resolve(&request.path, false);
    let requested = requested_target(&request);
    let requested_existed = requested.as_ref().map(|path| path.exists()).unwrap_or(true);
    let actual = transfer(ctx, &request, copy)?;
    let mut receipt = entry(&actual).ok_or_else(|| {
        tool_failure(
            "OUTCOME_UNKNOWN",
            "Dosya işleminin hedefi doğrulanamadı; otomatik tekrar yapılmadı.",
        )
    })?;
    let created_target = !requested_existed || requested.as_ref() != Some(&actual);
    if !receipt.is_directory && created_target {
        if copy {
            receipt.undo_id = undo::register(ctx, &actual.to_string_lossy(), None, None);
        } else if let Some(origin) = source.as_ref().filter(|origin| **origin != actual) {
            receipt.undo_id = undo::register(
                ctx,
                &actual.to_string_lossy(),
                None,
                Some(&origin.to_string_lossy()),
            );
            if receipt.undo_id.is_none() {
                // A move that cannot be undone is put back rather than left behind without a way home.
                move_across(&actual, origin).map_err(|source_error| fsx::local_file_error("FILE_UNDO_RECORD_FAILED", "Taşıma için geri alma kaydı oluşturulamadı; kaynak konuma dönüş doğrulanamadı.", &source_error))?;
                return Err(tool_failure(
                    "FILE_UNDO_RECORD_FAILED",
                    "Geri alma kaydı oluşturulamadı; taşıma geri alındı.",
                ));
            }
        }
    }
    if requested.as_ref().is_some_and(|path| *path != actual) {
        receipt.requested_path = requested.map(|path| file_path_text(&path));
    }
    Ok(Done::new(
        format!(
            "{} {}.",
            receipt.name,
            if copy { "kopyalandı" } else { "taşındı" }
        ),
        Some(data("files", vec![receipt])),
    ))
}

// ----- delete -----

/// Moves an item to the person's trash. macOS has no API that returns where the item went, so the item is moved into ~/.Trash directly (the way Finder does),
/// which also lets the undo record point at it; on the other platforms the system trash is used.
fn trash_item(path: &Path) -> std::result::Result<Option<PathBuf>, WireError> {
    #[cfg(target_os = "macos")]
    {
        let trash = fsx::home().join(".Trash");
        let _ = fs::create_dir_all(&trash);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "Öğe".into());
        let mut target = trash.join(&name);
        let mut counter = 2;
        while fs::symlink_metadata(&target).is_ok() {
            target = trash.join(format!("{name} {counter}"));
            counter += 1;
        }
        move_across(path, &target).map_err(|source| {
            fsx::local_file_error(
                "FILE_DELETE_FAILED",
                "Dosya Çöp Sepeti'ne taşınamadı.",
                &source,
            )
        })?;
        Ok(Some(target))
    }
    #[cfg(not(target_os = "macos"))]
    {
        trash::delete(path)
            .map_err(|_| tool_failure("FILE_DELETE_FAILED", "Dosya Çöp Sepeti'ne taşınamadı."))?;
        Ok(None)
    }
}

fn delete_file(ctx: &Context, value: &Value) -> Result<Done> {
    let request = operation(value)?;
    let target = fsx::resolve(&request.path, false)
        .filter(|path| path.exists())
        .ok_or_else(|| {
            fsx::missing_path_error(
                &request.path,
                &format!("Silinecek \"{}\" bulunamadı.", request.path),
            )
        })?;
    let home = fsx::home();
    let normalized = dunce::simplified(&target).to_path_buf();
    let users_root = Path::new("/Users");
    if normalized.parent().is_none()
        || normalized == home
        || normalized == users_root
        || normalized == Path::new("/home")
        || Some(normalized.as_path()) == home.parent()
    {
        return Err(permission(
            "PROTECTED_PATH",
            "Sistem veya kullanıcı kök klasörü silinemez.",
        ));
    }
    if is_symlink(&target) {
        return Err(permission(
            "SYMLINK_REJECTED",
            "Sembolik bağlantı silinmedi.",
        ));
    }
    if is_dir(&target) && !request.recursive {
        let empty = fs::read_dir(&target)
            .map_err(|source| {
                fsx::local_file_error(
                    "FILE_LIST_FAILED",
                    "Klasörün boş olduğu doğrulanamadı; silinmedi.",
                    &source,
                )
            })?
            .next()
            .is_none();
        if !empty {
            return Err(permission(
                "RECURSIVE_REQUIRED",
                "Dolu klasörü silmek için recursive=true açıkça belirtilmeli.",
            ));
        }
    }
    let regular = is_regular_file(&target);
    let original = file_path_text(&target);
    let trashed = trash_item(&target)?;
    let mut undo_id = None;
    if regular {
        if let Some(trashed_path) = trashed {
            undo_id = undo::register(ctx, &trashed_path.to_string_lossy(), None, Some(&original));
            if undo_id.is_none() {
                move_across(&trashed_path, &target).map_err(|_| {
                    tool_failure(
                        "FILE_UNDO_UNAVAILABLE",
                        "Geri alma kaydı oluşturulamadı; dosya özgün konumuna döndürülemedi.",
                    )
                })?;
                return Err(tool_failure(
                    "FILE_UNDO_UNAVAILABLE",
                    "Geri alma kaydı oluşturulamadı; dosya özgün konumuna döndürüldü.",
                ));
            }
        }
    }
    let item = Entry {
        path: original,
        name: target
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
        is_directory: !regular,
        undo_id,
        ..Entry::default()
    };
    Ok(Done::new(
        "Öğe Çöp Sepeti'ne taşındı.",
        Some(data("files", vec![item])),
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_tracked_copy_counts_every_byte_and_keeps_the_content() {
        let dir = std::env::temp_dir().join(format!("wiqo-copy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("in/alt")).unwrap();
        fs::write(dir.join("in/a.txt"), "12345").unwrap();
        fs::write(dir.join("in/alt/b.txt"), "678").unwrap();
        let mut counted = 0;
        copy_tracked(&dir.join("in"), &dir.join("out"), &mut |bytes| {
            counted += bytes
        })
        .unwrap();
        assert_eq!(counted, tree_bytes(&dir.join("in")));
        assert_eq!(counted, 8);
        assert_eq!(
            fs::read_to_string(dir.join("out/alt/b.txt")).unwrap(),
            "678"
        );
    }

    use super::*;
    use serde_json::json;

    fn scratch() -> (Context, PathBuf) {
        let dir = std::env::temp_dir().join(format!("wiqo-files-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        (Context::for_tests(), dunce::canonicalize(&dir).unwrap())
    }

    fn op(path: &Path, extra: Value) -> Value {
        let mut value = json!({ "path": path.to_string_lossy(), "overwrite": false, "recursive": false, "page": 0, "pageSize": 100 });
        if let (Some(target), Some(more)) = (value.as_object_mut(), extra.as_object()) {
            target.extend(more.clone());
        }
        json!({ "kind": "fileOperation", "value": value })
    }

    fn files_of(done: &Done) -> Vec<Value> {
        done.data.as_ref().unwrap()["value"]
            .as_array()
            .unwrap()
            .clone()
    }

    #[test]
    fn a_file_is_written_read_back_replaced_with_a_backup_and_undone() {
        let (ctx, dir) = scratch();
        let file = dir.join("not.txt");
        let done = write_file(&ctx, &op(&file, json!({ "content": "bir" }))).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "bir");
        let first_undo = files_of(&done)[0]["undoId"].as_str().map(str::to_string);
        assert!(first_undo.is_some());
        // A taken name is not a dead end: without `overwrite` both files stay and the receipt names the one that was made.
        let beside = write_file(&ctx, &op(&file, json!({ "content": "iki" }))).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "bir");
        assert_eq!(fs::read_to_string(dir.join("not (2).txt")).unwrap(), "iki");
        assert_eq!(files_of(&beside)[0]["name"], "not (2).txt");
        assert!(files_of(&beside)[0]["requestedPath"]
            .as_str()
            .is_some_and(|path| path.ends_with("/not.txt")));
        assert_eq!(
            write_file(
                &ctx,
                &op(&file, json!({ "content": "x", "collisionPolicy": "fail" }))
            )
            .unwrap_err()
            .code,
            "FILE_EXISTS"
        );
        assert_eq!(
            write_file(
                &ctx,
                &op(
                    &file,
                    json!({ "content": "x", "collisionPolicy": "replace" })
                )
            )
            .unwrap_err()
            .code,
            "OVERWRITE_REQUIRED"
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "bir");
        let second = write_file(
            &ctx,
            &op(&file, json!({ "content": "iki", "overwrite": true })),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "iki");
        assert!(ctx.services.data_dir.file("Backups").exists());
        let undo_id = files_of(&second)[0]["undoId"].as_str().unwrap().to_string();
        undo::undo_file(&ctx, &undo_id).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "bir");
        // The record is spent, and a file changed since is never overwritten.
        assert!(undo::undo_file(&ctx, &undo_id).is_err());
        let third = write_file(
            &ctx,
            &op(&file, json!({ "content": "üç", "overwrite": true })),
        )
        .unwrap();
        fs::write(&file, "elle değişti").unwrap();
        assert_eq!(
            undo::undo_file(&ctx, files_of(&third)[0]["undoId"].as_str().unwrap())
                .unwrap_err()
                .code,
            "FILE_UNDO_STALE"
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "elle değişti");
    }

    #[test]
    fn listing_sorts_naturally_hides_dotfiles_and_sensitive_names_and_pages() {
        let (ctx, dir) = scratch();
        for name in [
            "dosya10.txt",
            "dosya2.txt",
            "dosya1.txt",
            ".gizli",
            "sifre listesi.txt",
            "anahtar.pem",
        ] {
            fs::write(dir.join(name), "x").unwrap();
        }
        let listed = list_files(&ctx, &op(&dir, json!({}))).unwrap();
        let names: Vec<String> = files_of(&listed)
            .iter()
            .map(|item| item["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["dosya1.txt", "dosya2.txt", "dosya10.txt"]);
        let second_page = list_files(&ctx, &op(&dir, json!({ "page": 1, "pageSize": 2 }))).unwrap();
        assert_eq!(files_of(&second_page).len(), 1);
        assert_eq!(
            list_files(&ctx, &op(&dir.join("yok"), json!({})))
                .unwrap_err()
                .code,
            "FILE_NOT_FOUND"
        );
    }

    #[test]
    fn move_and_copy_rename_on_collision_and_move_can_be_undone() {
        let (ctx, dir) = scratch();
        fs::create_dir_all(dir.join("hedef")).unwrap();
        fs::write(dir.join("a.txt"), "A").unwrap();
        fs::write(dir.join("hedef/a.txt"), "eski").unwrap();
        let copied = transfer_op(
            &ctx,
            &op(
                &dir.join("a.txt"),
                json!({ "destination": dir.join("hedef").to_string_lossy() }),
            ),
            true,
        )
        .unwrap();
        let item = &files_of(&copied)[0];
        assert!(item["path"].as_str().unwrap().ends_with("a (2).txt"));
        assert_eq!(fs::read_to_string(dir.join("hedef/a.txt")).unwrap(), "eski");
        let moved = transfer_op(
            &ctx,
            &op(
                &dir.join("a.txt"),
                json!({ "destination": dir.join("tasindi.txt").to_string_lossy() }),
            ),
            false,
        )
        .unwrap();
        assert!(!dir.join("a.txt").exists() && dir.join("tasindi.txt").exists());
        undo::undo_file(&ctx, files_of(&moved)[0]["undoId"].as_str().unwrap()).unwrap();
        assert!(dir.join("a.txt").exists() && !dir.join("tasindi.txt").exists());
        assert_eq!(transfer_op(&ctx, &op(&dir.join("a.txt"), json!({ "destination": dir.join("hedef/a.txt").to_string_lossy(), "collisionPolicy": "fail" })), true).unwrap_err().code, "FILE_EXISTS");
        assert_eq!(
            transfer_op(
                &ctx,
                &op(
                    &dir.join("hedef"),
                    json!({ "destination": dir.join("x").to_string_lossy() })
                ),
                false
            )
            .unwrap_err()
            .code,
            "RECURSIVE_REQUIRED"
        );
    }

    #[test]
    fn reading_refuses_sensitive_names_and_large_files_and_returns_text_or_base64() {
        let (ctx, dir) = scratch();
        fs::write(dir.join("not.txt"), "merhaba").unwrap();
        fs::write(dir.join("ikili.bin"), [0u8, 159, 146, 150]).unwrap();
        fs::write(dir.join("big.txt"), vec![b'a'; 600_000]).unwrap();
        fs::write(dir.join("id_rsa"), "secret").unwrap();
        let text = read_file(&ctx, &op(&dir.join("not.txt"), json!({}))).unwrap();
        assert_eq!(text.data.unwrap()["value"]["text"], "merhaba");
        let binary = read_file(&ctx, &op(&dir.join("ikili.bin"), json!({}))).unwrap();
        assert!(binary.data.unwrap()["value"]["base64"].is_string());
        let head = read_file(&ctx, &op(&dir.join("big.txt"), json!({}))).unwrap();
        let head = &head.data.unwrap()["value"];
        assert_eq!(head["truncated"], true);
        assert_eq!(
            head["text"].as_str().unwrap().len(),
            MAX_LOCAL_READ_BYTES as usize
        );
        fs::write(dir.join("big.bin"), vec![0u8; 600_000]).unwrap();
        assert_eq!(
            read_file(&ctx, &op(&dir.join("big.bin"), json!({})))
                .unwrap_err()
                .code,
            "FILE_TOO_LARGE"
        );
        // Cut inside a letter, a long Turkish text still reads as UTF-8.
        fs::write(dir.join("uzun.txt"), "ş".repeat(300_000)).unwrap();
        let turkish = read_file(&ctx, &op(&dir.join("uzun.txt"), json!({}))).unwrap();
        assert!(turkish.data.unwrap()["value"]["text"]
            .as_str()
            .unwrap()
            .chars()
            .all(|letter| letter == 'ş'));
        assert_eq!(
            read_file(&ctx, &op(&dir.join("id_rsa"), json!({})))
                .unwrap_err()
                .code,
            "SENSITIVE_FILE_BLOCKED"
        );
        let missing = read_file(&ctx, &op(&dir.join("nott.txt"), json!({}))).unwrap_err();
        assert!(missing.message.contains("not.txt"), "{}", missing.message);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn deleting_moves_to_the_trash_and_can_be_undone() {
        let (ctx, dir) = scratch();
        let file = dir.join("silinecek.txt");
        fs::write(&file, "x").unwrap();
        let deleted = delete_file(&ctx, &op(&file, json!({}))).unwrap();
        assert!(!file.exists());
        undo::undo_file(&ctx, files_of(&deleted)[0]["undoId"].as_str().unwrap()).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "x");
        fs::create_dir_all(dir.join("dolu")).unwrap();
        fs::write(dir.join("dolu/a"), "x").unwrap();
        assert_eq!(
            delete_file(&ctx, &op(&dir.join("dolu"), json!({})))
                .unwrap_err()
                .code,
            "RECURSIVE_REQUIRED"
        );
        assert_eq!(
            delete_file(&ctx, &op(&fsx::home(), json!({})))
                .unwrap_err()
                .code,
            "PROTECTED_PATH"
        );
    }
}
