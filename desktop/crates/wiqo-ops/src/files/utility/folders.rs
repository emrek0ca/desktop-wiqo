//! Looking at a path, moving documents as one all-or-nothing step, and making a folder.

use super::*;

// ----- inspect / move / folders -----

pub(super) fn inspect(request: &Request) -> Result<Done> {
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
pub(super) fn move_documents(ctx: &Context, request: &Request, limit: usize) -> Result<Done> {
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
    let made: Vec<PathBuf> = if destination.exists() {
        Vec::new()
    } else {
        vec![destination.clone()]
    };
    if request.preview {
        let planned: Vec<(PathBuf, PathBuf)> = documents
            .iter()
            .map(|document| (document.clone(), destination.join(name_of(document))))
            .collect();
        return Ok(preview_of(&planned, &made));
    }
    if !destination.exists() {
        fs::create_dir(&destination).map_err(|_| {
            fail(
                "CREATE_FOLDER_FAILED",
                "Hedef klasör oluşturulamadı; hiçbir belge taşınmadı.",
            )
        })?;
    }
    // Whatever moved, all of it or part, can be put back with one tap.
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let total = documents.len();
    let shown = |moved: &[(PathBuf, PathBuf)]| -> Value {
        let now: Vec<PathBuf> = moved.iter().map(|(now, _)| now.clone()).collect();
        undoable(ctx, &now, moved, &made)
    };
    for document in &documents {
        let target = destination.join(name_of(document));
        let partial = |moved: &[(PathBuf, PathBuf)], message: String| -> Result<Done> {
            Ok(Done::partial(fail("PARTIAL_MOVE", message), shown(moved)))
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
                shown(&moved),
            ));
        }
        moved.push((target, document.clone()));
    }
    Ok(Done::new(
        format!(
            "{} belge {} klasörüne taşındı ve doğrulandı.",
            moved.len(),
            name_of(&destination)
        ),
        Some(shown(&moved)),
    ))
}

pub(super) fn create_folder(request: &Request) -> Result<Done> {
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
