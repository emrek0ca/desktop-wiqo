//! Documents: reading PDF, Word, Excel and plain text as text (the model never gets base64 of an Office file; it gets the words), and turning the text
//! `writeFile` was given into a real Word, Excel or PDF file when the path names one. Word and Excel are written here, the same on every platform; a
//! PDF is printed by the computer's own Chrome or Edge (macOS falls back to its print system).

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use wiqo_contract::FilePolicy;

use crate::fsx::{self, is_regular_file};
use crate::op::{tool_failure, Result};
use crate::process::run_with_timeout;

const MAX_FILE_BYTES: u64 = 250 * 1024 * 1024;
const MAX_TEXT_CHARS: usize = 500_000;
const MAX_PDF_PAGES: usize = 200;

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
    Ok(text)
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

// ----- writing: the text writeFile was given, as the format its path names -----

/// The bytes `writeFile` puts at `path`: the text itself, or a Word/Excel/PDF document made from it when the extension asks for one.
pub(crate) fn bytes_for(path: &Path, content: &str) -> Result<Vec<u8>> {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase());
    match extension.as_deref() {
        Some("docx") => docx_bytes(content),
        Some("xlsx") => xlsx_bytes(content),
        Some("pdf") => pdf_bytes(content),
        Some("doc" | "xls" | "ods") => Err(tool_failure(
            "DOCUMENT_UNSUPPORTED",
            "Bu eski biçim yazılmıyor; belgeyi .docx veya .xlsx olarak kaydet.",
        )),
        _ => Ok(content.as_bytes().to_vec()),
    }
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// One block of the text: markdown-style headings and bullets are kept as such, everything else is a paragraph.
enum Block<'a> {
    Heading(usize, &'a str),
    Bullet(&'a str),
    Paragraph(&'a str),
}

fn blocks(content: &str) -> Vec<Block<'_>> {
    content
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let level = trimmed.chars().take_while(|c| *c == '#').count();
            if (1..=3).contains(&level) && trimmed[level..].starts_with(' ') {
                Block::Heading(level, trimmed[level..].trim())
            } else if let Some(rest) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
            {
                Block::Bullet(rest.trim())
            } else {
                Block::Paragraph(line.trim_end())
            }
        })
        .collect()
}

fn zip_document(entries: &[(&str, String)]) -> Result<Vec<u8>> {
    let failed = || tool_failure("DOCUMENT_WRITE_FAILED", "Belge oluşturulamadı.");
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, body) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .map_err(|_| failed())?;
        zip.write_all(body.as_bytes()).map_err(|_| failed())?;
    }
    Ok(zip.finish().map_err(|_| failed())?.into_inner())
}

fn docx_bytes(content: &str) -> Result<Vec<u8>> {
    let run = |text: &str, size: Option<u32>| {
        let props = size
            .map(|half_points| format!("<w:rPr><w:b/><w:sz w:val=\"{half_points}\"/></w:rPr>"))
            .unwrap_or_default();
        format!(
            "<w:p><w:r>{props}<w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
            xml_escape(text)
        )
    };
    let body: String = blocks(content)
        .into_iter()
        .map(|block| match block {
            Block::Heading(level, text) => run(text, Some([36, 30, 26][level - 1])),
            Block::Bullet(text) => run(&format!("• {text}"), None),
            Block::Paragraph(text) => run(text, None),
        })
        .collect();
    zip_document(&[
        ("[Content_Types].xml", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.into()),
        ("_rels/.rels", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.into()),
        ("word/document.xml", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#)),
    ])
}

/// Rows of a table written as text: cells split by "|" or a tab (a markdown table's "---" line is left out), otherwise by a comma.
fn rows(content: &str) -> Vec<Vec<String>> {
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| {
            !line
                .trim()
                .trim_matches('|')
                .chars()
                .all(|c| matches!(c, '-' | ':' | '|' | ' '))
        })
        .map(|line| {
            let line = line.trim().trim_matches('|');
            let cells: Vec<&str> = if line.contains(['|', '\t']) {
                line.split(['|', '\t']).collect()
            } else {
                line.split(',').collect()
            };
            cells
                .into_iter()
                .map(|cell| cell.trim().to_string())
                .collect()
        })
        .collect()
}

fn column_name(mut index: usize) -> String {
    let mut name = String::new();
    loop {
        name.insert(0, (b'A' + (index % 26) as u8) as char);
        if index < 26 {
            return name;
        }
        index = index / 26 - 1;
    }
}

fn xlsx_bytes(content: &str) -> Result<Vec<u8>> {
    let sheet: String = rows(content)
        .iter()
        .enumerate()
        .map(|(row, cells)| {
            let cells: String = cells
                .iter()
                .enumerate()
                .map(|(column, cell)| {
                    let at = format!("{}{}", column_name(column), row + 1);
                    match cell.replace(',', ".").parse::<f64>() {
                        Ok(number) if number.is_finite() && !cell.is_empty() => {
                            format!("<c r=\"{at}\"><v>{number}</v></c>")
                        }
                        _ => format!(
                            "<c r=\"{at}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                            xml_escape(cell)
                        ),
                    }
                })
                .collect();
            format!("<row r=\"{}\">{cells}</row>", row + 1)
        })
        .collect();
    zip_document(&[
        ("[Content_Types].xml", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#.into()),
        ("_rels/.rels", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.into()),
        ("xl/workbook.xml", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sayfa1" sheetId="1" r:id="rId1"/></sheets></workbook>"#.into()),
        ("xl/_rels/workbook.xml.rels", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#.into()),
        ("xl/worksheets/sheet1.xml", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{sheet}</sheetData></worksheet>"#)),
    ])
}

fn html_page(content: &str) -> String {
    let body: String = blocks(content)
        .into_iter()
        .map(|block| match block {
            Block::Heading(level, text) => format!("<h{level}>{}</h{level}>", xml_escape(text)),
            Block::Bullet(text) => format!("<li>{}</li>", xml_escape(text)),
            Block::Paragraph("") => String::new(),
            Block::Paragraph(text) => format!("<p>{}</p>", xml_escape(text)),
        })
        .collect();
    format!("<!doctype html><html><head><meta charset=\"utf-8\"><style>body{{font-family:-apple-system,'Segoe UI',Helvetica,Arial,sans-serif;font-size:11pt;line-height:1.45;margin:0}}h1{{font-size:20pt}}h2{{font-size:16pt}}h3{{font-size:13pt}}li{{margin-left:1.2em}}</style></head><body>{body}</body></html>")
}

/// Chrome or Edge where they install themselves; the first that exists prints the PDF.
pub fn installed_browsers() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        found.extend(
            [
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            ]
            .map(PathBuf::from),
        );
    } else if cfg!(windows) {
        for base in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(root) = std::env::var_os(base).map(PathBuf::from) {
                found.push(root.join(r"Google\Chrome\Application\chrome.exe"));
                found.push(root.join(r"Microsoft\Edge\Application\msedge.exe"));
            }
        }
    } else {
        found.extend(
            [
                "/usr/bin/google-chrome",
                "/usr/bin/chromium",
                "/usr/bin/chromium-browser",
                "/usr/bin/microsoft-edge",
            ]
            .map(PathBuf::from),
        );
    }
    found.retain(|path| path.is_file());
    found
}

fn pdf_bytes(content: &str) -> Result<Vec<u8>> {
    let failed = || tool_failure("DOCUMENT_CONVERSION_FAILED", "PDF oluşturulamadı.");
    let scratch = std::env::temp_dir().join(format!("wiqo-document-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&scratch).map_err(|_| failed())?;
    let produced = print_pdf(content, &scratch);
    let _ = fs::remove_dir_all(&scratch);
    let bytes = produced?;
    if !bytes.starts_with(b"%PDF") {
        return Err(failed());
    }
    Ok(bytes)
}

fn print_pdf(content: &str, scratch: &Path) -> Result<Vec<u8>> {
    let failed = || tool_failure("DOCUMENT_CONVERSION_FAILED", "PDF oluşturulamadı.");
    let page = scratch.join("belge.html");
    fs::write(&page, html_page(content)).map_err(|_| failed())?;
    let output = scratch.join("belge.pdf");
    let page_url = url::Url::from_file_path(&page).map_err(|_| failed())?;
    for browser in installed_browsers() {
        let mut command = crate::process::command(&browser);
        command.args([
            "--headless".to_string(),
            "--disable-gpu".to_string(),
            "--no-first-run".to_string(),
            "--no-pdf-header-footer".to_string(),
            format!(
                "--user-data-dir={}",
                scratch.join("profile").to_string_lossy()
            ),
            format!("--print-to-pdf={}", output.to_string_lossy()),
            page_url.to_string(),
        ]);
        if run_with_timeout(command, Duration::from_secs(60)).is_some() {
            if let Ok(bytes) = fs::read(&output) {
                return Ok(bytes);
            }
        }
    }
    if cfg!(target_os = "macos") {
        let source = scratch.join("belge.txt");
        fs::write(&source, content).map_err(|_| failed())?;
        let mut command = crate::process::command("/usr/sbin/cupsfilter");
        command.args(["-m", "application/pdf", &source.to_string_lossy()]);
        if let Some(out) =
            run_with_timeout(command, Duration::from_secs(60)).filter(|out| out.status.success())
        {
            return Ok(out.stdout);
        }
    }
    Err(tool_failure(
        "DOCUMENT_ENGINE_UNAVAILABLE",
        "PDF yazmak için bu bilgisayarda Google Chrome veya Microsoft Edge bulunmalı.",
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

    #[test]
    fn written_word_and_excel_files_read_back_as_the_text_they_were_made_from() {
        let dir = scratch();
        let word = dir.join("rapor.docx");
        fs::write(
            &word,
            bytes_for(&word, "# Başlık\nİlk paragraf & <not>\n- madde").unwrap(),
        )
        .unwrap();
        let text = extract_text(&word).unwrap();
        assert!(
            text.contains("Başlık")
                && text.contains("İlk paragraf & <not>")
                && text.contains("• madde"),
            "{text}"
        );

        let sheet = dir.join("butce.xlsx");
        fs::write(
            &sheet,
            bytes_for(
                &sheet,
                "| Kalem | Tutar |\n|---|---|\n| Kira | 12500 |\n| Market | 3.250,5 |",
            )
            .unwrap(),
        )
        .unwrap();
        let text = extract_text(&sheet).unwrap();
        assert!(
            text.contains("Kalem,Tutar") && text.contains("Kira,12500"),
            "{text}"
        );
        assert!(!text.contains("---"), "{text}");

        let note = dir.join("not.txt");
        assert_eq!(
            bytes_for(&note, "düz metin").unwrap(),
            "düz metin".as_bytes()
        );
        assert_eq!(
            bytes_for(&dir.join("eski.doc"), "x").unwrap_err().code,
            "DOCUMENT_UNSUPPORTED"
        );
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(27), "AB");
    }

    #[test]
    fn a_pdf_is_printed_by_the_installed_browser_and_reads_back() {
        if installed_browsers().is_empty() {
            return; // no Chrome or Edge on this machine: the refusal path is the only one there is
        }
        let dir = scratch();
        let path = dir.join("ozet.pdf");
        fs::write(
            &path,
            bytes_for(&path, "# Özet\nÇalışma şöyle ilerledi.").unwrap(),
        )
        .unwrap();
        let text = extract_text(&path).unwrap();
        assert!(text.contains("Özet") && text.contains("Çalışma"), "{text}");
    }
}
