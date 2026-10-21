//! File work inside a terminal grant: read a line range, write a file, search contents, apply find-and-replace edits or unified patches — every edit
//! checked before any file is written (never half a change), a JSON file that parsed must still parse, and an undo record kept on this computer.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wiqo_core::engine::Services;
use wiqo_core::wire::WireError;

use super::policy::{self, redact_secrets};
use super::shell::{self, Control};

const UNDO_DAYS: i64 = 7;
const UNDO_KEPT: usize = 200;

fn invalid(code: &str, message: impl Into<String>) -> WireError {
    WireError::new(code, message, "validation")
}

fn failure(code: &str, message: impl Into<String>) -> WireError {
    WireError::new(code, message, "tool_failure")
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// A text file's lines `start..=end` (400 by default), numbered, credentials masked.
pub fn read(
    path: &Path,
    start: Option<usize>,
    end: Option<usize>,
) -> Result<(String, usize), WireError> {
    let too_large = || {
        invalid(
            "TERMINAL_FILE_TOO_LARGE",
            "Dosya okunamadı ya da 2 MB'tan büyük.",
        )
    };
    if fs::metadata(path).map_err(|_| too_large())?.len() > 2_000_000 {
        return Err(too_large());
    }
    let content = String::from_utf8(fs::read(path).map_err(|_| too_large())?)
        .map_err(|_| invalid("TERMINAL_FILE_NOT_TEXT", "Dosya UTF-8 metin değil."))?;
    let lines: Vec<&str> = content.split('\n').collect();
    let first = start.unwrap_or(1).max(1);
    if first > lines.len() {
        return Ok((String::new(), lines.len()));
    }
    let last = end.unwrap_or(first + 399).max(first).min(lines.len());
    let mut text = String::new();
    for (index, line) in lines.iter().enumerate().take(last).skip(first - 1) {
        text.push_str(&format!("{}\t{line}\n", index + 1));
        if text.len() > 60_000 {
            text.push_str("[... 60 KB sınırı; daha dar satır aralığı iste ...]\n");
            break;
        }
    }
    Ok((redact_secrets(&text), lines.len()))
}

/// A JSON file that parsed before the change must still parse after it (models drop a comma in package.json and then give up at the npm error).
fn broken_json(path: &Path, before: Option<&str>, after: &str, display: &str) -> Option<WireError> {
    let is_json = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
    let before = before?;
    if !is_json || serde_json::from_str::<serde_json::Value>(before).is_err() {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(after).err().map(|reason| {
        invalid(
            "TERMINAL_EDIT_INVALID_JSON",
            format!("{display}: değişiklik sonrası JSON geçersiz olurdu ({reason}); hiçbir dosya değiştirilmedi. Virgül ve parantezleri kontrol edip tekrar dene."),
        )
    })
}

fn write_atomic(path: &Path, content: &str) -> Result<(), WireError> {
    crate::files::write_atomic(path, content.as_bytes()).map_err(|source| {
        crate::fsx::local_file_error("TERMINAL_FILE_WRITE_FAILED", "Dosya yazılamadı.", &source)
    })
}

/// Creates or replaces a text file. Returns whether it was created.
pub fn write(path: &Path, content: &str) -> Result<bool, WireError> {
    if content.len() > 400_000 || content.contains('\0') {
        return Err(invalid(
            "TERMINAL_FILE_TOO_LARGE",
            "Yazılacak içerik 400 KB'tan büyük ya da ikili.",
        ));
    }
    let existed = path.exists();
    if existed {
        let before = fs::read_to_string(path).ok();
        if let Some(error) = broken_json(path, before.as_deref(), content, &name(path)) {
            return Err(error);
        }
    }
    write_atomic(path, content)?;
    Ok(!existed)
}

/// One find-and-replace edit, already resolved to a file in the grant.
pub struct Replacement {
    pub path: PathBuf,
    pub display: String,
    pub find: String,
    pub replace: String,
    pub replace_all: bool,
}

/// Exact find-and-replace edits (the form models get right far more often than diffs): each `find` occurs once unless `replace_all`.
pub fn replace(edits: &[Replacement]) -> Result<String, WireError> {
    let mut contents: Vec<(PathBuf, String, String)> = Vec::new();
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for edit in edits {
        if edit.find.is_empty() {
            return Err(invalid(
                "TERMINAL_EDIT_INVALID",
                format!(
                    "{}: aranacak metin boş; hiçbir dosya değiştirilmedi.",
                    edit.display
                ),
            ));
        }
        if !contents.iter().any(|(path, _, _)| *path == edit.path) {
            let text = fs::read_to_string(&edit.path).map_err(|_| {
                invalid(
                    "TERMINAL_FILE_NOT_TEXT",
                    format!("{} okunamadı; hiçbir dosya değiştirilmedi.", edit.display),
                )
            })?;
            contents.push((edit.path.clone(), text.clone(), text));
        }
        let entry = contents
            .iter_mut()
            .find(|(path, _, _)| *path == edit.path)
            .expect("loaded");
        let occurrences = entry.2.matches(&edit.find).count();
        if occurrences == 0 {
            return Err(invalid(
                "TERMINAL_EDIT_NOT_FOUND",
                format!("{}: aranan metin dosyada birebir bulunamadı (boşluk ve girinti dahil); dosyayı read_file ile okuyup tekrar dene. Hiçbir dosya değiştirilmedi.", edit.display),
            ));
        }
        if occurrences > 1 && !edit.replace_all {
            return Err(invalid(
                "TERMINAL_EDIT_AMBIGUOUS",
                format!("{}: aranan metin {occurrences} kez geçiyor; daha uzun, tek geçen bir parça ver ya da replaceAll kullan. Hiçbir dosya değiştirilmedi.", edit.display),
            ));
        }
        entry.2 = if edit.replace_all {
            entry.2.replace(&edit.find, &edit.replace)
        } else {
            entry.2.replacen(&edit.find, &edit.replace, 1)
        };
        *counts.entry(edit.display.clone()).or_default() +=
            if edit.replace_all { occurrences } else { 1 };
    }
    for (path, before, after) in &contents {
        if let Some(error) = broken_json(path, Some(before), after, &name(path)) {
            return Err(error);
        }
    }
    for (path, _, after) in &contents {
        write_atomic(path, after)?;
    }
    Ok(counts
        .iter()
        .map(|(display, count)| format!("{display}: {count} değişiklik"))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn header_matches(line: &str, prefix: &str, expected: &str) -> bool {
    let Some(raw) = line.strip_prefix(prefix) else {
        return false;
    };
    let raw = raw.split('\t').next().unwrap_or("");
    if raw == "/dev/null"
        || raw.split('/').any(|part| part == "..")
        || (raw.starts_with('/') && raw != expected)
    {
        return false;
    }
    let strip = |text: &str| {
        text.strip_prefix("a/")
            .or_else(|| text.strip_prefix("b/"))
            .unwrap_or(text)
            .to_string()
    };
    let (stripped, expected) = (strip(raw), strip(expected));
    stripped == expected
        || expected.ends_with(&format!("/{stripped}"))
        || stripped.ends_with(&format!("/{expected}"))
}

/// Applies a one-file unified diff to `current`; `None` when a context line does not match.
pub fn apply_unified(patch: &str, current: &str, expected_path: &str) -> Option<String> {
    let lines: Vec<&str> = patch.split('\n').collect();
    if lines.len() < 4
        || !header_matches(lines[0], "--- ", expected_path)
        || !header_matches(lines[1], "+++ ", expected_path)
    {
        return None;
    }
    let trailing_newline = current.ends_with('\n');
    let mut original: Vec<&str> = current.split('\n').collect();
    if trailing_newline {
        original.pop();
    }
    let hunk = regex::Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").ok()?;
    let (mut output, mut source, mut index, mut saw_hunk) =
        (Vec::<String>::new(), 0usize, 2usize, false);
    while index < lines.len() {
        if lines[index].is_empty() && index == lines.len() - 1 {
            break;
        }
        let captures = hunk.captures(lines[index])?;
        let old_start: usize = captures.get(1)?.as_str().parse().ok()?;
        let old_count: usize = match captures.get(2) {
            Some(count) => count.as_str().parse().ok()?,
            None => 1,
        };
        let cursor = if old_count == 0 && old_start == 0 {
            0
        } else {
            old_start.saturating_sub(1)
        };
        if cursor < source || cursor > original.len() {
            return None;
        }
        output.extend(original[source..cursor].iter().map(|line| line.to_string()));
        source = cursor;
        index += 1;
        saw_hunk = true;
        let mut consumed = 0;
        while index < lines.len() && !lines[index].starts_with("@@ ") {
            let line = lines[index];
            if line.starts_with("\\ No newline at end of file") {
                index += 1;
                continue;
            }
            if line.is_empty() && index == lines.len() - 1 {
                break;
            }
            let (marker, content) = line.split_at(line.chars().next()?.len_utf8());
            match marker {
                " " | "-" => {
                    if source >= original.len() || original[source] != content {
                        return None;
                    }
                    if marker == " " {
                        output.push(content.to_string());
                    }
                    source += 1;
                    consumed += 1;
                }
                "+" => output.push(content.to_string()),
                _ => return None,
            }
            index += 1;
        }
        if consumed != old_count {
            return None;
        }
    }
    if !saw_hunk {
        return None;
    }
    output.extend(original[source..].iter().map(|line| line.to_string()));
    Some(output.join("\n") + if trailing_newline { "\n" } else { "" })
}

/// Unified patches for 1-10 files: all must apply before any is written.
pub fn patch(edits: &[(PathBuf, String, String)]) -> Result<String, WireError> {
    let mut updated = Vec::new();
    for (path, patch, display) in edits {
        let current = fs::read_to_string(path).unwrap_or_default();
        let result = apply_unified(patch, &current, display)
            .filter(|result| !result.is_empty() && *result != current)
            .ok_or_else(|| invalid("TERMINAL_PATCH_REJECTED", format!("{display}: yama dosyanın güncel içeriğiyle eşleşmedi; hiçbir dosya değiştirilmedi.")))?;
        if let Some(error) = broken_json(path, Some(&current), &result, display) {
            return Err(error);
        }
        updated.push((path, result));
    }
    for (path, content) in &updated {
        write_atomic(path, content)?;
    }
    Ok(edits
        .iter()
        .map(|(_, patch, _)| patch.as_str())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Searches file contents under `directory` without a shell (the pattern can never become a command): ripgrep when installed, grep otherwise.
pub fn search(pattern: &str, directory: &Path, glob: Option<&str>) -> Result<String, WireError> {
    if pattern.is_empty() || pattern.chars().count() > 200 || pattern.contains('\0') {
        return Err(invalid(
            "TERMINAL_SEARCH_INVALID",
            "Arama kalıbı boş ya da çok uzun.",
        ));
    }
    let glob = glob.filter(|glob| !glob.is_empty() && glob.len() <= 100);
    let path = shell::interactive_path()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let separator = if cfg!(windows) { ';' } else { ':' };
    let ripgrep = path
        .split(separator)
        .map(|folder| Path::new(folder).join(if cfg!(windows) { "rg.exe" } else { "rg" }))
        .find(|candidate| candidate.is_file());
    let mut command = match &ripgrep {
        Some(rg) => {
            let mut command = crate::process::command(rg);
            command.args([
                "--line-number",
                "--no-heading",
                "--color",
                "never",
                "--max-count",
                "20",
                "--max-columns",
                "300",
                "-g",
                "!.git",
                "-g",
                "!node_modules",
            ]);
            if let Some(glob) = glob {
                command.args(["-g", glob]);
            }
            command.args(["--", pattern, "."]);
            command
        }
        None if cfg!(windows) => {
            let mut command = crate::process::command("findstr");
            command
                .args(["/S", "/N", "/I"])
                .arg(format!("/C:{pattern}"))
                .arg(glob.unwrap_or("*"));
            command
        }
        None => {
            let mut command = crate::process::command("grep");
            command.args([
                "-rnI",
                "--exclude-dir=.git",
                "--exclude-dir=node_modules",
                "-m",
                "20",
            ]);
            if let Some(glob) = glob {
                command.arg(format!("--include={glob}"));
            }
            command.args(["-e", pattern, "."]);
            command
        }
    };
    command.current_dir(directory).env("PATH", &path);
    let snapshot = shell::run(command, 20, &Control::default());
    if snapshot.timed_out {
        return Err(WireError::new(
            "TERMINAL_SEARCH_TIMEOUT",
            "Arama 20 saniyede bitmedi; daha dar bir klasör veya kalıp dene.",
            "timeout",
        ));
    }
    let lines: Vec<&str> = snapshot.stdout.lines().take(200).collect();
    Ok(redact_secrets(&if lines.is_empty() {
        "(eşleşme yok)".to_string()
    } else {
        lines.join("\n")
    }))
}

// ----- undo -----

#[derive(Serialize, Deserialize)]
struct UndoFile {
    path: PathBuf,
    created: bool,
    #[serde(with = "base64_bytes")]
    before: Option<Vec<u8>>,
    after_sha256: String,
}

#[derive(Serialize, Deserialize)]
struct UndoRecord {
    id: String,
    root: PathBuf,
    created_at: i64,
    files: Vec<UndoFile>,
}

mod base64_bytes {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(bytes) => {
                serializer.serialize_some(&base64::engine::general_purpose::STANDARD.encode(bytes))
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        let text: Option<String> = Option::deserialize(deserializer)?;
        text.map(|text| {
            base64::engine::general_purpose::STANDARD
                .decode(text)
                .map_err(serde::de::Error::custom)
        })
        .transpose()
    }
}

/// One file as it was before a change: its bytes (none when the change created it).
pub struct Before {
    pub path: PathBuf,
    pub bytes: Option<Vec<u8>>,
}

impl Before {
    pub fn read(path: &Path) -> Option<Before> {
        if !path.exists() {
            return Some(Before {
                path: path.to_path_buf(),
                bytes: None,
            });
        }
        fs::read(path).ok().map(|bytes| Before {
            path: path.to_path_buf(),
            bytes: Some(bytes),
        })
    }
}

fn undo_folder(services: &Services) -> PathBuf {
    services.data_dir.file("TerminalUndo")
}

/// Keeps an opaque undo token for a verified change; contents and paths stay on this computer.
pub fn register_undo(services: &Services, root: &Path, changes: Vec<Before>) -> Option<String> {
    if changes.is_empty() || changes.len() > 10 {
        return None;
    }
    let mut files = Vec::new();
    for change in changes {
        let after = fs::read(&change.path).ok()?;
        files.push(UndoFile {
            created: change.bytes.is_none(),
            before: change.bytes,
            after_sha256: sha(&after),
            path: change.path,
        });
    }
    let id = uuid::Uuid::new_v4().to_string();
    let record = UndoRecord {
        id: id.clone(),
        root: root.to_path_buf(),
        created_at: Utc::now().timestamp(),
        files,
    };
    let folder = undo_folder(services);
    fs::create_dir_all(&folder).ok()?;
    fs::write(
        folder.join(format!("{id}.json")),
        serde_json::to_vec(&record).ok()?,
    )
    .ok()?;
    prune(&folder);
    Some(id)
}

fn prune(folder: &Path) {
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(folder)
        .map(|items| {
            items
                .filter_map(|item| item.ok())
                .filter_map(|item| Some((item.metadata().ok()?.modified().ok()?, item.path())))
                .collect()
        })
        .unwrap_or_default();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    let week = std::time::Duration::from_secs((UNDO_DAYS * 86_400) as u64);
    for (index, (modified, path)) in entries.iter().enumerate() {
        if index >= UNDO_KEPT || modified.elapsed().unwrap_or_default() > week {
            let _ = fs::remove_file(path);
        }
    }
}

/// Restores a record only when every file still has exactly the bytes the change left: a stale button never overwrites newer work.
pub fn undo(services: &Services, undo_id: &str, root: &Path) -> Result<String, WireError> {
    let missing = || {
        invalid(
            "TERMINAL_UNDO_INVALID",
            "Geri alma kaydı bulunamadı, süresi doldu veya bu klasör iznine ait değil.",
        )
    };
    let id = uuid::Uuid::parse_str(undo_id)
        .map_err(|_| missing())?
        .to_string();
    let file = undo_folder(services).join(format!("{id}.json"));
    let record: UndoRecord =
        serde_json::from_slice(&fs::read(&file).map_err(|_| missing())?).map_err(|_| missing())?;
    if record.id != id
        || record.root != root
        || Utc::now().timestamp() - record.created_at > UNDO_DAYS * 86_400
        || record.files.is_empty()
    {
        return Err(missing());
    }
    let mut current = Vec::new();
    for item in &record.files {
        let path = policy::resolve_existing_file(&item.path.to_string_lossy(), root, root);
        let bytes = path.as_ref().and_then(|path| fs::read(path).ok());
        match (path, bytes) {
            (Some(path), Some(bytes)) if sha(&bytes) == item.after_sha256 => current.push((path, bytes, item)),
            _ => {
                return Err(invalid(
                    "TERMINAL_UNDO_STALE",
                    "Dosyalardan biri bu işlemden sonra değişmiş; yeni değişikliği korumak için geri alma yapılmadı.",
                ))
            }
        }
    }
    let mut restored: Vec<&PathBuf> = Vec::new();
    for (path, _, item) in &current {
        let done = match &item.before {
            None if item.created => fs::remove_file(path).is_ok(),
            Some(before) => crate::files::write_atomic(path, before).is_ok(),
            None => false,
        };
        if !done {
            for (path, bytes, _) in current
                .iter()
                .filter(|(path, _, _)| restored.contains(&path))
            {
                let _ = crate::files::write_atomic(path, bytes);
            }
            return Err(failure(
                "TERMINAL_UNDO_FAILED",
                "Geri alma tamamlanamadı; dosyaların durumu doğrulanamadı.",
            ));
        }
        restored.push(path);
    }
    let _ = fs::remove_file(file);
    Ok(format!("{} dosya önceki haline döndürüldü.", current.len()))
}

/// Undoes a terminal change by its id alone, inside the folder its own record names (the window's "Geri al"; the record decides the scope).
pub fn undo_recorded(services: &Services, undo_id: &str) -> Result<String, WireError> {
    let missing = || {
        invalid(
            "TERMINAL_UNDO_INVALID",
            "Geri alma kaydı bulunamadı veya süresi doldu.",
        )
    };
    let id = uuid::Uuid::parse_str(undo_id)
        .map_err(|_| missing())?
        .to_string();
    let bytes =
        fs::read(undo_folder(services).join(format!("{id}.json"))).map_err(|_| missing())?;
    let record: UndoRecord = serde_json::from_slice(&bytes).map_err(|_| missing())?;
    undo(services, &id, &record.root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_patch_applies_only_on_matching_context() {
        let current = "a\nb\nc\n";
        let patch = "--- a/x.txt\n+++ b/x.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n";
        assert_eq!(
            apply_unified(patch, current, "x.txt").as_deref(),
            Some("a\nB\nc\n")
        );
        assert_eq!(apply_unified(patch, "a\nz\nc\n", "x.txt"), None);
        assert_eq!(apply_unified(patch, current, "other.txt"), None);
    }

    #[test]
    fn edits_are_all_or_nothing_keep_json_valid_and_undo_restores() {
        let services = Services::temporary();
        let root = dunce::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("wiqo-edit-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let (a, json) = (root.join("a.ts"), root.join("package.json"));
        fs::write(&a, "const x = 1;\n").unwrap();
        fs::write(&json, "{\"a\": 1, \"b\": 2}").unwrap();
        let broken = [Replacement {
            path: json.clone(),
            display: "package.json".into(),
            find: ", \"b\": 2".into(),
            replace: ", \"b\": 2,".into(),
            replace_all: false,
        }];
        assert_eq!(
            replace(&broken).unwrap_err().code,
            "TERMINAL_EDIT_INVALID_JSON"
        );
        let missing = [
            Replacement {
                path: a.clone(),
                display: "a.ts".into(),
                find: "1".into(),
                replace: "2".into(),
                replace_all: false,
            },
            Replacement {
                path: a.clone(),
                display: "a.ts".into(),
                find: "nope".into(),
                replace: "x".into(),
                replace_all: false,
            },
        ];
        assert_eq!(
            replace(&missing).unwrap_err().code,
            "TERMINAL_EDIT_NOT_FOUND"
        );
        assert_eq!(
            fs::read_to_string(&a).unwrap(),
            "const x = 1;\n",
            "nothing was written"
        );
        let before = vec![Before::read(&a).unwrap()];
        replace(&missing[..1]).unwrap();
        let id = register_undo(&services, &root, before).unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "const x = 2;\n");
        undo(&services, &id, &root).unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "const x = 1;\n");
        assert_eq!(
            undo(&services, &id, &root).unwrap_err().code,
            "TERMINAL_UNDO_INVALID"
        );
    }
}
