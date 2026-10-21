//! One-sentence chores on files (`fileUtility`): a new folder, a zip, merged or split PDFs, a smaller picture, duplicates, disk usage, a tidy folder, a batch
//! rename, documents moved as one all-or-nothing step. Every path goes through the same resolution as the other file commands (no symlink targets, Turkish
//! folder names translated), and nothing is overwritten: a taken name gets " 2".

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

use crate::fsx::{self, entry, is_dir, is_regular_file, is_symlink};
use crate::op::{data, error, payload, permission, tool_failure, Blocking, Done, Result};
use crate::process::run_with_timeout;
use crate::Op;

const REPORT_CHARS: usize = 20_000;
const DOCUMENT_TYPES: [&str; 8] = ["pdf", "doc", "docx", "rtf", "odt", "pages", "txt", "md"];

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
        }
    }
}

fn file_utility(_ctx: &Context, value: &Value) -> Result<Done> {
    let request: Request = payload(value, "fileUtility", "dosya işlemi")?;
    let limit = request.limit.clamp(1, 500);
    match request.action.as_str() {
        "inspect" => inspect(&request),
        "moveDocuments" => move_documents(&request, limit),
        "createFolder" => create_folder(&request),
        "archive" => archive(&request),
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
        "organize" => organize(&request, limit),
        "batchRename" => batch_rename(&request, limit),
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

/// The path itself when it is free, otherwise "name 2.ext", "name 3.ext", ...
pub(crate) fn free_path(path: &Path) -> PathBuf {
    if fs::symlink_metadata(path).is_err() {
        return path.to_path_buf();
    }
    let (stem, ext) = (
        stem_of(path),
        path.extension().map(|e| e.to_string_lossy().to_string()),
    );
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    for index in 2..1000 {
        let name = match &ext {
            Some(ext) => format!("{stem} {index}.{ext}"),
            None => format!("{stem} {index}"),
        };
        let candidate = parent.join(name);
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
    path.to_path_buf()
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

pub(crate) fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bayt", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bayt")
    } else {
        format!("{:.1} {}", value, UNITS[unit]).replace('.', ",")
    }
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

// ----- inspect / move / folders -----

fn inspect(request: &Request) -> Result<Done> {
    let raw = request.path.clone().unwrap_or_default();
    let path = fsx::resolve(&raw, true)
        .filter(|path| !is_symlink(path))
        .ok_or_else(|| {
            error(
                "INVALID_PATH",
                "Kontrol edilecek yol güvenli değil veya üst klasörüne erişilemiyor.",
                "validation",
            )
        })?;
    if fs::symlink_metadata(&path).is_err() {
        return Ok(Done::new("Yol bulunamadı.", Some(listing(&[]))));
    }
    let item =
        entry(&path).ok_or_else(|| fail("INSPECT_FAILED", "Dosya bilgisi doğrulanamadı."))?;
    let message = if item.is_directory {
        "Klasör bulundu."
    } else {
        "Dosya bulundu."
    };
    Ok(Done::new(message, Some(data("files", vec![item]))))
}

/// The documents directly inside a folder, moved to another folder as one step: either all of them fit (no name is taken) or nothing moves.
fn move_documents(request: &Request, limit: usize) -> Result<Done> {
    let invalid = || {
        fail(
            "INVALID_FOLDER",
            "Kaynak klasör ve hedefin üst klasörü var olmalı; hedef ayrı bir klasör olmalı.",
        )
    };
    let source = existing_folder(&request.path).map_err(|_| invalid())?;
    let destination = request
        .destination
        .as_deref()
        .and_then(|raw| fsx::resolve(raw, true))
        .filter(|path| !is_symlink(path) && (!path.exists() || is_dir(path)) && *path != source)
        .ok_or_else(invalid)?;
    let mut documents: Vec<PathBuf> = regular_files(&source, false, usize::MAX)
        .into_iter()
        .filter(|path| DOCUMENT_TYPES.contains(&extension_of(path).as_str()))
        .collect();
    documents.sort_by(|a, b| fsx::natural_cmp(&name_of(a), &name_of(b)));
    if documents.len() > limit {
        return Err(fail(
            "TOO_MANY_DOCUMENTS",
            format!(
                "{} belge bulundu; bu işlem en fazla {limit} belgeyi tek seferde taşıyabilir. Hiçbir dosya taşınmadı.",
                documents.len()
            ),
        ));
    }
    if documents.is_empty() {
        return Err(fail(
            "NO_MATCHING_FILES",
            "Kaynak klasörde taşınacak belge bulunamadı; hiçbir dosya taşınmadı.",
        ));
    }
    if documents
        .iter()
        .any(|document| fs::symlink_metadata(destination.join(name_of(document))).is_ok())
    {
        return Err(permission(
            "FILE_EXISTS",
            "Hedefte aynı adlı belge var; hiçbir dosya taşınmadı.",
        ));
    }
    if !destination.exists() {
        fs::create_dir(&destination).map_err(|_| {
            fail(
                "CREATE_FOLDER_FAILED",
                "Hedef klasör oluşturulamadı; hiçbir belge taşınmadı.",
            )
        })?;
    }
    let mut moved: Vec<PathBuf> = Vec::new();
    let total = documents.len();
    for document in &documents {
        let target = destination.join(name_of(document));
        let partial = |moved: &[PathBuf], message: String| -> Result<Done> {
            Ok(Done::partial(fail("PARTIAL_MOVE", message), listing(moved)))
        };
        if !is_regular_file(document) || fs::symlink_metadata(&target).is_ok() {
            return partial(
                &moved,
                format!("{}/{total} belge taşındı; kalanların durumu değişti. Otomatik tekrar yapılmadı.", moved.len()),
            );
        }
        if crate::files::move_across(document, &target).is_err() {
            return partial(
                &moved,
                format!(
                    "{}/{total} belge taşındı; kalanlar taşınamadı. Otomatik tekrar yapılmadı.",
                    moved.len()
                ),
            );
        }
        if !is_regular_file(&target) || document.exists() {
            return Ok(Done::partial(
                fail(
                    "OUTCOME_UNKNOWN",
                    "Dosya taşıma sonucu doğrulanamadı; otomatik tekrar yapılmadı.",
                ),
                listing(&moved),
            ));
        }
        moved.push(target);
    }
    Ok(Done::new(
        format!(
            "{} belge {} klasörüne taşındı ve doğrulandı.",
            moved.len(),
            name_of(&destination)
        ),
        Some(listing(&moved)),
    ))
}

fn create_folder(request: &Request) -> Result<Done> {
    let raw = request.path.clone().unwrap_or_default();
    let folder = fsx::resolve(&raw, true).ok_or_else(|| {
        fail(
            "INVALID_PATH",
            "Klasörün açılacağı yer yok ya da yol geçersiz; üst klasörü kontrol et.",
        )
    })?;
    if fs::symlink_metadata(&folder).is_ok() {
        if is_dir(&folder) {
            return Ok(Done::new(
                format!("{} klasörü zaten var.", name_of(&folder)),
                Some(listing(&[folder])),
            ));
        }
        return Err(fail("ALREADY_EXISTS", "Bu adla bir dosya var."));
    }
    fs::create_dir(&folder).map_err(|source| {
        fsx::local_file_error("CREATE_FOLDER_FAILED", "Klasör oluşturulamadı.", &source)
    })?;
    if !is_dir(&folder) {
        return Err(fail(
            "POSTCONDITION_FAILED",
            "Klasör oluşturulduğu doğrulanamadı.",
        ));
    }
    Ok(Done::new(
        format!("{} klasörü oluşturuldu.", name_of(&folder)),
        Some(listing(&[folder])),
    ))
}

// ----- zip -----

fn sources(request: &Request) -> Result<Vec<PathBuf>> {
    let mut raws: Vec<Option<String>> = request
        .paths
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(Some)
        .collect();
    if request.path.is_some() {
        raws.push(request.path.clone());
    }
    raws.iter().map(existing).collect()
}

fn archive(request: &Request) -> Result<Done> {
    let sources = sources(request)?;
    let first = sources
        .first()
        .ok_or_else(|| error("INVALID_PAYLOAD", "Arşivlenecek dosya yok.", "validation"))?;
    let name = if sources.len() == 1 {
        stem_of(first)
    } else {
        "Arşiv".into()
    };
    let parent = first.parent().map(Path::to_path_buf).unwrap_or_default();
    let output = target(&request.destination, &parent.join(format!("{name}.zip")))?;
    let failed = || fail("ARCHIVE_FAILED", "Zip oluşturulamadı.");
    let temporary = output.with_file_name(format!(".{}.wiqo-partial", name_of(&output)));
    let written = (|| -> std::io::Result<()> {
        let mut zip = zip::ZipWriter::new(fs::File::create(&temporary)?);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        // One source keeps its own name at the top; several go under the archive's name, as Finder does.
        let prefix = if sources.len() == 1 {
            String::new()
        } else {
            format!("{name}/")
        };
        for source in &sources {
            let base = source.parent().map(Path::to_path_buf).unwrap_or_default();
            for item in walkdir::WalkDir::new(source).follow_links(false) {
                let item = item.map_err(std::io::Error::other)?;
                if item.file_type().is_symlink() || name_of(item.path()).starts_with(".DS_Store") {
                    continue;
                }
                let relative = item.path().strip_prefix(&base).unwrap_or(item.path());
                let inner = format!("{prefix}{}", relative.to_string_lossy().replace('\\', "/"));
                if item.file_type().is_dir() {
                    zip.add_directory(format!("{inner}/"), options)?;
                } else {
                    zip.start_file(inner, options)?;
                    std::io::copy(&mut fs::File::open(item.path())?, &mut zip)?;
                }
            }
        }
        zip.finish()?.flush()
    })();
    if written.is_err() || fs::rename(&temporary, &output).is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(failed());
    }
    Ok(Done::new(
        format!(
            "{} hazır ({} öğe, {}).",
            name_of(&output),
            sources.len(),
            human(size(&output))
        ),
        Some(listing(&[output])),
    ))
}

fn extract(request: &Request) -> Result<Done> {
    let archive = existing(&request.path)?;
    let parent = archive.parent().map(Path::to_path_buf).unwrap_or_default();
    let folder = target(&request.destination, &parent.join(stem_of(&archive)))?;
    let failed = || fail("EXTRACT_FAILED", "Arşiv açılamadı; zip dosyası mı?");
    let file = fs::File::open(&archive).map_err(|_| failed())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| failed())?;
    fs::create_dir_all(&folder).map_err(|_| failed())?;
    for index in 0..zip.len() {
        let mut item = zip.by_index(index).map_err(|_| failed())?;
        // `enclosed_name` refuses "../" and absolute names, so nothing lands outside the folder.
        let Some(relative) = item.enclosed_name() else {
            continue;
        };
        if relative.starts_with("__MACOSX") {
            continue;
        }
        let path = folder.join(relative);
        if item.is_dir() {
            fs::create_dir_all(&path).map_err(|_| failed())?;
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|_| failed())?;
        }
        let mut bytes = Vec::new();
        item.read_to_end(&mut bytes).map_err(|_| failed())?;
        fs::write(&path, bytes).map_err(|_| failed())?;
    }
    Ok(Done::new(
        format!("Arşiv {} klasörüne açıldı.", name_of(&folder)),
        Some(listing(&[folder])),
    ))
}

// ----- PDF -----

fn load_pdf(path: &Path) -> Result<lopdf::Document> {
    lopdf::Document::load(path).map_err(|_| {
        fail(
            "NOT_A_PDF",
            format!("{} PDF olarak açılamadı.", name_of(path)),
        )
    })
}

/// Pages of several PDFs, in order, as one document (the lopdf merge recipe: renumber each, keep one catalog and one page tree).
fn merged(documents: Vec<lopdf::Document>) -> Option<lopdf::Document> {
    use lopdf::{Document, Object, ObjectId};
    let mut max_id = 1;
    let mut pages: Vec<(ObjectId, Object)> = Vec::new();
    let mut objects: BTreeMap<ObjectId, Object> = BTreeMap::new();
    for mut document in documents {
        document.renumber_objects_with(max_id);
        max_id = document.max_id + 1;
        for (_, id) in document.get_pages() {
            pages.push((id, document.get_object(id).ok()?.clone()));
        }
        objects.extend(document.objects);
    }
    let mut output = Document::with_version("1.5");
    let mut catalog: Option<(ObjectId, Object)> = None;
    let mut tree: Option<(ObjectId, Object)> = None;
    for (id, object) in objects {
        match object.type_name().unwrap_or("") {
            "Catalog" => catalog = Some((catalog.map(|(first, _)| first).unwrap_or(id), object)),
            "Pages" => {
                if let Ok(dictionary) = object.as_dict() {
                    let mut dictionary = dictionary.clone();
                    if let Some((_, Object::Dictionary(previous))) = &tree {
                        dictionary.extend(previous);
                    }
                    tree = Some((
                        tree.map(|(first, _)| first).unwrap_or(id),
                        Object::Dictionary(dictionary),
                    ));
                }
            }
            "Page" | "Outlines" | "Outline" => {}
            _ => {
                output.objects.insert(id, object);
            }
        }
    }
    let (tree_id, tree_object) = tree?;
    let (catalog_id, catalog_object) = catalog?;
    for (id, page) in &pages {
        let mut dictionary = page.as_dict().ok()?.clone();
        dictionary.set("Parent", tree_id);
        output.objects.insert(*id, Object::Dictionary(dictionary));
    }
    let mut dictionary = tree_object.as_dict().ok()?.clone();
    dictionary.set("Count", pages.len() as u32);
    dictionary.set(
        "Kids",
        pages
            .iter()
            .map(|(id, _)| Object::Reference(*id))
            .collect::<Vec<_>>(),
    );
    output
        .objects
        .insert(tree_id, Object::Dictionary(dictionary));
    let mut dictionary = catalog_object.as_dict().ok()?.clone();
    dictionary.set("Pages", tree_id);
    dictionary.remove(b"Outlines");
    output
        .objects
        .insert(catalog_id, Object::Dictionary(dictionary));
    output.trailer.set("Root", catalog_id);
    output.max_id = output.objects.keys().map(|(id, _)| *id).max().unwrap_or(1);
    output.renumber_objects();
    output.compress();
    Some(output)
}

fn save_pdf(document: &mut lopdf::Document, path: &Path) -> bool {
    let temporary = path.with_file_name(format!(".{}.wiqo-partial", name_of(path)));
    let saved = document.save(&temporary).is_ok() && fs::rename(&temporary, path).is_ok();
    if !saved {
        let _ = fs::remove_file(&temporary);
    }
    saved
}

fn merge_pdf(request: &Request) -> Result<Done> {
    let sources: Vec<PathBuf> = request
        .paths
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|raw| existing(&Some(raw)))
        .collect::<Result<_>>()?;
    if sources.len() < 2 {
        return Err(error(
            "INVALID_PAYLOAD",
            "Birleştirmek için en az iki PDF gerekir.",
            "validation",
        ));
    }
    let documents = sources
        .iter()
        .map(|path| load_pdf(path))
        .collect::<Result<Vec<_>>>()?;
    let parent = sources[0]
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let output = target(&request.destination, &parent.join("Birleşik.pdf"))?;
    let mut document =
        merged(documents).ok_or_else(|| fail("MERGE_FAILED", "PDF'ler birleştirilemedi."))?;
    let count = document.get_pages().len();
    if count == 0 || !save_pdf(&mut document, &output) {
        return Err(fail("MERGE_FAILED", "PDF'ler birleştirilemedi."));
    }
    Ok(Done::new(
        format!(
            "{} PDF birleştirildi: {} ({count} sayfa).",
            sources.len(),
            name_of(&output)
        ),
        Some(listing(&[output])),
    ))
}

fn split_pdf(request: &Request, limit: usize) -> Result<Done> {
    let source = existing(&request.path)?;
    let document = load_pdf(&source)?;
    let stem = stem_of(&source);
    let parent = source.parent().map(Path::to_path_buf).unwrap_or_default();
    let folder = target(
        &request.destination,
        &parent.join(format!("{stem} sayfalar")),
    )?;
    fs::create_dir_all(&folder)
        .map_err(|_| fail("SPLIT_FAILED", "Sayfalar için klasör açılamadı."))?;
    let total = document.get_pages().len() as u32;
    let mut outputs = 0;
    for page in 1..=total.min(limit as u32) {
        let mut single = document.clone();
        let others: Vec<u32> = (1..=total).filter(|other| *other != page).collect();
        single.delete_pages(&others);
        single.prune_objects();
        single.compress();
        if save_pdf(
            &mut single,
            &folder.join(format!("{stem}-sayfa-{page}.pdf")),
        ) {
            outputs += 1;
        }
    }
    if outputs == 0 {
        return Err(fail("SPLIT_FAILED", "PDF sayfalara ayrılamadı."));
    }
    Ok(Done::new(
        format!("{outputs} sayfa ayrı PDF oldu: {}.", name_of(&folder)),
        Some(listing(&[folder])),
    ))
}

// ----- images -----

fn compress_image(request: &Request) -> Result<Done> {
    let source = existing(&request.path)?;
    let edge: u32 = request
        .mode
        .as_deref()
        .and_then(|mode| mode.parse().ok())
        .unwrap_or(1600u32)
        .clamp(200, 8000);
    let parent = source.parent().map(Path::to_path_buf).unwrap_or_default();
    let output = target(
        &request.destination,
        &parent.join(format!("{}-küçük.jpg", stem_of(&source))),
    )?;
    let failed = || {
        fail(
            "COMPRESS_FAILED",
            "Görsel küçültülemedi; bir görsel dosyası mı?",
        )
    };
    let encoded = image::open(&source).ok().and_then(|picture| {
        let picture = if picture.width().max(picture.height()) > edge {
            picture.resize(edge, edge, image::imageops::FilterType::Lanczos3)
        } else {
            picture
        };
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 75)
            .encode_image(&picture.to_rgb8())
            .ok()?;
        Some(bytes)
    });
    match encoded {
        Some(bytes) => crate::files::write_atomic(&output, &bytes).map_err(|_| failed())?,
        // HEIC and the other formats the image library cannot read: the system's own converter.
        None if cfg!(target_os = "macos") => {
            let edge = edge.to_string();
            let out = run(
                "/usr/bin/sips",
                &[
                    "-Z",
                    &edge,
                    "-s",
                    "format",
                    "jpeg",
                    "-s",
                    "formatOptions",
                    "75",
                    &source.to_string_lossy(),
                    "--out",
                    &output.to_string_lossy(),
                ],
                60,
            );
            if !succeeded(&out) || !output.exists() {
                return Err(failed());
            }
        }
        None => return Err(failed()),
    }
    Ok(Done::new(
        format!(
            "{}: {} → {}.",
            name_of(&output),
            human(size(&source)),
            human(size(&output))
        ),
        Some(listing(&[output])),
    ))
}

// ----- reports -----

fn find_duplicates(request: &Request, limit: usize) -> Result<Done> {
    let folder = existing_folder(&request.path)?;
    let files = regular_files(&folder, true, 5_000);
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for file in &files {
        let bytes = size(file);
        if bytes > 0 {
            by_size.entry(bytes).or_default().push(file.clone());
        }
    }
    let mut groups: Vec<Vec<PathBuf>> = Vec::new();
    for (_, same) in by_size.into_iter().filter(|(_, same)| same.len() > 1) {
        let mut by_hash: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for file in same {
            let Ok(mut handle) = fs::File::open(&file) else {
                continue;
            };
            let mut hasher = Sha256::new();
            if std::io::copy(&mut handle, &mut hasher).is_ok() {
                by_hash
                    .entry(hex::encode(hasher.finalize()))
                    .or_default()
                    .push(file);
            }
        }
        groups.extend(by_hash.into_values().filter(|group| group.len() > 1));
    }
    groups.sort_by_key(|group| std::cmp::Reverse(size(&group[0]) * group.len() as u64));
    if groups.is_empty() {
        return Ok(Done::new(
            "Kopya dosya yok.",
            Some(report(
                &fsx::abbreviate(&folder),
                &format!("{} dosyada kopya yok.", files.len()),
            )),
        ));
    }
    let lines: Vec<String> = groups
        .iter()
        .take(limit)
        .enumerate()
        .map(|(index, group)| {
            format!(
                "{}. {} × {}: {}",
                index + 1,
                human(size(&group[0])),
                group.len(),
                group
                    .iter()
                    .map(|path| short(path))
                    .collect::<Vec<_>>()
                    .join(" | ")
            )
        })
        .collect();
    let wasted: u64 = groups
        .iter()
        .map(|group| size(&group[0]) * (group.len() as u64 - 1))
        .sum();
    Ok(Done::new(
        format!(
            "{} kopya grubu bulundu; silinirse {} boşalır.",
            groups.len(),
            human(wasted)
        ),
        Some(report(&fsx::abbreviate(&folder), &lines.join("\n"))),
    ))
}

fn disk_usage(request: &Request, limit: usize) -> Result<Done> {
    let folder = existing_folder(&request.path)?;
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let disk = disks
        .list()
        .iter()
        .filter(|disk| folder.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len());
    let (free, total) = disk
        .map(|disk| (human(disk.available_space()), human(disk.total_space())))
        .unwrap_or_else(|| ("?".into(), "?".into()));
    let files = regular_files(&folder, true, 20_000);
    let mut largest: Vec<&PathBuf> = files.iter().collect();
    largest.sort_by_key(|path| std::cmp::Reverse(size(path)));
    let mut folders: HashMap<String, u64> = HashMap::new();
    for file in &files {
        if let Ok(relative) = file.strip_prefix(&folder) {
            let mut parts = relative.components();
            if let (Some(top), Some(_)) = (parts.next(), parts.next()) {
                *folders
                    .entry(top.as_os_str().to_string_lossy().to_string())
                    .or_default() += size(file);
            }
        }
    }
    let mut folders: Vec<(String, u64)> = folders.into_iter().collect();
    folders.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
    let total_bytes: u64 = files.iter().map(|path| size(path)).sum();
    let mut lines = vec![
        format!("Disk: {free} boş / {total}"),
        format!("{} toplam: {}", short(&folder), human(total_bytes)),
        format!(
            "En büyük klasörler: {}",
            folders
                .iter()
                .take(8)
                .map(|(name, bytes)| format!("{name}: {}", human(*bytes)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "En büyük dosyalar:".into(),
    ];
    lines.extend(
        largest
            .iter()
            .take(limit)
            .map(|path| format!("- {} {}", short(path), human(size(path)))),
    );
    Ok(Done::new(
        format!("Disk: {free} boş."),
        Some(report(&fsx::abbreviate(&folder), &lines.join("\n"))),
    ))
}

fn recent_files(limit: usize) -> Result<Done> {
    let home = fsx::home();
    let mut files: Vec<PathBuf> = [
        dirs::desktop_dir(),
        dirs::document_dir(),
        dirs::download_dir(),
    ]
    .into_iter()
    .flatten()
    .chain(["Desktop", "Documents", "Downloads"].map(|name| home.join(name)))
    .collect::<std::collections::BTreeSet<_>>()
    .into_iter()
    .flat_map(|folder| regular_files(&folder, true, 3_000))
    .collect();
    files.sort_by_key(|path| std::cmp::Reverse(modified(path)));
    files.dedup();
    files.truncate(limit);
    Ok(Done::new(
        format!("Son değişen {} dosya.", files.len()),
        Some(listing(&files)),
    ))
}

// ----- trash, finder, printing -----

fn restore_from_trash(request: &Request) -> Result<Done> {
    let wanted = normalize(request.name.as_deref().unwrap_or(""));
    #[cfg(target_os = "macos")]
    {
        let trash = fsx::home().join(".Trash");
        let items: Vec<PathBuf> = fs::read_dir(&trash)
            .map_err(|source| {
                fsx::local_file_error("TRASH_UNREADABLE", "Çöp kutusu okunamadı.", &source)
            })?
            .filter_map(|item| item.ok().map(|item| item.path()))
            .filter(|path| !name_of(path).starts_with('.'))
            .collect();
        let best = items
            .iter()
            .map(|path| (path, score(&stem_of(path), &wanted)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|(_, rank)| *rank >= 0.6)
            .map(|(path, _)| path.clone());
        let Some(best) = best else {
            let mut recent = items.clone();
            recent.sort_by_key(|path| std::cmp::Reverse(modified(path)));
            let names: Vec<String> = recent.iter().take(5).map(|path| name_of(path)).collect();
            return Err(fail(
                "FILE_NOT_FOUND",
                format!(
                    "Çöpte \"{}\" yok. Son atılanlar: {}.",
                    request.name.as_deref().unwrap_or(""),
                    if names.is_empty() {
                        "çöp boş".into()
                    } else {
                        names.join(", ")
                    }
                ),
            ));
        };
        let desktop = fsx::desktop_dir().unwrap_or_else(fsx::home);
        let output = target(&request.destination, &desktop.join(name_of(&best)))?;
        crate::files::move_across(&best, &output).map_err(|source| {
            fsx::local_file_error("RESTORE_FAILED", "Dosya çöpten geri getirilemedi.", &source)
        })?;
        let folder = output.parent().map(short).unwrap_or_default();
        Ok(Done::new(
            format!(
                "{} çöpten {folder} klasörüne geri getirildi.",
                name_of(&output)
            ),
            Some(listing(&[output])),
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let items = trash::os_limited::list()
            .map_err(|_| fail("TRASH_UNREADABLE", "Çöp kutusu okunamadı."))?;
        let best = items
            .iter()
            .map(|item| {
                let name = item.name.to_string_lossy().to_string();
                let stem = stem_of(Path::new(&name));
                (item, score(&stem, &wanted))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|(_, rank)| *rank >= 0.6)
            .map(|(item, _)| item.clone());
        let Some(best) = best else {
            return Err(fail(
                "FILE_NOT_FOUND",
                format!("Çöpte \"{}\" yok.", request.name.as_deref().unwrap_or("")),
            ));
        };
        let output = best.original_path();
        trash::os_limited::restore_all([best]).map_err(|_| {
            fail(
                "RESTORE_FAILED",
                "Dosya çöpten geri getirilemedi (aynı adlı dosya olabilir).",
            )
        })?;
        Ok(Done::new(
            format!("{} eski yerine geri getirildi.", name_of(&output)),
            Some(listing(&[output])),
        ))
    }
}

fn reveal(request: &Request) -> Result<Done> {
    let path = existing(&request.path)?;
    let text = path.to_string_lossy().to_string();
    let shown = if cfg!(target_os = "macos") {
        succeeded(&run("/usr/bin/open", &["-R", &text], 15))
    } else if cfg!(windows) {
        // Explorer returns 1 even when it worked, so only "could it start" counts.
        run("explorer", &[&format!("/select,{text}")], 15).is_some()
    } else {
        let folder = path.parent().unwrap_or(&path).to_string_lossy().to_string();
        open::that_detached(folder).is_ok()
    };
    if !shown {
        return Err(fail("REVEAL_FAILED", "Dosya gösterilemedi."));
    }
    let manager = if cfg!(target_os = "macos") {
        "Finder'da"
    } else {
        "dosya yöneticisinde"
    };
    Ok(Done::new(
        format!("{} {manager} gösterildi.", name_of(&path)),
        Some(listing(&[path])),
    ))
}

fn open_with(request: &Request) -> Result<Done> {
    let path = existing(&request.path)?;
    let app = request.name.clone().unwrap_or_default();
    if app.trim().is_empty() {
        return Err(error(
            "INVALID_PAYLOAD",
            "Hangi uygulamayla açılacağı yazılmamış.",
            "validation",
        ));
    }
    let opened = if cfg!(target_os = "macos") {
        succeeded(&run(
            "/usr/bin/open",
            &["-a", &app, &path.to_string_lossy()],
            20,
        ))
    } else {
        open::with_detached(&path, &app).is_ok()
    };
    if !opened {
        return Err(fail(
            "APPLICATION_NOT_FOUND",
            format!("\"{app}\" uygulaması bulunamadı ya da dosyayı açamadı."),
        ));
    }
    Ok(Done::new(
        format!("{} {app} ile açıldı.", name_of(&path)),
        Some(listing(&[path])),
    ))
}

fn print(request: &Request) -> Result<Done> {
    let path = existing(&request.path)?;
    let text = path.to_string_lossy().to_string();
    let out = if cfg!(windows) {
        run(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                &format!(
                    "Start-Process -FilePath '{}' -Verb Print",
                    text.replace('\'', "''")
                ),
            ],
            30,
        )
    } else {
        run("lp", &[&text], 30)
    };
    if !succeeded(&out) {
        let detail = out
            .map(|out| {
                String::from_utf8_lossy(&out.stderr)
                    .chars()
                    .take(160)
                    .collect::<String>()
            })
            .filter(|text| !text.trim().is_empty());
        return Err(fail(
            "PRINT_FAILED",
            match detail {
                Some(detail) => format!("Yazdırılamadı: {detail}"),
                None => "Yazdırılamadı; varsayılan yazıcı tanımlı mı?".into(),
            },
        ));
    }
    Ok(Done::new(
        format!("{} yazıcıya gönderildi.", name_of(&path)),
        Some(listing(&[path])),
    ))
}

/// Text from a scan or a picture. A PDF with a text layer is read as text; a picture needs the system's text recognition, which this build reaches
/// through `tesseract` when it is installed.
fn ocr(request: &Request) -> Result<Done> {
    let path = existing(&request.path)?;
    if extension_of(&path) == "pdf" {
        if let Ok(text) = crate::documents::extract_text(&path) {
            if !text.trim().is_empty() {
                return Ok(Done::new(
                    format!("{} okundu.", name_of(&path)),
                    Some(report(&path.to_string_lossy(), &text)),
                ));
            }
        }
    }
    let out = run(
        "tesseract",
        &[&path.to_string_lossy(), "stdout", "-l", "tur+eng"],
        120,
    )
    .filter(|out| out.status.success());
    let Some(out) = out else {
        return Err(fail(
            "OCR_UNSUPPORTED",
            "Görseldeki yazıyı okumak için bu bilgisayarda metin tanıma (tesseract) kurulu değil; görseli telefona gönderip orada okutabilirim.",
        ));
    };
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if text.trim().is_empty() {
        return Err(fail("OCR_EMPTY", "Görselde okunabilir yazı bulunamadı."));
    }
    Ok(Done::new(
        format!("{} okundu.", name_of(&path)),
        Some(report(&path.to_string_lossy(), &text)),
    ))
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

// ----- tidy -----

fn bucket_for(extension: &str) -> &'static str {
    const GROUPS: [(&str, &[&str]); 8] = [
        (
            "Belgeler",
            &["doc", "docx", "pages", "txt", "md", "rtf", "odt"],
        ),
        ("PDF", &["pdf"]),
        ("Tablolar", &["xls", "xlsx", "csv", "numbers", "ods"]),
        ("Sunumlar", &["ppt", "pptx", "key", "odp"]),
        (
            "Görseller",
            &["png", "jpg", "jpeg", "heic", "gif", "webp", "svg", "tiff"],
        ),
        ("Videolar", &["mov", "mp4", "m4v", "avi", "mkv"]),
        ("Sesler", &["mp3", "m4a", "wav", "aiff", "flac"]),
        ("Arşivler", &["zip", "rar", "7z", "dmg", "tar", "gz"]),
    ];
    GROUPS
        .iter()
        .find(|(_, extensions)| extensions.contains(&extension))
        .map(|(name, _)| *name)
        .unwrap_or("Diğer")
}

fn organize(request: &Request, limit: usize) -> Result<Done> {
    let folder = existing_folder(&request.path)?;
    let by_month = request.mode.as_deref() == Some("month");
    let mut moved: BTreeMap<String, usize> = BTreeMap::new();
    for file in regular_files(&folder, false, limit) {
        let bucket = if by_month {
            date(modified(&file), "%Y-%m")
        } else {
            bucket_for(&extension_of(&file)).to_string()
        };
        let destination = folder.join(&bucket);
        if fs::create_dir_all(&destination).is_err() {
            continue;
        }
        if crate::files::move_across(&file, &free_path(&destination.join(name_of(&file)))).is_ok() {
            *moved.entry(bucket).or_default() += 1;
        }
    }
    if moved.is_empty() {
        return Err(fail(
            "NOTHING_TO_ORGANIZE",
            "Düzenlenecek dosya yok (alt klasörlerdekilere dokunulmaz).",
        ));
    }
    let mut counts: Vec<(&String, &usize)> = moved.iter().collect();
    counts.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    let summary = counts
        .iter()
        .map(|(name, count)| format!("{name}: {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let folders: Vec<PathBuf> = moved.keys().map(|name| folder.join(name)).collect();
    Ok(Done::new(
        format!(
            "{} dosya klasörlere ayrıldı: {summary}.",
            moved.values().sum::<usize>()
        ),
        Some(listing(&folders)),
    ))
}

fn batch_rename(request: &Request, limit: usize) -> Result<Done> {
    let folder = existing_folder(&request.path)?;
    let pattern = request
        .name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| {
            error(
                "INVALID_PAYLOAD",
                "Ad kalıbı yazılmamış (ör. \"Fatura {n}\").",
                "validation",
            )
        })?;
    let mut files = regular_files(&folder, false, limit);
    files.sort_by_key(|path| modified(path));
    let width = files.len().to_string().len();
    let mut renamed: Vec<PathBuf> = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let mut base = pattern
            .replace("{n}", &format!("{:0width$}", index + 1))
            .replace("{name}", &stem_of(file))
            .replace("{date}", &date(modified(file), "%Y-%m-%d"));
        // A pattern without a counter still needs a different name per file.
        if !pattern.contains("{n}") && !pattern.contains("{name}") && files.len() > 1 {
            base = format!("{base} {:0width$}", index + 1);
        }
        if !is_valid_file_name(&base) {
            return Err(error(
                "INVALID_NAME",
                "Ad kalıbı geçerli bir dosya adı üretmiyor.",
                "validation",
            ));
        }
        let ext = file.extension().map(|e| e.to_string_lossy().to_string());
        let name = match ext {
            Some(ext) if !ext.is_empty() => format!("{base}.{ext}"),
            _ => base,
        };
        let destination = free_path(&folder.join(name));
        if fs::rename(file, &destination).is_ok() {
            renamed.push(destination);
        }
    }
    if renamed.is_empty() {
        return Err(fail("RENAME_FAILED", "Yeniden adlandırılacak dosya yok."));
    }
    let shown: Vec<PathBuf> = renamed.iter().take(20).cloned().collect();
    Ok(Done::new(
        format!("{} dosya yeniden adlandırıldı.", renamed.len()),
        Some(listing(&shown)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wiqo-utility-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dunce::canonicalize(dir).unwrap()
    }

    fn call(value: Value) -> Result<Done> {
        file_utility(
            &Context::for_tests(),
            &json!({ "kind": "fileUtility", "value": value }),
        )
    }

    #[test]
    fn documents_move_all_or_nothing() {
        let dir = temp();
        let source = dir.join("in");
        fs::create_dir(&source).unwrap();
        for name in ["a.pdf", "b.docx", "c.png"] {
            fs::write(source.join(name), b"x").unwrap();
        }
        let destination = dir.join("out");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("a.pdf"), b"taken").unwrap();
        let path = source.to_string_lossy().to_string();
        let target = destination.to_string_lossy().to_string();
        let refused =
            call(json!({ "action": "moveDocuments", "path": path, "destination": target }))
                .unwrap_err();
        assert_eq!(refused.code, "FILE_EXISTS");
        assert!(
            source.join("b.docx").exists(),
            "nothing moves when one name is taken"
        );
        fs::remove_file(destination.join("a.pdf")).unwrap();
        let done = call(json!({ "action": "moveDocuments", "path": path, "destination": target }))
            .unwrap();
        assert!(done.message.starts_with("2 belge"));
        assert!(source.join("c.png").exists() && destination.join("b.docx").exists());
    }

    #[test]
    fn zip_round_trip_and_names_never_overwrite() {
        let dir = temp();
        let folder = dir.join("rapor");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("not.txt"), b"merhaba").unwrap();
        let path = folder.to_string_lossy().to_string();
        call(json!({ "action": "archive", "path": path })).unwrap();
        call(json!({ "action": "archive", "path": path })).unwrap();
        assert!(dir.join("rapor.zip").exists() && dir.join("rapor 2.zip").exists());
        let zip = dir.join("rapor.zip").to_string_lossy().to_string();
        let target = dir.join("açılan").to_string_lossy().to_string();
        call(json!({ "action": "extract", "path": zip, "destination": target })).unwrap();
        assert_eq!(
            fs::read(dir.join("açılan/rapor/not.txt")).unwrap(),
            b"merhaba"
        );
    }

    #[test]
    fn pdfs_merge_and_split() {
        use lopdf::content::{Content, Operation};
        use lopdf::{dictionary, Document, Object, Stream};
        fn pdf(path: &Path, pages: usize) {
            let mut doc = Document::with_version("1.5");
            let tree = doc.new_object_id();
            let mut kids = Vec::new();
            for _ in 0..pages {
                let content = Content {
                    operations: vec![Operation::new("BT", vec![]), Operation::new("ET", vec![])],
                };
                let stream = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
                kids.push(Object::from(doc.add_object(dictionary! {
                    "Type" => "Page", "Parent" => tree, "Contents" => stream,
                    "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
                })));
            }
            doc.objects.insert(
                tree,
                Object::Dictionary(
                    dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => pages as u32 },
                ),
            );
            let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => tree });
            doc.trailer.set("Root", catalog);
            doc.save(path).unwrap();
        }
        let dir = temp();
        pdf(&dir.join("a.pdf"), 2);
        pdf(&dir.join("b.pdf"), 3);
        let paths = [dir.join("a.pdf"), dir.join("b.pdf")].map(|p| p.to_string_lossy().to_string());
        let done = call(json!({ "action": "mergePDF", "paths": paths })).unwrap();
        assert!(done.message.contains("5 sayfa"), "{}", done.message);
        let merged = dir.join("Birleşik.pdf").to_string_lossy().to_string();
        let done = call(json!({ "action": "splitPDF", "path": merged })).unwrap();
        assert!(done.message.starts_with("5 sayfa"), "{}", done.message);
        assert_eq!(
            Document::load(dir.join("Birleşik sayfalar/Birleşik-sayfa-4.pdf"))
                .unwrap()
                .get_pages()
                .len(),
            1
        );
    }

    #[test]
    fn organize_and_rename_keep_every_file() {
        let dir = temp();
        for name in ["x.pdf", "y.jpg", "z.unknown"] {
            fs::write(dir.join(name), b"x").unwrap();
        }
        let path = dir.to_string_lossy().to_string();
        call(json!({ "action": "organize", "path": path })).unwrap();
        assert!(
            dir.join("PDF/x.pdf").exists()
                && dir.join("Görseller/y.jpg").exists()
                && dir.join("Diğer/z.unknown").exists()
        );
        let pdfs = dir.join("PDF").to_string_lossy().to_string();
        call(json!({ "action": "batchRename", "path": pdfs, "name": "Fatura {n}" })).unwrap();
        assert!(dir.join("PDF/Fatura 1.pdf").exists());
        assert_eq!(human(1_500_000), "1,5 MB");
    }
}
