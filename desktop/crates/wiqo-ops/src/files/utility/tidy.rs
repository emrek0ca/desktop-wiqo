//! Tidying a folder: sorting files into folders by kind, and renaming many at once.

use super::*;

// ----- tidy -----

pub(super) fn bucket_for(extension: &str) -> &'static str {
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

pub(super) fn organize(ctx: &Context, request: &Request, limit: usize) -> Result<Done> {
    let folder = existing_folder(&request.path)?;
    let by_month = request.mode.as_deref() == Some("month");
    let mut moved: BTreeMap<String, usize> = BTreeMap::new();
    let (mut moves, mut made) = (Vec::new(), Vec::new());
    if request.preview {
        let mut planned = Vec::new();
        let mut new_folders = std::collections::BTreeSet::new();
        for file in regular_files(&folder, false, limit) {
            let bucket = if by_month { date(modified(&file), "%Y-%m") } else { bucket_for(&extension_of(&file)).to_string() };
            let destination = folder.join(&bucket);
            if !destination.exists() {
                new_folders.insert(destination.clone());
            }
            planned.push((file.clone(), destination.join(name_of(&file))));
        }
        if planned.is_empty() {
            return Err(fail("NOTHING_TO_ORGANIZE", "Düzenlenecek dosya yok (alt klasörlerdekilere dokunulmaz)."));
        }
        return Ok(preview_of(&planned, &new_folders.into_iter().collect::<Vec<_>>()));
    }
    for file in regular_files(&folder, false, limit) {
        let bucket = if by_month {
            date(modified(&file), "%Y-%m")
        } else {
            bucket_for(&extension_of(&file)).to_string()
        };
        let destination = folder.join(&bucket);
        if !destination.exists() {
            if fs::create_dir(&destination).is_err() {
                continue;
            }
            made.push(destination.clone());
        }
        let target = free_path(&destination.join(name_of(&file)));
        if crate::files::move_across(&file, &target).is_ok() {
            *moved.entry(bucket).or_default() += 1;
            moves.push((target, file));
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
        Some(undoable(ctx, &folders, &moves, &made)),
    ))
}

pub(super) fn batch_rename(ctx: &Context, request: &Request, limit: usize) -> Result<Done> {
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
    let mut renamed: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut planned: Vec<(PathBuf, PathBuf)> = Vec::new();
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
        if request.preview {
            planned.push((file.clone(), folder.join(name)));
            continue;
        }
        let destination = free_path(&folder.join(name));
        if fs::rename(file, &destination).is_ok() {
            renamed.push((destination, file.clone()));
        }
    }
    if request.preview {
        if planned.is_empty() {
            return Err(fail("RENAME_FAILED", "Yeniden adlandırılacak dosya yok."));
        }
        return Ok(preview_of(&planned, &[]));
    }
    if renamed.is_empty() {
        return Err(fail("RENAME_FAILED", "Yeniden adlandırılacak dosya yok."));
    }
    let shown: Vec<PathBuf> = renamed
        .iter()
        .take(20)
        .map(|(now, _)| now.clone())
        .collect();
    Ok(Done::new(
        format!("{} dosya yeniden adlandırıldı.", renamed.len()),
        Some(undoable(ctx, &shown, &renamed, &[])),
    ))
}
