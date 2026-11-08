//! Finding files. On a Mac the Spotlight index answers in well under a second (by name, kind, date and, when asked, by what is inside); everywhere else, and
//! when Spotlight has nothing to say, a bounded walk over the searched folders takes over.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use serde_json::Value;
use wiqo_contract::names::fold;
use wiqo_contract::FilePolicy;
use wiqo_core::engine::{Context, Lane, Operation, Outcome};

use crate::fsx::{self, Entry};
use crate::op::{data, error, outcome, payload, tool_failure, Done, Result};
use crate::Op;

const PAGE_SIZE: usize = 12;
const CANDIDATE_LIMIT: usize = 400;
const SURVEY_LIMIT: usize = 1_000;
const MAX_ALTERNATIVES: usize = 6;
const WALK_ENTRIES: usize = 60_000;
const WALK_SECONDS: u64 = 8;

/// Places that hold other programs' files, not the person's: they would drown a search for "rapor" in build output and package checkouts.
const NOISY_COMPONENTS: [&str; 15] = [
    "node_modules",
    "deriveddata",
    ".git",
    "pods",
    "sourcepackages",
    "checkouts",
    ".build",
    "venv",
    ".venv",
    "__pycache__",
    "site-packages",
    "library",
    ".trash",
    "caches",
    "target",
];
const NOISY_SUFFIXES: [&str; 6] = [
    ".app",
    ".xcodeproj",
    ".xcworkspace",
    ".framework",
    ".photoslibrary",
    ".lproj",
];

fn extensions(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "pdf" => &["pdf"],
        "document" => &["doc", "docx", "rtf", "odt", "pages"],
        "spreadsheet" => &["xls", "xlsx", "csv", "tsv", "numbers", "ods"],
        "presentation" => &["ppt", "pptx", "key", "odp"],
        "image" => &[
            "jpg", "jpeg", "png", "gif", "heic", "webp", "tiff", "bmp", "svg",
        ],
        "text" => &[
            "txt", "md", "markdown", "log", "json", "xml", "yml", "yaml", "html",
        ],
        "archive" => &["zip", "rar", "7z", "tar", "gz", "tgz", "dmg"],
        "audio" => &["mp3", "m4a", "wav", "aac", "flac"],
        "video" => &["mp4", "mov", "mkv", "avi", "m4v"],
        _ => return None,
    })
}

#[cfg(target_os = "macos")]
fn uniform_types(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "pdf" => &["com.adobe.pdf"],
        "document" => &[
            "org.openxmlformats.wordprocessingml.document",
            "com.microsoft.word.doc",
            "public.rtf",
            "com.apple.iwork.pages.sffpages",
            "org.oasis-open.opendocument.text",
        ],
        "spreadsheet" => &[
            "org.openxmlformats.spreadsheetml.sheet",
            "com.microsoft.excel.xls",
            "public.comma-separated-values-text",
            "com.apple.iwork.numbers.sffnumbers",
            "org.oasis-open.opendocument.spreadsheet",
        ],
        "presentation" => &[
            "org.openxmlformats.presentationml.presentation",
            "com.microsoft.powerpoint.ppt",
            "com.apple.iwork.keynote.sffkey",
            "org.oasis-open.opendocument.presentation",
        ],
        "image" => &["public.image"],
        "audio" => &["public.audio"],
        "video" => &["public.movie"],
        "archive" => &["public.archive"],
        "folder" => &["public.folder"],
        _ => return None,
    })
}

/// `FileSearchPayload`.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct SearchRequest {
    query: String,
    cursor: Option<String>,
    folder: Option<String>,
    everywhere: Option<bool>,
    kind: Option<String>,
    modified_within_days: Option<i64>,
    content: Option<bool>,
    sort: Option<String>,
    alternatives: Option<Vec<String>>,
    survey: Option<bool>,
}

#[derive(Clone)]
struct Spec {
    /// The query's words first, then each alternative's: a name matching any group counts.
    groups: Vec<Vec<String>>,
    limit: usize,
    roots: Vec<PathBuf>,
    kind: Option<String>,
    since: Option<SystemTime>,
    content: bool,
    recent: bool,
}

/// Words worth matching: letters, digits and a few name characters. Anything that could change the meaning of a search expression is dropped.
fn tokens(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric() || "._-@#".contains(*c))
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .take(8)
        .collect()
}

/// Desktop, Documents and Downloads where the system really keeps them (OneDrive moves them on Windows).
fn own_folders() -> Vec<PathBuf> {
    ["~/Desktop", "~/Documents", "~/Downloads"]
        .iter()
        .map(|name| PathBuf::from(fsx::canonical_user_path(name)))
        .filter(|path| path.exists())
        .collect()
}

fn build(request: &SearchRequest, now: SystemTime) -> Result<Spec> {
    let roots = if request.everywhere == Some(true) {
        vec![fsx::home()]
    } else if let Some(folder) = &request.folder {
        let resolved = fsx::resolve(folder, false)
            .filter(|path| fsx::is_dir(path))
            .ok_or_else(|| {
                fsx::missing_path_error(folder, &format!("\"{folder}\" klasörü bulunamadı."))
            })?;
        vec![resolved]
    } else {
        own_folders()
    };
    if roots.is_empty() {
        return Err(tool_failure("SEARCH_FAILED", "Aranacak klasör bulunamadı."));
    }
    let words = tokens(&request.query);
    let alternatives = request
        .alternatives
        .iter()
        .flatten()
        .take(MAX_ALTERNATIVES)
        .map(|alternative| tokens(alternative))
        .filter(|group| !group.is_empty());
    let groups: Vec<Vec<String>> = std::iter::once(words.clone())
        .chain(alternatives)
        .filter(|group| !group.is_empty())
        .collect();
    Ok(Spec {
        groups,
        limit: CANDIDATE_LIMIT,
        roots,
        kind: request.kind.clone(),
        since: request
            .modified_within_days
            .map(|days| now - Duration::from_secs(days.max(0) as u64 * 86_400)),
        content: request.content == Some(true),
        recent: request.sort.as_deref() == Some("recent")
            || (words.is_empty() && request.sort.as_deref() != Some("relevance")),
    })
}

fn is_noise(path: &str) -> bool {
    let lowered = path.to_lowercase().replace('\\', "/");
    let parts: Vec<&str> = lowered.split('/').filter(|part| !part.is_empty()).collect();
    if parts
        .iter()
        .any(|part| NOISY_COMPONENTS.contains(part) || part.starts_with('.'))
    {
        return true;
    }
    parts.split_last().is_some_and(|(_, folders)| {
        folders
            .iter()
            .any(|folder| NOISY_SUFFIXES.iter().any(|suffix| folder.ends_with(suffix)))
    })
}

fn admissible(path: &str) -> bool {
    if path.chars().count() > 500 || is_noise(path) {
        return false;
    }
    let policy = FilePolicy::get();
    let resolved = dunce::canonicalize(path)
        .map(|resolved| resolved.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string());
    !policy.is_sensitive(path) && !policy.is_sensitive(&resolved)
}

/// Higher is a better match: the name is the query, starts with it, holds every word; shorter paths win ties, since what people mean is rarely buried
/// in a project checkout.
fn score(name: &str, path: &str, groups: &[Vec<String>]) -> f64 {
    groups
        .iter()
        .map(|group| group_score(name, path, group))
        .fold(0.0, f64::max)
}

fn group_score(name: &str, path: &str, tokens: &[String]) -> f64 {
    if tokens.is_empty() {
        return 0.0;
    }
    let stem = Path::new(name)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| name.to_string());
    let folded = fold(&stem);
    let query = fold(&tokens.join(" "));
    let mut total = if folded == query {
        6.0
    } else if folded.starts_with(&query) {
        3.0
    } else if folded.contains(&query) {
        2.0
    } else {
        0.0
    };
    total += tokens
        .iter()
        .filter(|token| folded.contains(&fold(token)))
        .count() as f64;
    total - path.split('/').count() as f64 * 0.02
}

fn modified(entry: &Entry) -> &str {
    entry.modified_at.as_deref().unwrap_or("")
}

fn rank(mut entries: Vec<Entry>, spec: &Spec) -> Vec<Entry> {
    entries.sort_by(|left, right| {
        if !spec.recent {
            let order = score(&right.name, &right.path, &spec.groups)
                .partial_cmp(&score(&left.name, &left.path, &spec.groups))
                .unwrap_or(Ordering::Equal);
            if order != Ordering::Equal {
                return order;
            }
        }
        modified(right)
            .cmp(modified(left))
            .then_with(|| fsx::natural_cmp(&left.path, &right.path))
    });
    entries
}

fn found_entry(path: &Path) -> Option<Entry> {
    let mut found = fsx::entry(path)?;
    found.file_type = if found.is_directory {
        None
    } else {
        found.file_type.map(|ext| ext.to_lowercase())
    };
    Some(found)
}

fn newer_than(entry: &Entry, since: SystemTime) -> bool {
    entry
        .modified_at
        .as_deref()
        .is_none_or(|stamp| stamp >= fsx::iso(since).as_str())
}

/// A bounded walk, level by level (what people look for is rarely buried deep in a project checkout, so the shallow places are seen first):
/// at most 60,000 entries or 8 seconds, names and kinds only. Hidden folders, package folders and app bundles are never entered.
fn walk(spec: &Spec) -> Vec<Entry> {
    let deadline = Instant::now() + Duration::from_secs(WALK_SECONDS);
    let folded: Vec<Vec<String>> = spec
        .groups
        .iter()
        .map(|group| group.iter().map(|word| fold(word)).collect())
        .collect();
    let wanted = spec.kind.as_deref().and_then(extensions);
    let (mut visited, mut found) = (0usize, Vec::new());
    let mut queue: std::collections::VecDeque<(PathBuf, usize)> =
        spec.roots.iter().map(|root| (root.clone(), 0)).collect();
    while let Some((folder, depth)) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        let mut entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for item in entries {
            visited += 1;
            if visited > WALK_ENTRIES || Instant::now() >= deadline || found.len() >= spec.limit {
                return found;
            }
            let name = item.file_name().to_string_lossy().to_string();
            let lowered = name.to_lowercase();
            let Ok(file_type) = item.file_type() else {
                continue;
            };
            if file_type.is_symlink()
                || name.starts_with('.')
                || NOISY_COMPONENTS.contains(&lowered.as_str())
            {
                continue;
            }
            let is_dir = file_type.is_dir();
            if is_dir
                && depth < 12
                && !NOISY_SUFFIXES
                    .iter()
                    .any(|suffix| lowered.ends_with(suffix))
            {
                queue.push_back((item.path(), depth + 1));
            }
            if is_dir != (spec.kind.as_deref() == Some("folder")) {
                continue;
            }
            if let Some(extensions) = wanted {
                let extension = Path::new(&name)
                    .extension()
                    .map(|ext| ext.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if !extensions.contains(&extension.as_str()) {
                    continue;
                }
            }
            let folded_name = fold(&name);
            if !folded.is_empty()
                && !folded
                    .iter()
                    .any(|group| group.iter().all(|word| folded_name.contains(word.as_str())))
            {
                continue;
            }
            let Some(candidate) = found_entry(&item.path()) else {
                continue;
            };
            if spec
                .since
                .is_some_and(|since| !newer_than(&candidate, since))
            {
                continue;
            }
            if admissible(&candidate.path) {
                found.push(candidate);
            }
        }
    }
    found
}

/// The Spotlight expression for a search. Names always match case- and diacritic-insensitively; with `content`, a word may also match inside the file.
#[cfg(target_os = "macos")]
fn spotlight_predicate(spec: &Spec) -> String {
    let mut parts: Vec<String> = Vec::new();
    let several = spec.groups.len() > 1;
    let clauses: Vec<String> = spec
        .groups
        .iter()
        .map(|group| {
            let each: Vec<String> = group
                .iter()
                .map(|token| {
                    let name = format!("kMDItemDisplayName == \"*{token}*\"cd");
                    if spec.content {
                        format!("({name} || kMDItemTextContent == \"*{token}*\"cd)")
                    } else {
                        name
                    }
                })
                .collect();
            let joined = each.join(" && ");
            if several && each.len() > 1 {
                format!("({joined})")
            } else {
                joined
            }
        })
        .collect();
    if !clauses.is_empty() {
        parts.push(if several {
            format!("({})", clauses.join(" || "))
        } else {
            clauses[0].clone()
        });
    }
    match spec.kind.as_deref() {
        Some("text") => parts.push("(kMDItemContentTypeTree == \"public.plain-text\" && kMDItemContentTypeTree != \"public.source-code\")".into()),
        Some(kind) => {
            if let Some(types) = uniform_types(kind) {
                parts.push(format!("({})", types.iter().map(|uti| format!("kMDItemContentTypeTree == \"{uti}\"")).collect::<Vec<_>>().join(" || ")));
            }
        }
        None if spec.groups.is_empty() => parts.push("kMDItemContentType != \"public.folder\"".into()),
        None => {}
    }
    if let Some(since) = spec.since {
        parts.push(format!(
            "kMDItemFSContentChangeDate >= $time.iso({})",
            fsx::iso(since)
        ));
    }
    parts.join(" && ")
}

/// `None` when Spotlight could not answer (the caller walks instead).
#[cfg(target_os = "macos")]
fn spotlight(spec: &Spec) -> Option<Vec<Entry>> {
    let mut command = crate::process::command("/usr/bin/mdfind");
    for root in &spec.roots {
        command.arg("-onlyin").arg(root);
    }
    command.arg(spotlight_predicate(spec));
    let output = crate::process::run_with_timeout(command, Duration::from_secs(8))?;
    if !output.status.success() {
        return None;
    }
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout)
        .lines()
        .take(spec.limit * 3)
    {
        if !admissible(line) || !seen.insert(line.to_string()) {
            continue;
        }
        let Some(found) = found_entry(Path::new(line)) else {
            continue;
        };
        if spec.since.is_some_and(|since| !newer_than(&found, since)) {
            continue;
        }
        entries.push(found);
        if entries.len() >= spec.limit {
            break;
        }
    }
    Some(entries)
}

#[cfg(not(target_os = "macos"))]
fn spotlight(_spec: &Spec) -> Option<Vec<Entry>> {
    None
}

fn scope_name(root: &Path) -> String {
    if root == fsx::home() {
        return "Ev klasörü".into();
    }
    match root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .as_deref()
    {
        Some("Desktop") => "Masaüstü".into(),
        Some("Documents") => "Belgeler".into(),
        Some("Downloads") => "İndirilenler".into(),
        Some(other) => other.to_string(),
        None => root.to_string_lossy().to_string(),
    }
}

fn survey(spec: &Spec, matched: Vec<Entry>) -> Done {
    let mut widened = spec.clone();
    widened.groups.clear();
    widened.recent = true;
    widened.limit = SURVEY_LIMIT;
    let mut broad = spotlight(&widened);
    if broad.as_ref().is_none_or(Vec::is_empty) {
        broad = Some(walk(&widened));
    }
    let mut seen = HashSet::new();
    let mut listed = Vec::new();
    for item in rank(matched, spec)
        .into_iter()
        .chain(rank(broad.unwrap_or_default(), &widened))
    {
        if seen.insert(item.path.clone()) {
            listed.push(item);
            if listed.len() >= SURVEY_LIMIT {
                break;
            }
        }
    }
    Done::new(
        format!("{} dosya listelendi.", listed.len()),
        Some(data("files", listed)),
    )
}

pub(crate) fn search(value: &Value) -> Result<Done> {
    let request: SearchRequest = payload(value, "fileSearch", "dosya arama")?;
    if request
        .cursor
        .as_deref()
        .is_some_and(|cursor| !cursor.starts_with("o:"))
    {
        return Err(error(
            "INVALID_CURSOR",
            "Aramayı yeniden başlat.",
            "validation",
        ));
    }
    let offset: usize = request
        .cursor
        .as_deref()
        .and_then(|cursor| cursor[2..].parse().ok())
        .unwrap_or(0);
    let spec = build(&request, SystemTime::now())?;

    let mut entries = spotlight(&spec);
    let mut used_index = entries.is_some();
    if entries.is_none() || (entries.as_ref().is_some_and(Vec::is_empty) && !spec.content) {
        // No index, or it knows nothing here (a folder Spotlight does not cover): walk the folders.
        let walked = walk(&spec);
        if entries.is_none() || !walked.is_empty() {
            entries = Some(walked);
            used_index = false;
        }
    }
    let entries = entries.unwrap_or_default();
    if request.survey == Some(true) {
        return Ok(survey(&spec, entries));
    }
    let ranked = rank(entries, &spec);
    let total = ranked.len();
    let page: Vec<Entry> = ranked.into_iter().skip(offset).take(PAGE_SIZE).collect();
    let scope = spec
        .roots
        .iter()
        .map(|root| scope_name(root))
        .collect::<Vec<_>>()
        .join(", ");
    let remaining = total as i64 - offset as i64 - page.len() as i64;
    let mut message = if total == 0 {
        format!("Eşleşen dosya bulunamadı (aranan: {scope}).")
    } else {
        format!(
            "{total} sonuç (aranan: {scope}); {}–{} gösteriliyor.",
            offset + 1,
            offset + page.len()
        )
    };
    if remaining > 0 {
        message.push_str(&format!(" Devam kodu o:{}.", offset + page.len()));
    }
    if spec.content && !used_index {
        message.push_str(" İçerik araması için Spotlight dizini gerekli; yalnızca adlara bakıldı.");
    }
    Ok(Done::new(message, Some(data("files", page))))
}

pub(crate) fn operations() -> Vec<Op> {
    vec![Arc::new(SearchFiles)]
}

struct SearchFiles;

#[async_trait::async_trait]
impl Operation for SearchFiles {
    fn name(&self) -> &'static str {
        "searchFiles"
    }

    fn lane(&self) -> Lane {
        Lane::ReadOnly
    }

    async fn run(&self, _ctx: &Context, payload: &Value) -> Outcome {
        let payload = payload.clone();
        match tokio::task::spawn_blocking(move || search(&payload)).await {
            Ok(result) => outcome(result),
            Err(_) => Outcome::failed(error(
                "OUTCOME_UNKNOWN",
                "Arama beklenmedik şekilde durdu.",
                "tool_failure",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wiqo-search-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("proje/node_modules")).unwrap();
        for (name, body) in [
            ("Fatura Ağustos.pdf", "x"),
            ("fatura_eylul.pdf", "x"),
            ("rapor.docx", "x"),
            ("sifre listesi.txt", "x"),
            ("proje/node_modules/fatura.js", "x"),
            ("proje/fatura notu.txt", "x"),
        ] {
            fs::write(dir.join(name), body).unwrap();
        }
        dunce::canonicalize(dir).unwrap()
    }

    fn run(folder: &Path, extra: Value) -> Done {
        let mut value = json!({ "query": "", "folder": folder.to_string_lossy() });
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        search(&json!({ "kind": "fileSearch", "value": value })).unwrap()
    }

    fn names(done: &Done) -> Vec<String> {
        done.data.as_ref().unwrap()["value"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn names_are_found_without_noise_or_sensitive_files_and_diacritics_do_not_matter() {
        let dir = scratch();
        let found = names(&run(&dir, json!({ "query": "fatura" })));
        assert!(
            found.contains(&"Fatura Ağustos.pdf".to_string())
                && found.contains(&"fatura_eylul.pdf".to_string())
                && found.contains(&"fatura notu.txt".to_string())
        );
        assert!(
            !found.contains(&"fatura.js".to_string()),
            "node_modules is noise"
        );
        assert!(
            !names(&run(&dir, json!({ "query": "sifre" })))
                .contains(&"sifre listesi.txt".to_string()),
            "sensitive names are left out"
        );
        assert!(names(&run(&dir, json!({ "query": "agustos" })))
            .contains(&"Fatura Ağustos.pdf".to_string()));
    }

    #[test]
    fn kind_alternatives_and_paging_work_together() {
        let dir = scratch();
        assert_eq!(names(&run(&dir, json!({ "kind": "pdf" }))).len(), 2);
        let any = names(&run(
            &dir,
            json!({ "query": "kontrat", "alternatives": ["rapor"] }),
        ));
        assert_eq!(any, vec!["rapor.docx".to_string()]);
        let none = run(&dir, json!({ "query": "olmayanşey" }));
        assert!(none.message.starts_with("Eşleşen dosya bulunamadı"));
        let folders = names(&run(&dir, json!({ "kind": "folder" })));
        assert!(folders.contains(&"proje".to_string()));
        let bad =
            search(&json!({ "kind": "fileSearch", "value": { "query": "x", "cursor": "zzz" } }))
                .unwrap_err();
        assert_eq!(bad.code, "INVALID_CURSOR");
    }

    #[test]
    fn a_survey_lists_everything_in_scope_with_the_name_matches_first() {
        let dir = scratch();
        let listed = names(&run(&dir, json!({ "query": "rapor", "survey": true })));
        assert_eq!(listed[0], "rapor.docx");
        assert!(listed.len() >= 4);
    }
}
