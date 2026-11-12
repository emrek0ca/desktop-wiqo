//! Zip: packing files into an archive and unpacking one.

use super::*;

// ----- zip -----

pub(super) fn sources(request: &Request) -> Result<Vec<PathBuf>> {
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

pub(super) fn archive(ctx: &Context, request: &Request) -> Result<Done> {
    let sources = sources(request)?;
    // A credential file does not leave under a harmless zip name: the same sensitive-name rule as read and send applies to what is packed.
    let policy = wiqo_contract::FilePolicy::get();
    if sources.iter().any(|source| policy.is_sensitive(&source.to_string_lossy())) {
        return Err(error("SENSITIVE_FILE_BLOCKED", "Kimlik bilgisi içerebilecek dosyalar arşive eklenemez.", "permission"));
    }
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
    let total: u64 = sources
        .iter()
        .map(|source| crate::files::tree_bytes(source))
        .sum();
    let mut done = 0;
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
                if item.file_type().is_symlink() || name_of(item.path()).starts_with(".DS_Store") || policy.is_sensitive(&item.path().to_string_lossy()) {
                    continue;
                }
                let relative = item.path().strip_prefix(&base).unwrap_or(item.path());
                let inner = format!("{prefix}{}", relative.to_string_lossy().replace('\\', "/"));
                if item.file_type().is_dir() {
                    zip.add_directory(format!("{inner}/"), options)?;
                } else {
                    zip.start_file(inner, options)?;
                    done += std::io::copy(&mut fs::File::open(item.path())?, &mut zip)?;
                    ctx.progress(done, total);
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

pub(super) fn extract(request: &Request) -> Result<Done> {
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
