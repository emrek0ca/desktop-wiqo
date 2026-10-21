//! `convertFile`: a picture, a text or an office document as a PDF next to the original. The work happens in a scratch folder, so a half-written PDF never
//! appears; a taken name gets " (2)", and `overwrite` replaces only after a backup.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use wiqo_core::engine::{Context, Lane};

use crate::fsx::{self, entry, is_dir, is_regular_file};
use crate::op::{data, payload, tool_failure, Blocking, Done, Result};
use crate::process::run_with_timeout;
use crate::Op;

pub(crate) fn operations() -> Vec<Op> {
    vec![Arc::new(Blocking {
        name: "convertFile",
        lane: Lane::SideEffect,
        run: convert_file,
    })]
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Request {
    path: String,
    destination: Option<String>,
    overwrite: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Image,
    Text,
    Writer,
    Sheet,
    Slides,
}

/// The same classes as `RovaFileConversion.engine(for:)` on the phone.
fn kind(path: &Path) -> Option<Kind> {
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "heic" | "tif" | "tiff" | "gif" | "bmp" => Some(Kind::Image),
        "txt" | "md" | "log" => Some(Kind::Text),
        "docx" | "doc" | "rtf" | "odt" | "pages" | "html" | "htm" => Some(Kind::Writer),
        "xlsx" | "xls" | "csv" | "ods" | "numbers" => Some(Kind::Sheet),
        "pptx" | "ppt" | "odp" | "key" => Some(Kind::Slides),
        _ => None,
    }
}

pub(crate) fn convert_file(ctx: &Context, value: &Value) -> Result<Done> {
    let request: Request = payload(value, "fileOperation", "dosya")?;
    let invalid = |message: &str| tool_failure("INVALID_FILE_PATH", message);
    let source = fsx::resolve(&request.path, false)
        .filter(|path| is_regular_file(path))
        .ok_or_else(|| invalid("Bu dosya PDF'e çevrilemez ya da bulunamadı."))?;
    let kind =
        kind(&source).ok_or_else(|| invalid("Bu dosya PDF'e çevrilemez ya da bulunamadı."))?;
    let preferred = match &request.destination {
        Some(raw) => {
            let path = fsx::resolve(raw, true)
                .ok_or_else(|| invalid("PDF'in kaydedileceği yol geçersiz."))?;
            if is_dir(&path) {
                path.join(format!("{}.pdf", stem(&source)))
            } else {
                path
            }
        }
        None => source.with_extension("pdf"),
    };
    if !preferred.parent().is_some_and(is_dir) {
        return Err(invalid("PDF'in kaydedileceği klasör bulunamadı."));
    }
    let destination = unique(&preferred, request.overwrite);
    let scratch = std::env::temp_dir().join(format!("wiqo-convert-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&scratch)
        .map_err(|_| tool_failure("CONVERT_FAILED", "Dönüştürme başlatılamadı."))?;
    let produced = scratch.join("out.pdf");
    let result = run_conversion(kind, &source, &produced, &scratch);
    let made = result.and_then(|()| {
        let size = fs::metadata(&produced).map(|meta| meta.len()).unwrap_or(0);
        if size == 0 {
            return Err("PDF üretilemedi.".to_string());
        }
        Ok(())
    });
    if let Err(reason) = made {
        let _ = fs::remove_dir_all(&scratch);
        return Err(tool_failure("CONVERT_FAILED", reason));
    }
    if destination.exists() {
        crate::files::back_up(ctx, &destination)?;
    }
    let placed = crate::files::move_across(&produced, &destination);
    let _ = fs::remove_dir_all(&scratch);
    placed.map_err(|_| tool_failure("FILE_WRITE_FAILED", "PDF hedef klasöre kaydedilemedi."))?;
    let item = entry(&destination)
        .ok_or_else(|| tool_failure("FILE_WRITE_FAILED", "PDF doğrulanamadı."))?;
    Ok(Done::new(
        format!("PDF hazır: {}", item.name),
        Some(data("files", vec![item])),
    ))
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "belge".into())
}

fn unique(path: &Path, overwrite: bool) -> PathBuf {
    if overwrite || !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    (2..100)
        .map(|index| parent.join(format!("{} ({index}).pdf", stem(path))))
        .find(|candidate| !candidate.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

fn run(program: &str, args: &[&str], seconds: u64) -> Option<std::process::Output> {
    let mut command = crate::process::command(program);
    command.args(args);
    run_with_timeout(command, Duration::from_secs(seconds))
}

fn ok(output: &Option<std::process::Output>) -> bool {
    output.as_ref().is_some_and(|out| out.status.success())
}

/// `Ok` when `output` holds the PDF, otherwise the sentence for the person.
fn run_conversion(
    kind: Kind,
    source: &Path,
    output: &Path,
    scratch: &Path,
) -> std::result::Result<(), String> {
    let (src, out) = (
        source.to_string_lossy().to_string(),
        output.to_string_lossy().to_string(),
    );
    match kind {
        Kind::Image => {
            if image_pdf(source, output) {
                return Ok(());
            }
            if cfg!(target_os = "macos")
                && ok(&run(
                    "/usr/bin/sips",
                    &["-s", "format", "pdf", &src, "--out", &out],
                    60,
                ))
            {
                return Ok(());
            }
            Err("Görsel PDF'e çevrilemedi.".into())
        }
        Kind::Text if cfg!(target_os = "macos") => {
            let result = run("/usr/sbin/cupsfilter", &["-m", "application/pdf", &src], 60)
                .filter(|result| result.status.success() && !result.stdout.is_empty())
                .ok_or("Metin PDF'e çevrilemedi.")?;
            fs::write(output, result.stdout).map_err(|_| "PDF yazılamadı.".to_string())
        }
        _ => {
            if let Some(office) = libre_office() {
                let done = run(
                    &office,
                    &[
                        "--headless",
                        "--convert-to",
                        "pdf",
                        "--outdir",
                        &scratch.to_string_lossy(),
                        &src,
                    ],
                    120,
                );
                let converted = scratch.join(format!("{}.pdf", stem(source)));
                if ok(&done) && fs::rename(&converted, output).is_ok() {
                    return Ok(());
                }
            }
            office_app(kind, source, output)
        }
    }
}

fn libre_office() -> Option<String> {
    let mut candidates: Vec<String> = vec![
        "/Applications/LibreOffice.app/Contents/MacOS/soffice".into(),
        "/opt/homebrew/bin/soffice".into(),
        "/usr/local/bin/soffice".into(),
        "/usr/bin/soffice".into(),
        "/usr/bin/libreoffice".into(),
        r"C:\Program Files\LibreOffice\program\soffice.exe".into(),
        r"C:\Program Files (x86)\LibreOffice\program\soffice.exe".into(),
    ];
    candidates.retain(|path| Path::new(path).is_file());
    candidates.into_iter().next()
}

/// One page per picture, the JPEG embedded as it is: no other program needed.
fn image_pdf(source: &Path, output: &Path) -> bool {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};
    let Ok(picture) = image::open(source) else {
        return false;
    };
    let rgb = picture.to_rgb8();
    let (width, height) = rgb.dimensions();
    let mut jpeg = Vec::new();
    if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode_image(&rgb)
        .is_err()
    {
        return false;
    }
    // A4 width, the height that keeps the picture's shape.
    let page_width = 595.0_f32;
    let page_height = page_width * height as f32 / width.max(1) as f32;
    let mut document = Document::with_version("1.5");
    let tree = document.new_object_id();
    let image = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => width, "Height" => height,
            "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "DCTDecode",
        },
        jpeg,
    ));
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    page_width.into(),
                    0.into(),
                    0.into(),
                    page_height.into(),
                    0.into(),
                    0.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Im0".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    let Ok(encoded) = content.encode() else {
        return false;
    };
    let contents = document.add_object(Stream::new(dictionary! {}, encoded));
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => tree, "Contents" => contents,
        "MediaBox" => vec![0.into(), 0.into(), page_width.into(), page_height.into()],
        "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image } },
    });
    document.objects.insert(
        tree,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => tree });
    document.trailer.set("Root", catalog);
    document.save(output).is_ok()
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Without LibreOffice: Pages/Numbers/Keynote or Microsoft Office on a Mac (AppleScript), Microsoft Office on Windows (COM).
fn office_app(kind: Kind, source: &Path, output: &Path) -> std::result::Result<(), String> {
    let (src, out) = (
        source.to_string_lossy().to_string(),
        output.to_string_lossy().to_string(),
    );
    let missing =
        "Bu dosyayı PDF'e çevirmek için LibreOffice ya da Microsoft Office kurulu olmalı."
            .to_string();
    if cfg!(target_os = "macos") {
        let (iwork, microsoft) = match kind {
            Kind::Sheet => ("Numbers", "Microsoft Excel"),
            Kind::Slides => ("Keynote", "Microsoft PowerPoint"),
            _ => ("Pages", "Microsoft Word"),
        };
        let microsoft_body = match kind {
            Kind::Sheet => format!(
                "open workbook workbook file name {}\nsave workbook as active workbook filename {} file format PDF file format\nclose active workbook saving no",
                quoted(&src), quoted(&out)
            ),
            Kind::Slides => format!(
                "open {}\nsave active presentation in {} as save as PDF\nclose active presentation",
                quoted(&src), quoted(&out)
            ),
            _ => format!(
                "open POSIX file {}\nsave as active document file name {} file format format PDF\nclose active document saving no",
                quoted(&src), quoted(&out)
            ),
        };
        let attempts = [
            (iwork, format!(
                "set theDocument to open POSIX file {}\nexport theDocument to POSIX file {} as PDF\nclose theDocument saving no",
                quoted(&src), quoted(&out)
            )),
            (microsoft, microsoft_body),
        ];
        let mut last = None;
        for (app, body) in attempts {
            if !crate::apps::is_installed(app) {
                continue;
            }
            let script = format!(
                "with timeout of 110 seconds\ntell application {}\n{body}\nend tell\nend timeout",
                quoted(app)
            );
            let result = run("/usr/bin/osascript", &["-e", &script], 120);
            if ok(&result) && output.exists() {
                return Ok(());
            }
            let stderr = result
                .map(|r| String::from_utf8_lossy(&r.stderr).to_string())
                .unwrap_or_default();
            last = Some(
                if stderr.contains("-1743") || stderr.contains("not allowed") {
                    format!("{app} için Wiqo'ya Otomasyon izni ver (Sistem Ayarları › Gizlilik ve Güvenlik › Otomasyon).")
                } else {
                    format!("{app} dosyayı PDF'e çeviremedi.")
                },
            );
        }
        return Err(last.unwrap_or_else(|| {
            "Bu dosyayı PDF'e çevirmek için Pages/Numbers/Keynote, Microsoft Office veya LibreOffice kurulu olmalı.".into()
        }));
    }
    if cfg!(windows) {
        let (src, out) = (src.replace('\'', "''"), out.replace('\'', "''"));
        let script = match kind {
            Kind::Sheet => format!("$a=New-Object -ComObject Excel.Application;$a.DisplayAlerts=$false;$b=$a.Workbooks.Open('{src}');$b.ExportAsFixedFormat(0,'{out}');$b.Close($false);$a.Quit()"),
            Kind::Slides => format!("$a=New-Object -ComObject PowerPoint.Application;$p=$a.Presentations.Open('{src}',$true,$false,$false);$p.SaveAs('{out}',32);$p.Close();$a.Quit()"),
            _ => format!("$a=New-Object -ComObject Word.Application;$a.Visible=$false;$d=$a.Documents.Open('{src}',$false,$true);$d.SaveAs([ref]'{out}',[ref]17);$d.Close($false);$a.Quit()"),
        };
        if ok(&run(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", &script],
            120,
        )) && output.exists()
        {
            return Ok(());
        }
    }
    Err(missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_picture_becomes_a_one_page_pdf_beside_it_and_never_over_a_taken_name() {
        let dir = std::env::temp_dir().join(format!("wiqo-convert-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let dir = dunce::canonicalize(dir).unwrap();
        let picture = dir.join("foto.png");
        image::RgbImage::from_pixel(40, 20, image::Rgb([200, 30, 30]))
            .save(&picture)
            .unwrap();
        fs::write(dir.join("foto.pdf"), b"taken").unwrap();
        let request =
            json!({ "kind": "fileOperation", "value": { "path": picture.to_string_lossy() } });
        let done = convert_file(&Context::for_tests(), &request).unwrap();
        assert_eq!(done.message, "PDF hazır: foto (2).pdf");
        assert_eq!(
            lopdf::Document::load(dir.join("foto (2).pdf"))
                .unwrap()
                .get_pages()
                .len(),
            1
        );
        assert_eq!(fs::read(dir.join("foto.pdf")).unwrap(), b"taken");
    }
}
