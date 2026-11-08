//! The Trash, the file manager, other apps and the printer: restoring, revealing, opening with, printing, reading text off a picture, tags.

use super::*;

// ----- trash, finder, printing -----

pub(super) fn restore_from_trash(request: &Request) -> Result<Done> {
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

pub(super) fn reveal(request: &Request) -> Result<Done> {
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

pub(super) fn open_with(request: &Request) -> Result<Done> {
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

pub(super) fn print(request: &Request) -> Result<Done> {
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
pub(super) fn ocr(request: &Request) -> Result<Done> {
    let path = existing(&request.path)?;
    if extension_of(&path) == "pdf" {
        if let Ok(text) = crate::files::documents::extract_text(&path) {
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
