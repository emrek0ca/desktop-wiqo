//! The file rules every desktop applies the same way: which names are sensitive, which folders are the person's own, what a valid path or file name is.
//! The lists live in `contracts/companion/file-policy.json`.

use std::collections::HashSet;
use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    own_folders: Vec<String>,
    sensitive_names: Vec<String>,
    sensitive_extensions: Vec<String>,
    sensitive_words: Vec<String>,
    folder_aliases: std::collections::HashMap<String, String>,
}

#[derive(Debug)]
pub struct FilePolicy {
    own_folders: HashSet<String>,
    sensitive_names: Vec<String>,
    sensitive_extensions: HashSet<String>,
    sensitive_words: Vec<String>,
    folder_aliases: std::collections::HashMap<String, String>,
}

impl FilePolicy {
    pub fn get() -> &'static FilePolicy {
        static POLICY: OnceLock<FilePolicy> = OnceLock::new();
        POLICY.get_or_init(|| {
            let file: File =
                serde_json::from_str(crate::FILE_POLICY_JSON).expect("file-policy.json is valid");
            FilePolicy {
                own_folders: file.own_folders.into_iter().collect(),
                sensitive_names: file.sensitive_names,
                sensitive_extensions: file.sensitive_extensions.into_iter().collect(),
                sensitive_words: file.sensitive_words,
                folder_aliases: file.folder_aliases,
            }
        })
    }

    /// Key material and credential stores: never read automatically and left out of searches. `raw` may be a path or a file name.
    pub fn is_sensitive(&self, raw: &str) -> bool {
        let lowered = raw.to_lowercase().replace('\\', "/");
        let parts: Vec<&str> = lowered.split('/').filter(|part| !part.is_empty()).collect();
        if parts.iter().any(|part| {
            self.sensitive_names
                .iter()
                .any(|name| part == name || part.starts_with(&format!("{name}.")))
        }) {
            return true;
        }
        if let Some(last) = parts.last() {
            if let Some((_, extension)) = last.rsplit_once('.') {
                if self.sensitive_extensions.contains(extension) {
                    return true;
                }
            }
            if *last == ".env" || last.starts_with(".env.") {
                return true;
            }
        }
        self.sensitive_words
            .iter()
            .any(|word| lowered.contains(word.as_str()))
    }

    /// The path is (a place inside) Desktop, Documents or Downloads of some user's home.
    pub fn is_own_folder(&self, raw: &str) -> bool {
        match home_relative_parts(raw) {
            Some(parts) => parts
                .first()
                .is_some_and(|first| self.own_folders.contains(&first.to_lowercase())),
            None => false,
        }
    }

    /// "masaüstü" -> "Desktop" and the other folder names a Turkish desktop shows; the key is the folded spelling.
    pub fn folder_alias(&self, folded: &str) -> Option<&str> {
        self.folder_aliases.get(folded).map(String::as_str)
    }
}

fn home_relative_parts(raw: &str) -> Option<Vec<String>> {
    let mut path = raw.trim().replace('\\', "/");
    if let Some(rest) = path.strip_prefix("~/") {
        path = rest.to_string();
    } else if let Some(rest) = path
        .strip_prefix("/Users/")
        .or_else(|| path.strip_prefix("/home/"))
    {
        let mut parts = rest.split('/').filter(|part| !part.is_empty());
        parts.next()?;
        path = parts.collect::<Vec<_>>().join("/");
    } else if path.len() > 10
        && path.as_bytes()[1] == b':'
        && path[2..].to_lowercase().starts_with("/users/")
    {
        let mut parts = path[9..].split('/').filter(|part| !part.is_empty());
        parts.next()?;
        path = parts.collect::<Vec<_>>().join("/");
    } else {
        return None;
    }
    let parts: Vec<String> = path
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    if parts.iter().any(|part| part == ".." || part == ".") {
        None
    } else {
        Some(parts)
    }
}

/// A path a phone or the server may name: absolute or `~/`, no `..`, no control characters, at most 2,048 characters. (Windows drive paths are also accepted.)
pub fn is_valid_local_path(raw: &str) -> bool {
    let path = raw.trim();
    if path.is_empty()
        || path.chars().count() > 2_048
        || path.contains('\0')
        || path.contains('\n')
        || path.contains('\r')
    {
        return false;
    }
    let absolute = path.starts_with('/')
        || path.starts_with("~/")
        || (path.len() >= 3
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':'
            && (path.as_bytes()[2] == b'\\' || path.as_bytes()[2] == b'/'));
    absolute && !path.split(['/', '\\']).any(|part| part == "..")
}

/// A single file name on the Desktop: not empty, no separators, no control characters, at most 255 characters.
pub fn is_valid_file_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && trimmed.chars().count() <= 255
        && trimmed != "."
        && trimmed != ".."
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
        && !trimmed.chars().any(|c| (c as u32) < 32 || c as u32 == 127)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_and_keys_are_sensitive_wherever_they_sit() {
        let policy = FilePolicy::get();
        for raw in [
            "~/.ssh/id_rsa",
            "C:\\Users\\Ada\\.aws\\credentials",
            "notlar/sifre listesi.txt",
            "~/Desktop/anahtar.pem",
            ".env.production",
            "~/Documents/Şifreler.xlsx",
            "backup code.txt",
        ] {
            assert!(policy.is_sensitive(raw), "{raw}");
        }
        for raw in ["~/Desktop/fatura.pdf", "rapor.docx", "~/Downloads/foto.jpg"] {
            assert!(!policy.is_sensitive(raw), "{raw}");
        }
    }

    #[test]
    fn own_folders_are_recognised_on_every_platform() {
        let policy = FilePolicy::get();
        assert!(policy.is_own_folder("~/Desktop/a.pdf"));
        assert!(policy.is_own_folder("/Users/ada/Downloads"));
        assert!(policy.is_own_folder("/home/ada/Documents/x"));
        assert!(policy.is_own_folder("C:\\Users\\Ada\\Desktop\\x.txt"));
        assert!(!policy.is_own_folder("~/Library/Mail"));
        assert!(!policy.is_own_folder("/etc/hosts"));
        assert!(!policy.is_own_folder("~/Desktop/../.ssh"));
    }

    #[test]
    fn paths_and_names_are_checked_the_same_way_everywhere() {
        assert!(is_valid_local_path("~/Desktop/a.txt"));
        assert!(is_valid_local_path("/tmp/x"));
        assert!(is_valid_local_path("C:\\Users\\Ada\\x.txt"));
        assert!(!is_valid_local_path("relative/path"));
        assert!(!is_valid_local_path("~/Desktop/../x"));
        assert!(!is_valid_local_path(""));
        assert!(is_valid_file_name("rapor.pdf"));
        assert!(!is_valid_file_name("a/b"));
        assert!(!is_valid_file_name(".."));
        assert!(!is_valid_file_name("a\u{1}b"));
    }
}
