//! Local paths the way a person names them, and the checks every file operation shares: Turkish folder names, `~`, no symlink at the end of a path,
//! readable errors that suggest the real name.

use std::cmp::Ordering;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use wiqo_contract::names::{normalize, score};
use wiqo_contract::{is_valid_local_path, FilePolicy};
use wiqo_core::wire::WireError;

use crate::op::tool_failure;

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

pub fn desktop_dir() -> Option<PathBuf> {
    dirs::desktop_dir().or_else(|| Some(home().join("Desktop")))
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// `~/Desktop` for a path under the home folder, the path itself otherwise.
pub fn abbreviate(path: &Path) -> String {
    match path.strip_prefix(home()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", display_path(rest)),
        Err(_) => display_path(path),
    }
}

/// The model writes the folder names a Turkish person sees ("~/Masaüstü/rapor.pdf") or guesses a home folder ("/Users/emre/Desktop"); on disk they are
/// ~/Desktop and the real home. Only the first folder under home is translated; everything else stays as written.
pub fn canonical_user_path(raw: &str) -> String {
    let home_path = home();
    let home_text = display_path(&home_path);
    // Windows paths arrive with backslashes (C:\Users\emre\Masaüstü); compared with forward slashes, like `display_path`.
    #[cfg(windows)]
    let raw = &raw.replace('\\', "/");
    let trimmed = raw.trim();
    let mut path = if let Some(rest) = trimmed.strip_prefix("~/") {
        format!("{home_text}/{rest}")
    } else if trimmed == "~" {
        home_text.clone()
    } else {
        trimmed.to_string()
    };
    let components: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if components.len() >= 2 && (components[0] == "Users" || components[0] == "home") {
        let guessed = format!("/{}/{}", components[0], components[1]);
        if guessed != home_text && !Path::new(&guessed).exists() {
            path = std::iter::once(home_text.clone())
                .chain(components[2..].iter().map(|part| part.to_string()))
                .collect::<Vec<_>>()
                .join("/");
        }
    } else if components.len() >= 3
        && components[0].len() == 2
        && components[0].ends_with(':')
        && components[1].eq_ignore_ascii_case("Users")
    {
        // A guessed Windows home ("C:/Users/emre/Desktop") on a computer whose user is someone else.
        let guessed = format!("{}/{}/{}", components[0], components[1], components[2]);
        if !guessed.eq_ignore_ascii_case(&home_text) && !Path::new(&guessed).exists() {
            path = std::iter::once(home_text.clone())
                .chain(components[3..].iter().map(|part| part.to_string()))
                .collect::<Vec<_>>()
                .join("/");
        }
    }
    let Some(rest) = path.strip_prefix(&format!("{home_text}/")) else {
        return path;
    };
    let mut parts: Vec<String> = rest
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    let Some(first) = parts.first().cloned() else {
        return path;
    };
    let key = normalize(&first).replace(' ', "");
    if let Some(english) = FilePolicy::get().folder_alias(&key) {
        if !home_path.join(&first).exists() {
            parts[0] = english.to_string();
        }
    }
    std::iter::once(home_text)
        .chain(parts)
        .collect::<Vec<_>>()
        .join("/")
}

pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

pub fn is_dir(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

pub fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// The real, absolute place a path names. An existing entry must not itself be a symlink (a model-produced path never silently operates on a link's target);
/// a missing last component is allowed when asked for, if its folder exists.
pub fn resolve(raw: &str, allow_missing_last: bool) -> Option<PathBuf> {
    if !is_valid_local_path(raw) {
        return None;
    }
    let expanded = PathBuf::from(canonical_user_path(raw));
    if !expanded.is_absolute() {
        return None;
    }
    if fs::symlink_metadata(&expanded).is_ok() {
        if is_symlink(&expanded) {
            return None;
        }
        return dunce::canonicalize(&expanded).ok();
    }
    if !allow_missing_last {
        return None;
    }
    let parent = expanded.parent()?;
    if !parent.exists() || is_symlink(parent) || !is_dir(parent) {
        return None;
    }
    let resolved_parent = dunce::canonicalize(parent).ok()?;
    Some(resolved_parent.join(expanded.file_name()?))
}

/// Case-insensitive, with numbers compared as numbers ("dosya2" before "dosya10"), like the file manager sorts.
pub fn natural_cmp(left: &str, right: &str) -> Ordering {
    let (mut a, mut b) = (left.chars().peekable(), right.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return left.cmp(right),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let number = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>| {
                    let mut digits = String::new();
                    while let Some(c) = chars.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(c);
                        chars.next();
                    }
                    digits.trim_start_matches('0').to_string()
                };
                let (da, db) = (number(&mut a), number(&mut b));
                let order = da.len().cmp(&db.len()).then_with(|| da.cmp(&db));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

pub fn iso(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `FileSystemEntry`: one file or folder as the phone shows it.
#[derive(Debug, Clone, Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Entry {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_path: Option<String>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_type: Option<String>,
    pub byte_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    pub is_directory: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo_id: Option<String>,
}

pub fn entry(path: &Path) -> Option<Entry> {
    let meta = fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_string())
        .filter(|ext| !ext.is_empty());
    Some(Entry {
        path: display_path(dunce::simplified(path)),
        name: path.file_name()?.to_string_lossy().to_string(),
        file_type: if meta.is_dir() { None } else { extension },
        byte_count: if meta.is_dir() { 0 } else { meta.len() as i64 },
        modified_at: meta.modified().ok().map(iso),
        is_directory: meta.is_dir(),
        ..Entry::default()
    })
}

/// Makes a filesystem failure readable: a missing permission names the setting that fixes it, anything else gets the operation's own sentence.
pub fn local_file_error(code: &str, fallback: &str, error: &std::io::Error) -> WireError {
    if error.kind() == ErrorKind::PermissionDenied {
        let message = if cfg!(target_os = "macos") {
            "Wiqo bu klasörü okuyamıyor; bilgisayardaki Wiqo penceresinde İzinler bölümünden klasör iznini (gerekirse Tam Disk Erişimi'ni) ver."
        } else {
            "Wiqo'nun bu klasöre erişme izni yok. Klasörün izinlerini kontrol edin."
        };
        return WireError::new("FULL_DISK_ACCESS_REQUIRED", message, "permission");
    }
    tool_failure(code, fallback)
}

/// "Not found" that helps the next attempt: the folder that was searched and the closest names in it, so the model can retry with the real file name.
pub fn missing_path_error(raw: &str, message: &str) -> WireError {
    let path = PathBuf::from(canonical_user_path(raw));
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let wanted = normalize(
        &path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_default(),
    );
    let mut detail = message.to_string();
    match fs::read_dir(&parent) {
        Ok(names) => {
            let mut ranked: Vec<(String, f64)> = names
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .filter(|name| !name.starts_with('.'))
                .map(|name| {
                    let stem = Path::new(&name)
                        .file_stem()
                        .map(|stem| stem.to_string_lossy().to_string())
                        .unwrap_or_else(|| name.clone());
                    let similarity = score(&stem, &wanted);
                    (name, similarity)
                })
                .filter(|(_, similarity)| *similarity >= 0.5)
                .collect();
            ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
            let suggestions: Vec<String> =
                ranked.into_iter().take(3).map(|(name, _)| name).collect();
            let folder = abbreviate(&parent);
            if suggestions.is_empty() {
                detail.push_str(&format!(
                    " ({folder} içinde bu adla bir öğe yok; önce klasörü listele.)"
                ));
            } else {
                detail.push_str(&format!(
                    " {folder} içindeki benzer adlar: {}.",
                    suggestions.join(", ")
                ));
            }
        }
        Err(_) => detail.push_str(&format!(" ({} klasörü de yok.)", abbreviate(&parent))),
    }
    tool_failure("FILE_NOT_FOUND", detail)
}

pub fn sensitive_blocked(what: &str) -> WireError {
    WireError::new(
        "SENSITIVE_FILE_BLOCKED",
        format!("Kimlik bilgisi içerebilecek {what} Wiqo tarafından okunamaz."),
        "permission",
    )
}

fn is_likely_text(path: &Path) -> bool {
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    [
        "txt", "md", "csv", "tsv", "log", "json", "xml", "html", "htm", "yml", "yaml", "ini",
        "srt", "swift", "py", "js", "ts",
    ]
    .contains(&ext.as_str())
}

/// Text the way people save it on a Turkish computer: UTF-8 first, then Windows-1254, then Latin-1, so an old .txt does not fail as "okunamadı".
pub fn decode_text(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    let (text, _, had_errors) = encoding_rs::WINDOWS_1254.decode(bytes);
    if !had_errors {
        return text.to_string();
    }
    bytes.iter().map(|&byte| byte as char).collect()
}

pub fn read_text(path: &Path) -> std::io::Result<String> {
    Ok(decode_text(&fs::read(path)?))
}

pub fn text_if_likely(path: &Path, bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes)
        .ok()
        .map(str::to_string)
        .or_else(|| is_likely_text(path).then(|| decode_text(bytes)))
}

pub fn mime_type(path: &Path) -> String {
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turkish_folder_names_and_guessed_homes_land_on_the_real_folders() {
        let home = display_path(&home());
        assert_eq!(
            canonical_user_path("~/Masaüstü/rapor.pdf"),
            format!("{home}/Desktop/rapor.pdf")
        );
        assert_eq!(
            canonical_user_path("~/İndirilenler"),
            format!("{home}/Downloads")
        );
        assert_eq!(
            canonical_user_path("/Users/biri-olmayan-xyz/Documents/a.txt"),
            format!("{home}/Documents/a.txt")
        );
        assert_eq!(canonical_user_path("/tmp/x"), "/tmp/x");
    }

    #[test]
    fn numbers_inside_names_sort_as_numbers() {
        let mut names = vec!["dosya10", "Dosya2", "dosya1"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["dosya1", "Dosya2", "dosya10"]);
    }

    #[test]
    fn a_symlink_at_the_end_of_a_path_is_never_followed() {
        let dir = std::env::temp_dir().join(format!("wiqo-fsx-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let real = dir.join("gercek.txt");
        fs::write(&real, "x").unwrap();
        #[cfg(unix)]
        {
            let link = dir.join("baglanti.txt");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(resolve(&link.to_string_lossy(), false).is_none());
        }
        assert!(resolve(&real.to_string_lossy(), false).is_some());
        assert!(resolve(&dir.join("yeni.txt").to_string_lossy(), false).is_none());
        assert!(resolve(&dir.join("yeni.txt").to_string_lossy(), true).is_some());
        assert!(resolve("relative.txt", true).is_none());
    }

    #[test]
    fn old_turkish_text_files_are_still_readable() {
        assert_eq!(decode_text("Şğü".as_bytes()), "Şğü");
        assert_eq!(decode_text(&[0xDE, 0xF0, 0xFC]), "Şğü");
    }
}
