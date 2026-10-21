//! Reading documents as text: PDF, Word, Excel and plain text formats. The model never gets base64 of an Office file; it gets the words.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use wiqo_contract::FilePolicy;
use wiqo_core::engine::{Context, Lane};

use crate::fsx::{self, is_regular_file};
use crate::op::{data, tool_failure, Blocking, Done, Result};
use crate::Op;

const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_TEXT_CHARS: usize = 500_000;
const MAX_PDF_PAGES: usize = 200;

pub(crate) fn operations() -> Vec<Op> {
    vec![Arc::new(Blocking {
        name: "readDocument",
        lane: Lane::ReadOnly,
        run: read_document,
    })]
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Txt,
    Md,
    Csv,
    Html,
    Doc,
    Docx,
    Xlsx,
    Pdf,
}

impl Format {
    fn of(path: &Path) -> Option<Format> {
        match path.extension()?.to_string_lossy().to_lowercase().as_str() {
            "txt" => Some(Format::Txt),
            "md" | "markdown" => Some(Format::Md),
            "csv" => Some(Format::Csv),
            "html" | "htm" => Some(Format::Html),
            "doc" => Some(Format::Doc),
            "docx" => Some(Format::Docx),
            "xls" | "xlsx" | "ods" => Some(Format::Xlsx),
            "pdf" => Some(Format::Pdf),
            _ => None,
        }
    }

    /// The name the contract gives the format (`DocumentFormat`).
    fn wire(self) -> &'static str {
        match self {
            Format::Txt => "txt",
            Format::Md => "md",
            Format::Csv => "csv",
            Format::Html => "html",
            Format::Doc => "doc",
            Format::Docx => "docx",
            Format::Xlsx => "xlsx",
            Format::Pdf => "pdf",
        }
    }
}

/// Formats whose bytes are not text: `readFile` extracts their words instead of returning base64.
pub(crate) fn is_document(path: &Path) -> bool {
    matches!(
        Format::of(path),
        Some(Format::Pdf | Format::Doc | Format::Docx | Format::Xlsx)
    )
}

fn failure(code: &str, message: &str) -> wiqo_core::wire::WireError {
    tool_failure(code, message)
}

/// The text of a document at a path that is already validated.
pub(crate) fn extract_text(path: &Path) -> Result<String> {
    Ok(extract(path)?.1)
}

fn extract(path: &Path) -> Result<(Format, String)> {
    if FilePolicy::get().is_sensitive(&path.to_string_lossy()) {
        return Err(fsx::sensitive_blocked("dosyalar"));
    }
    if !is_regular_file(path) {
        return Err(failure(
            "DOCUMENT_NOT_REGULAR",
            "Yalnızca normal belge dosyaları okunabilir.",
        ));
    }
    let size = fs::metadata(path)
        .map_err(|_| failure("DOCUMENT_READ_FAILED", "Belge okunamadı."))?
        .len();
    if size > MAX_FILE_BYTES {
        return Err(failure(
            "DOCUMENT_TOO_LARGE",
            "Belge desteklenen boyut sınırını aşıyor.",
        ));
    }
    let format = Format::of(path).ok_or_else(|| {
        failure(
            "DOCUMENT_UNSUPPORTED",
            "Bu belge biçimi henüz desteklenmiyor.",
        )
    })?;
    let text = match format {
        Format::Txt | Format::Md | Format::Csv | Format::Html => {
            fsx::read_text(path).map_err(|source| {
                fsx::local_file_error("DOCUMENT_READ_FAILED", "Belge okunamadı.", &source)
            })?
        }
        Format::Pdf => pdf_text(path)?,
        Format::Docx => docx_text(path)?,
        Format::Xlsx => spreadsheet_text(path)?,
        Format::Doc => legacy_word_text(path)?,
    };
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(failure(
            "DOCUMENT_TEXT_TOO_LARGE",
            "Belgeden çıkarılan metin desteklenen sınırı aşıyor.",
        ));
    }
    Ok((format, text))
}

fn pdf_text(path: &Path) -> Result<String> {
    let bytes =
        fs::read(path).map_err(|_| failure("DOCUMENT_PARSE_FAILED", "PDF belgesi açılamadı."))?;
    // The parser panics on some damaged files; that is a failed read, not a crash.
    let pages = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(&bytes))
        .map_err(|_| failure("DOCUMENT_PARSE_FAILED", "PDF belgesi açılamadı."))?;
    let pages = pages.map_err(|_| failure("DOCUMENT_PARSE_FAILED", "PDF belgesi açılamadı."))?;
    if pages.len() > MAX_PDF_PAGES {
        return Err(failure(
            "DOCUMENT_LIMIT",
            "PDF kilitli veya 200 sayfalık okuma sınırını aşıyor.",
        ));
    }
    let text = pages
        .iter()
        .enumerate()
        .map(|(index, page)| format!("--- Sayfa {} ---\n{}", index + 1, page.trim()))
        .collect::<Vec<_>>()
        .join("\n\n");
    if pages.iter().all(|page| page.trim().is_empty()) {
        return Err(failure("DOCUMENT_OCR_REQUIRED", "Bu PDF taranmış görünüyor; metin katmanı bulunamadı. Belgeyi görsel olarak ekleyip görsel analizle okumayı dene."));
    }
    Ok(text)
}

fn zip_entry(path: &Path, name: &str) -> Result<String> {
    let file =
        fs::File::open(path).map_err(|_| failure("DOCUMENT_READ_FAILED", "Belge okunamadı."))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|_| failure("DOCUMENT_PARSE_FAILED", "Belge açılamadı."))?;
    let entry = archive
        .by_name(name)
        .map_err(|_| failure("DOCUMENT_PARSE_FAILED", "Belge içeriği bulunamadı."))?;
    let mut xml = String::new();
    entry
        .take(60 * 1024 * 1024)
        .read_to_string(&mut xml)
        .map_err(|_| failure("DOCUMENT_PARSE_FAILED", "Belge içeriği okunamadı."))?;
    Ok(xml)
}

/// Paragraphs of a .docx: the text of every `w:t`, one line per `w:p`, a tab between table cells.
fn docx_text(path: &Path) -> Result<String> {
    use quick_xml::events::Event;
    let xml = zip_entry(path, "word/document.xml")?;
    let mut reader = quick_xml::Reader::from_str(&xml);
    let (mut out, mut in_text) = (String::new(), false);
    loop {
        match reader.read_event() {
            Ok(Event::Start(tag)) => match tag.name().as_ref() {
                b"w:t" => in_text = true,
                b"w:tab" => out.push('\t'),
                _ => {}
            },
            Ok(Event::Empty(tag)) => match tag.name().as_ref() {
                b"w:tab" => out.push('\t'),
                b"w:br" => out.push('\n'),
                _ => {}
            },
            Ok(Event::End(tag)) => match tag.name().as_ref() {
                b"w:t" => in_text = false,
                b"w:p" => out.push('\n'),
                b"w:tc" => out.push('\t'),
                _ => {}
            },
            Ok(Event::Text(text)) if in_text => out.push_str(
                &text
                    .unescape()
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            Ok(Event::Eof) => break,
            Err(_) => return Err(failure("DOCUMENT_PARSE_FAILED", "Word belgesi çözülemedi.")),
            _ => {}
        }
    }
    Ok(out.trim().to_string())
}

fn spreadsheet_text(path: &Path) -> Result<String> {
    use calamine::{open_workbook_auto, Data, Reader};
    let mut workbook = open_workbook_auto(path)
        .map_err(|_| failure("DOCUMENT_PARSE_FAILED", "Excel belgesi açılamadı."))?;
    let mut out = String::new();
    for name in workbook.sheet_names() {
        let Ok(range) = workbook.worksheet_range(&name) else {
            continue;
        };
        out.push_str(&format!("## {name}\n"));
        for row in range.rows() {
            let cells: Vec<String> = row
                .iter()
                .map(|cell| match cell {
                    Data::Empty => String::new(),
                    Data::String(text) => text.clone(),
                    other => other.to_string(),
                })
                .map(|cell| {
                    if cell.contains([',', '"', '\n']) {
                        format!("\"{}\"", cell.replace('"', "\"\""))
                    } else {
                        cell
                    }
                })
                .collect();
            out.push_str(&cells.join(","));
            out.push('\n');
        }
        out.push('\n');
    }
    Ok(out.trim().to_string())
}

/// Old .doc files need a converter that is already on the machine: textutil on a Mac, LibreOffice elsewhere.
fn legacy_word_text(path: &Path) -> Result<String> {
    let attempts: Vec<(&str, Vec<String>)> = if cfg!(target_os = "macos") {
        vec![(
            "/usr/bin/textutil",
            vec![
                "-convert".into(),
                "txt".into(),
                "-stdout".into(),
                path.to_string_lossy().into(),
            ],
        )]
    } else {
        vec![]
    };
    for (program, arguments) in attempts {
        if let Ok(output) = crate::process::command(program).args(arguments).output() {
            if output.status.success() {
                return Ok(String::from_utf8_lossy(&output.stdout).to_string());
            }
        }
    }
    Err(failure("DOCUMENT_ENGINE_UNAVAILABLE", "Eski .doc biçimini okuyacak bir araç bu bilgisayarda yok; belgeyi .docx olarak kaydedip tekrar dene."))
}

fn read_document(_ctx: &Context, value: &Value) -> Result<Done> {
    let filename: String = crate::op::payload(value, "fileName", "belge adı")?;
    if FilePolicy::get().is_sensitive(&filename) {
        return Err(fsx::sensitive_blocked("dosyalar"));
    }
    let missing = || {
        fsx::missing_path_error(
            &format!("~/Desktop/{filename}"),
            &format!("Masaüstünde \"{filename}\" belgesi bulunamadı."),
        )
    };
    let desktop = fsx::desktop_dir().ok_or_else(missing)?;
    if !wiqo_contract::is_valid_file_name(&filename) {
        return Err(missing());
    }
    let path = desktop.join(filename.trim());
    let resolved = fsx::resolve(&path.to_string_lossy(), false).ok_or_else(missing)?;
    if dunce::canonicalize(&desktop).ok().as_deref() != resolved.parent() {
        return Err(missing());
    }
    let (format, text) = extract(&resolved)?;
    Ok(Done::new(
        format!("{filename} okundu."),
        Some(data(
            "documentContent",
            json!({ "filename": filename, "format": format.wire(), "text": text }),
        )),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wiqo-docs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dunce::canonicalize(&dir).unwrap()
    }

    #[test]
    fn a_docx_gives_its_paragraphs_and_table_cells() {
        let dir = scratch();
        let path = dir.join("rapor.docx");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        zip.start_file(
            "word/document.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(br#"<w:document><w:body><w:p><w:r><w:t>Merhaba</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#).unwrap();
        zip.finish().unwrap();
        let text = extract_text(&path).unwrap();
        assert!(text.starts_with("Merhaba"), "{text}");
        assert!(text.contains("A1") && text.contains("B1"));
        assert!(is_document(&path));
    }

    #[test]
    fn unsupported_oversized_and_sensitive_documents_are_refused_with_contract_codes() {
        let dir = scratch();
        let odd = dir.join("model.xyz");
        fs::write(&odd, "x").unwrap();
        assert_eq!(extract_text(&odd).unwrap_err().code, "DOCUMENT_UNSUPPORTED");
        assert_eq!(
            extract_text(&dir.join("sifre.txt")).unwrap_err().code,
            "SENSITIVE_FILE_BLOCKED"
        );
        let text = dir.join("notlar.txt");
        fs::write(&text, "satır bir").unwrap();
        assert_eq!(extract_text(&text).unwrap(), "satır bir");
        let broken = dir.join("bozuk.pdf");
        fs::write(&broken, "bu bir pdf değil").unwrap();
        assert_eq!(
            extract_text(&broken).unwrap_err().code,
            "DOCUMENT_PARSE_FAILED"
        );
    }
}
