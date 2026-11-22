//! What the local agent knows about a repository without reading it whole: its files, the symbols each one defines, which files import which, and
//! for a change, which modules depend on it. Plain line patterns per language (no parser, no index service); each file is read again only when its
//! size or modification time changed, so a second look at the same project costs a directory walk.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use regex::Regex;

const MOST_FILES: usize = 6_000;
const FILE_BYTES: u64 = 400_000;
const OUT_CHARS: usize = 12_000;
const SKIPPED: &[&str] = &[
    ".git", "node_modules", "target", "dist", "build", ".next", ".nuxt", "Pods", "DerivedData", "__pycache__", ".venv", "venv", "coverage", ".gradle",
    ".idea", ".turbo", ".cache", "vendor", "out",
];

#[derive(Clone)]
struct Symbol {
    kind: &'static str,
    name: String,
    line: usize,
}

#[derive(Clone)]
struct Indexed {
    stamp: (u64, SystemTime),
    lines: usize,
    symbols: Vec<Symbol>,
    /// What the file names as its imports, as written ("./util", "crate::a", "pkg.mod").
    imports: Vec<String>,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Indexed>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Indexed>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

struct Lang {
    exts: &'static [&'static str],
    symbols: Regex,
    imports: Option<Regex>,
}

/// Symbol pattern: the last capture group that matched is the name, the first word group is the kind.
fn langs() -> &'static [Lang] {
    static LANGS: OnceLock<Vec<Lang>> = OnceLock::new();
    LANGS.get_or_init(|| {
        let re = |text: &str| Regex::new(text).expect("repo pattern");
        vec![
            Lang { exts: &["ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "svelte"],
                symbols: re(r"^(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:abstract\s+)?(?:async\s+)?(function\*?|class|interface|type|enum|const|let|var)\s+([A-Za-z_$][\w$]*)"),
                imports: Some(re(r#"(?:^|\s)(?:import|export)\s[^'"]*?from\s*['"]([^'"]+)['"]|^\s*import\s*['"]([^'"]+)['"]|require\(\s*['"]([^'"]+)['"]\s*\)|import\(\s*['"]([^'"]+)['"]\s*\)"#)) },
            Lang { exts: &["py"],
                symbols: re(r"^(?:    )?(?:async\s+)?(def|class)\s+([A-Za-z_]\w*)"),
                imports: Some(re(r"^\s*from\s+([\w.]+)\s+import|^\s*import\s+([\w.]+)")) },
            Lang { exts: &["rs"],
                symbols: re(r"^\s*(?:pub(?:\([\w:]+\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(fn|struct|enum|trait|mod|const|static|type|macro_rules!)\s*([A-Za-z_]\w*)"),
                imports: Some(re(r"^\s*(?:pub(?:\([\w:]+\))?\s+)?mod\s+(\w+)\s*;|^\s*(?:pub\s+)?use\s+((?:crate|super|self)::[\w:]+)")) },
            Lang { exts: &["swift"],
                symbols: re(r"^\s*(?:(?:public|private|internal|fileprivate|open|final|static|override|nonisolated|@\w+(?:\([^)]*\))?)\s+)*(func|class|struct|enum|protocol|extension|actor|typealias)\s+([A-Za-z_]\w*)"),
                imports: None },
            Lang { exts: &["go"],
                symbols: re(r"^(func|type)\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)"),
                imports: Some(re(r#"^\s*import\s+"([^"]+)"|^\s+(?:\w+\s+)?"([^"]+)"\s*$"#)) },
            Lang { exts: &["java", "kt", "kts", "cs", "scala", "dart"],
                symbols: re(r"^\s*(?:(?:public|private|protected|internal|static|final|abstract|sealed|data|open|override|suspend|async|partial)\s+)*(class|interface|enum|record|object|fun|void|struct)\s+([A-Za-z_]\w*)"),
                imports: Some(re(r"^\s*import\s+(?:static\s+)?([\w.]+)|^\s*using\s+([\w.]+)\s*;")) },
            Lang { exts: &["rb"], symbols: re(r"^\s*(def|class|module)\s+([\w.:?!]+)"), imports: Some(re(r#"^\s*require(?:_relative)?\s+['"]([^'"]+)['"]"#)) },
            Lang { exts: &["php"], symbols: re(r"^\s*(?:(?:abstract|final|public|private|protected|static)\s+)*(function|class|interface|trait)\s+([A-Za-z_]\w*)"),
                imports: Some(re(r#"^\s*(?:require|include)(?:_once)?\s*\(?\s*['"]([^'"]+)['"]"#)) },
            Lang { exts: &["c", "h", "cc", "cpp", "hpp"], symbols: re(r"^(struct|class|enum|typedef|#define)\s+([A-Za-z_]\w*)"), imports: Some(re(r#"^\s*#include\s+"([^"]+)""#)) },
            Lang { exts: &["css", "scss", "less"], symbols: re(r"^(@mixin|@function|--)([\w-]+)"), imports: Some(re(r#"@import\s+(?:url\()?['"]([^'"]+)['"]"#)) },
            Lang { exts: &["html", "htm"], symbols: re(r#"(id)="([\w-]+)""#), imports: Some(re(r##"(?:src|href)="([^"#:?]+\.(?:js|mjs|css|html))""##)) },
        ]
    })
}

fn ext_of(path: &Path) -> String {
    path.extension().map(|ext| ext.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn lang_of(path: &Path) -> Option<&'static Lang> {
    let ext = ext_of(path);
    langs().iter().find(|lang| lang.exts.contains(&ext.as_str()))
}

fn index_text(path: &Path, text: &str) -> (Vec<Symbol>, Vec<String>) {
    let Some(lang) = lang_of(path) else { return (Vec::new(), Vec::new()) };
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let go_block = ext_of(path) == "go";
    let mut in_go_imports = false;
    for (number, line) in text.lines().enumerate() {
        if line.len() > 400 {
            continue;
        }
        if symbols.len() < 400 {
            if let Some(found) = lang.symbols.captures(line) {
                let kind = found.get(1).map(|kind| kind.as_str()).unwrap_or("");
                if let Some(name) = found.iter().skip(2).flatten().last() {
                    let kind: &'static str = match kind {
                        "function" | "function*" | "def" | "fn" | "func" | "fun" | "void" => "fn",
                        "class" | "struct" | "record" | "object" | "actor" => "class",
                        "interface" | "protocol" | "trait" => "interface",
                        "type" | "typealias" | "typedef" => "type",
                        "enum" => "enum",
                        "const" | "let" | "var" | "static" | "#define" | "--" => "value",
                        "mod" | "module" => "module",
                        "extension" => "extension",
                        "id" => "id",
                        _ => "symbol",
                    };
                    symbols.push(Symbol { kind, name: name.as_str().to_string(), line: number + 1 });
                }
            }
        }
        let Some(pattern) = &lang.imports else { continue };
        if go_block {
            let trimmed = line.trim();
            if trimmed.starts_with("import (") {
                in_go_imports = true;
                continue;
            }
            if in_go_imports && trimmed == ")" {
                in_go_imports = false;
            }
            if !in_go_imports && !trimmed.starts_with("import ") {
                continue;
            }
        }
        for found in pattern.captures_iter(line) {
            if let Some(name) = found.iter().skip(1).flatten().next() {
                if imports.len() < 200 {
                    imports.push(name.as_str().to_string());
                }
            }
        }
    }
    (symbols, imports)
}

/// The project's files, relative to its root: git's own list when it is a repository (ignored files stay out), a bounded walk otherwise.
fn list_files(root: &Path) -> Vec<String> {
    if root.join(".git").exists() {
        let out = std::process::Command::new("git")
            .args(["-C", &root.to_string_lossy(), "ls-files", "-co", "--exclude-standard", "-z"])
            .stdin(std::process::Stdio::null())
            .output();
        if let Some(out) = out.ok().filter(|out| out.status.success()) {
            return String::from_utf8_lossy(&out.stdout)
                .split('\0')
                .filter(|path| !path.is_empty() && !path.split('/').any(|part| SKIPPED.contains(&part)))
                .take(MOST_FILES)
                .map(str::to_string)
                .collect();
        }
    }
    let mut found = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if !SKIPPED.contains(&name.as_str()) && !name.starts_with('.') {
                    folders.push(path);
                }
            } else if kind.is_file() {
                if let Ok(relative) = path.strip_prefix(root) {
                    found.push(relative.to_string_lossy().replace('\\', "/"));
                }
                if found.len() >= MOST_FILES {
                    return found;
                }
            }
        }
    }
    found.sort();
    found
}

/// Every listed file's index, from the cache when the file is unchanged.
fn indexed(root: &Path, files: &[String]) -> HashMap<String, Indexed> {
    let mut known = cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut out = HashMap::new();
    for relative in files {
        let path = root.join(relative);
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        let stamp = (meta.len(), meta.modified().unwrap_or(SystemTime::UNIX_EPOCH));
        if let Some(entry) = known.get(&path).filter(|entry| entry.stamp == stamp) {
            out.insert(relative.clone(), entry.clone());
            continue;
        }
        let entry = if meta.len() > FILE_BYTES || lang_of(&path).is_none() {
            let lines = if meta.len() <= FILE_BYTES { std::fs::read_to_string(&path).map(|text| text.lines().count()).unwrap_or(0) } else { 0 };
            Indexed { stamp, lines, symbols: Vec::new(), imports: Vec::new() }
        } else {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let (symbols, imports) = index_text(&path, &text);
                    Indexed { stamp, lines: text.lines().count(), symbols, imports }
                }
                Err(_) => Indexed { stamp, lines: 0, symbols: Vec::new(), imports: Vec::new() },
            }
        };
        known.insert(path, entry.clone());
        out.insert(relative.clone(), entry);
    }
    if known.len() > 60_000 {
        known.clear();
    }
    out
}

fn normalize(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::Normal(name) => parts.push(name.to_string_lossy().to_string()),
            _ => {}
        }
    }
    parts.join("/")
}

/// The project file an import names, when it names one of the project's own (packages and the standard library are not followed).
fn resolve(from: &str, import: &str, files: &HashSet<&str>) -> Option<String> {
    let dir = Path::new(from).parent().unwrap_or(Path::new(""));
    let ext = ext_of(Path::new(from));
    let try_all = |base: String, candidates: &[&str]| -> Option<String> {
        candidates.iter().map(|suffix| format!("{base}{suffix}")).find(|candidate| files.contains(candidate.as_str()))
    };
    match ext.as_str() {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue" | "svelte" | "css" | "scss" | "less" | "html" | "htm" | "c" | "h" | "cc" | "cpp" | "hpp" | "rb" | "php" => {
            let relative = import.starts_with('.') || matches!(ext.as_str(), "css" | "scss" | "less" | "html" | "htm" | "c" | "h" | "cc" | "cpp" | "hpp" | "php" | "rb");
            let base = if relative { normalize(&dir.join(import)) } else if let Some(rest) = import.strip_prefix("@/").or_else(|| import.strip_prefix("~/")) { format!("src/{rest}") } else { return None };
            let stem = base.strip_suffix(".js").or_else(|| base.strip_suffix(".mjs")).map(str::to_string);
            try_all(base.clone(), &["", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".vue", ".svelte", ".rb", ".scss", "/index.ts", "/index.tsx", "/index.js", "/index.jsx"])
                .or_else(|| stem.and_then(|stem| try_all(stem, &[".ts", ".tsx"])))
        }
        "py" => {
            let dots = import.chars().take_while(|c| *c == '.').count();
            let rest = import[dots..].replace('.', "/");
            let mut bases = Vec::new();
            if dots > 0 {
                let mut up = dir.to_path_buf();
                for _ in 1..dots {
                    up.pop();
                }
                bases.push(normalize(&up.join(&rest)));
            } else {
                bases.extend([rest.clone(), format!("src/{rest}"), normalize(&dir.join(&rest))]);
            }
            bases.into_iter().find_map(|base| try_all(base, &[".py", "/__init__.py"]))
        }
        "rs" => {
            if let Some(path) = import.strip_prefix("crate::") {
                let first = path.split("::").next().unwrap_or("");
                let src = from.split("/src/").next().filter(|_| from.contains("/src/")).map(|crate_dir| format!("{crate_dir}/src")).unwrap_or_else(|| "src".into());
                return try_all(format!("{src}/{first}"), &[".rs", "/mod.rs"]);
            }
            if import.contains("::") {
                return None;
            }
            // `mod name;` sits next to the file (main.rs, lib.rs, mod.rs) or in a folder named after it.
            let name = Path::new(from).file_stem().map(|stem| stem.to_string_lossy().to_string()).unwrap_or_default();
            let own = if matches!(name.as_str(), "main" | "lib" | "mod") { dir.to_path_buf() } else { dir.join(&name) };
            try_all(normalize(&own.join(import)), &[".rs", "/mod.rs"])
        }
        "go" => {
            let tail = import.rsplit('/').next().unwrap_or(import);
            files.iter().find(|file| file.ends_with(".go") && Path::new(file).parent().and_then(|parent| parent.file_name()).is_some_and(|parent| parent == tail)).map(|file| file.to_string())
        }
        _ => None,
    }
}

struct Graph {
    imports: HashMap<String, Vec<String>>,
    importers: HashMap<String, Vec<String>>,
}

fn graph(index: &HashMap<String, Indexed>) -> Graph {
    let names: HashSet<&str> = index.keys().map(String::as_str).collect();
    let mut imports: HashMap<String, Vec<String>> = HashMap::new();
    let mut importers: HashMap<String, Vec<String>> = HashMap::new();
    for (file, entry) in index {
        for import in &entry.imports {
            if let Some(target) = resolve(file, import, &names).filter(|target| target != file) {
                let list = imports.entry(file.clone()).or_default();
                if !list.contains(&target) {
                    list.push(target.clone());
                    importers.entry(target).or_default().push(file.clone());
                }
            }
        }
    }
    Graph { imports, importers }
}

/// Files that depend on `file`, up to three steps away: what a change to it can break.
fn dependents(graph: &Graph, file: &str) -> Vec<(String, usize)> {
    let mut seen: HashSet<String> = HashSet::from([file.to_string()]);
    let mut out = Vec::new();
    let mut layer = vec![file.to_string()];
    for depth in 1..=3 {
        let mut next = Vec::new();
        for current in &layer {
            for importer in graph.importers.get(current).into_iter().flatten() {
                if seen.insert(importer.clone()) {
                    out.push((importer.clone(), depth));
                    next.push(importer.clone());
                }
            }
        }
        layer = next;
    }
    out
}

fn clip(text: String) -> String {
    if text.chars().count() <= OUT_CHARS {
        return text;
    }
    let mut cut: String = text.chars().take(OUT_CHARS).collect();
    cut.push_str("\n… (kısaltıldı; query ya da file ile daralt)");
    cut
}

fn overview(root: &Path, files: &[String], index: &HashMap<String, Indexed>, graph: &Graph) -> String {
    let mut by_ext: HashMap<String, (usize, usize)> = HashMap::new();
    let mut folders: HashMap<String, usize> = HashMap::new();
    for file in files {
        let ext = ext_of(Path::new(file));
        let lines = index.get(file).map(|entry| entry.lines).unwrap_or(0);
        let slot = by_ext.entry(if ext.is_empty() { "(uzantısız)".into() } else { ext }).or_default();
        slot.0 += 1;
        slot.1 += lines;
        let folder: Vec<&str> = file.split('/').collect();
        let key = if folder.len() > 2 { format!("{}/{}/", folder[0], folder[1]) } else if folder.len() == 2 { format!("{}/", folder[0]) } else { "./".into() };
        *folders.entry(key).or_default() += 1;
    }
    let mut langs: Vec<_> = by_ext.into_iter().collect();
    langs.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    let mut folders: Vec<_> = folders.into_iter().collect();
    folders.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut central: Vec<(&String, usize)> = graph.importers.iter().map(|(file, list)| (file, list.len())).collect();
    central.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let mut big: Vec<(&String, usize)> = index.iter().map(|(file, entry)| (file, entry.lines)).collect();
    big.sort_by(|a, b| b.1.cmp(&a.1));
    let entries: Vec<&String> = files.iter().filter(|file| {
        let name = file.rsplit('/').next().unwrap_or(file);
        matches!(name, "main.rs" | "lib.rs" | "main.py" | "app.py" | "manage.py" | "index.ts" | "index.js" | "main.ts" | "main.js" | "App.tsx" | "App.jsx" | "main.go" | "index.html" | "server.ts" | "server.js")
            && file.matches('/').count() <= 3
    }).take(12).collect();
    let symbols: usize = index.values().map(|entry| entry.symbols.len()).sum();
    let mut out = vec![format!("DEPO HARİTASI: {} — {} dosya, {} sembol, {} yerel import bağı", root.display(), files.len(), symbols, graph.imports.values().map(Vec::len).sum::<usize>())];
    out.push(format!("Diller (dosya/satır): {}", langs.iter().take(10).map(|(ext, (count, lines))| format!("{ext} {count}/{lines}")).collect::<Vec<_>>().join(", ")));
    out.push(format!("Klasörler: {}", folders.iter().take(25).map(|(folder, count)| format!("{folder} ({count})")).collect::<Vec<_>>().join(", ")));
    if !entries.is_empty() {
        out.push(format!("Giriş noktaları: {}", entries.iter().map(|file| file.as_str()).collect::<Vec<_>>().join(", ")));
    }
    if !central.is_empty() {
        out.push(format!("En çok içe aktarılanlar: {}", central.iter().take(12).map(|(file, count)| format!("{file} ({count})")).collect::<Vec<_>>().join(", ")));
    }
    out.push(format!("En büyük dosyalar (satır): {}", big.iter().take(10).map(|(file, lines)| format!("{file} {lines}")).collect::<Vec<_>>().join(", ")));
    out.push("Daraltmak için: query (sembol/dosya adı ara) ya da file (o dosyanın sembolleri, importları, ona bağlı dosyalar).".into());
    clip(out.join("\n"))
}

fn search(files: &[String], index: &HashMap<String, Indexed>, graph: &Graph, query: &str) -> String {
    let terms: Vec<String> = query.split(|c: char| c.is_whitespace() || c == ',').map(|term| term.trim().to_lowercase()).filter(|term| term.chars().count() >= 2).collect();
    if terms.is_empty() {
        return "query en az iki harflik bir kelime olmalı.".into();
    }
    let mut scored: Vec<(usize, &String, Vec<&Symbol>)> = Vec::new();
    for file in files {
        let lower_path = file.to_lowercase();
        let entry = index.get(file);
        let mut score = 0;
        let mut hits = Vec::new();
        for term in &terms {
            if lower_path.contains(term.as_str()) {
                score += 3;
            }
            for symbol in entry.map(|entry| entry.symbols.as_slice()).unwrap_or_default() {
                let name = symbol.name.to_lowercase();
                if name == *term {
                    score += 10;
                    hits.push(symbol);
                } else if name.contains(term.as_str()) {
                    score += 4;
                    hits.push(symbol);
                }
            }
        }
        if score > 0 {
            score += graph.importers.get(file).map(Vec::len).unwrap_or(0).min(5);
            scored.push((score, file, hits));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    if scored.is_empty() {
        return format!("\"{query}\" için dosya adı ya da sembol bulunamadı. İçerikte aramak için search aracını kullan.");
    }
    let lines: Vec<String> = scored.iter().take(15).map(|(_, file, hits)| {
        let shown: Vec<String> = hits.iter().take(6).map(|symbol| format!("{} {}:{}", symbol.kind, symbol.name, symbol.line)).collect();
        let users = graph.importers.get(*file).map(Vec::len).unwrap_or(0);
        format!("{file}{}{}", if shown.is_empty() { String::new() } else { format!(" — {}", shown.join(", ")) }, if users > 0 { format!(" [{users} dosya içe aktarıyor]") } else { String::new() })
    }).collect();
    clip(format!("\"{query}\" için ilgili dosyalar:\n{}", lines.join("\n")))
}

fn describe(file: &str, index: &HashMap<String, Indexed>, graph: &Graph) -> String {
    let Some(entry) = index.get(file) else {
        return format!("{file} projede yok (ya da git'in yok saydığı bir dosya). Yolu proje köküne göre ver.");
    };
    let mut out = vec![format!("{file}: {} satır", entry.lines)];
    if !entry.symbols.is_empty() {
        out.push(format!("Semboller: {}", entry.symbols.iter().take(80).map(|symbol| format!("{} {}:{}", symbol.kind, symbol.name, symbol.line)).collect::<Vec<_>>().join(", ")));
    }
    if let Some(list) = graph.imports.get(file) {
        out.push(format!("İçe aktardıkları (proje içi): {}", list.join(", ")));
    }
    let outside: Vec<&String> = entry.imports.iter().filter(|import| !import.starts_with('.') && !import.starts_with("crate::") && !import.starts_with("super::")).take(20).collect();
    if !outside.is_empty() {
        out.push(format!("Dış paketler/modüller: {}", outside.iter().map(|import| import.as_str()).collect::<Vec<_>>().join(", ")));
    }
    let affected = dependents(graph, file);
    if affected.is_empty() {
        out.push("Bu dosyayı içe aktaran proje dosyası yok.".into());
    } else {
        out.push(format!("Etkilenebilecek dosyalar (bağımlılık derinliği): {}", affected.iter().take(40).map(|(file, depth)| format!("{file} ({depth})")).collect::<Vec<_>>().join(", ")));
    }
    clip(out.join("\n"))
}

/// The `repo_map` tool: an overview of the project, the files a query is about, or one file's place in it.
pub(super) fn repo_map(root: &Path, query: Option<&str>, file: Option<&str>) -> String {
    let files = list_files(root);
    if files.is_empty() {
        return format!("{} içinde dosya bulunamadı.", root.display());
    }
    let index = indexed(root, &files);
    let graph = graph(&index);
    match (file.map(str::trim).filter(|file| !file.is_empty()), query.map(str::trim).filter(|query| !query.is_empty())) {
        (Some(file), _) => {
            let relative = Path::new(file).strip_prefix(root).map(|path| path.to_string_lossy().replace('\\', "/")).unwrap_or_else(|_| file.trim_start_matches("./").to_string());
            describe(&relative, &index, &graph)
        }
        (None, Some(query)) => search(&files, &index, &graph, query),
        (None, None) => overview(root, &files, &index, &graph),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> PathBuf {
        let root = std::env::temp_dir().join(format!("wiqo-repo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src/util")).unwrap();
        std::fs::write(root.join("package.json"), "{}").unwrap();
        std::fs::write(root.join("src/index.ts"), "import { add } from \"./util/math.js\";\nexport function main() { return add(1, 2); }\n").unwrap();
        std::fs::write(root.join("src/util/math.ts"), "export function add(a: number, b: number) { return a + b; }\nexport const PI = 3.14;\n").unwrap();
        std::fs::write(root.join("src/app.ts"), "import { main } from './index';\nclass App {}\n").unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::write(root.join("node_modules/x/index.js"), "export function add() {}").unwrap();
        root
    }

    #[test]
    fn finds_symbols_and_the_modules_a_change_reaches() {
        let root = project();
        let map = repo_map(&root, None, None);
        assert!(map.contains("4 dosya"), "{map}");
        let found = repo_map(&root, Some("add"), None);
        assert!(found.lines().nth(1).unwrap().starts_with("src/util/math.ts — fn add:1"), "{found}");
        assert!(!found.contains("node_modules"), "{found}");
        let one = repo_map(&root, None, Some("src/util/math.ts"));
        assert!(one.contains("src/index.ts (1)") && one.contains("src/app.ts (2)"), "{one}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn python_and_rust_imports_resolve_to_files() {
        let names: HashSet<&str> = ["pkg/__init__.py", "pkg/db.py", "app.py", "src/main.rs", "src/net/mod.rs", "src/store.rs"].into_iter().collect();
        assert_eq!(resolve("app.py", "pkg.db", &names).as_deref(), Some("pkg/db.py"));
        assert_eq!(resolve("pkg/db.py", ".", &names).as_deref(), Some("pkg/__init__.py"));
        assert_eq!(resolve("src/main.rs", "net", &names).as_deref(), Some("src/net/mod.rs"));
        assert_eq!(resolve("src/net/mod.rs", "crate::store::Thing", &names).as_deref(), Some("src/store.rs"));
        assert_eq!(resolve("app.py", "os", &names), None);
    }
}

// ---- repo_find: which part of the project is about this --------------------------------------------------------------------------------------------------

/// What a question about a project is answered from: the files cut into short windows of lines, and a ranking of the windows by BM25 over the words in them
/// (identifiers split at `_` and at camelCase, so `parseDims` is found by "parse dims" and by "dims"). No model, no network: built from the files on this
/// computer, kept in memory while the files do not change. "Where is the coupon handled" has no symbol to look up; this finds the lines.
const WINDOW: usize = 30;
const STEP: usize = 15;
const MOST_FOUND: usize = 8;
const FIND_FILE_BYTES: u64 = 200_000;
const MOST_WINDOWS: usize = 60_000;

struct Window {
    file: usize,
    from: usize,
    terms: HashMap<String, u16>,
    length: usize,
}

struct FindIndex {
    stamp: (usize, u64, u64),
    files: Vec<String>,
    windows: Vec<Window>,
    frequency: HashMap<String, usize>,
    average: f64,
}

fn find_cache() -> &'static Mutex<HashMap<PathBuf, std::sync::Arc<FindIndex>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, std::sync::Arc<FindIndex>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

const STOP: &[&str] = &["the", "and", "for", "with", "this", "that", "from", "into", "are", "was", "bir", "bu", "ve", "ile", "için", "gibi", "var", "yok", "mi", "mı", "de", "da"];

/// The words of a text: lower-cased, with identifiers also split into their parts.
fn words(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()).filter(|word| word.chars().count() >= 2) {
        let lower = word.to_lowercase();
        let mut parts: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut previous_lower = false;
        for c in word.chars() {
            if c.is_uppercase() && previous_lower && !current.is_empty() {
                parts.push(std::mem::take(&mut current).to_lowercase());
            }
            previous_lower = c.is_lowercase() || c.is_ascii_digit();
            current.push(c);
        }
        parts.push(current.to_lowercase());
        if !STOP.contains(&lower.as_str()) {
            found.push(lower.clone());
        }
        if parts.len() > 1 {
            found.extend(parts.into_iter().filter(|part| part.chars().count() >= 2 && !STOP.contains(&part.as_str())));
        }
    }
    found
}

fn build_find_index(root: &Path, files: &[String]) -> FindIndex {
    let mut kept: Vec<String> = Vec::new();
    let mut windows: Vec<Window> = Vec::new();
    let mut frequency: HashMap<String, usize> = HashMap::new();
    let mut total_length = 0usize;
    let (mut count, mut newest, mut size) = (0usize, 0u64, 0u64);
    for relative in files {
        let path = root.join(relative);
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        count += 1;
        size += meta.len();
        newest = newest.max(meta.modified().ok().and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0));
        if meta.len() > FIND_FILE_BYTES || meta.len() == 0 || windows.len() >= MOST_WINDOWS {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        if bytes.contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let file = kept.len();
        kept.push(relative.clone());
        let mut from = 0;
        while from < lines.len() {
            let to = (from + WINDOW).min(lines.len());
            let mut terms: HashMap<String, u16> = HashMap::new();
            let mut length = 0;
            // The file's own name is part of what each of its windows is about.
            for word in words(relative).into_iter().chain(words(&lines[from..to].join("\n"))) {
                *terms.entry(word).or_insert(0) += 1;
                length += 1;
            }
            for term in terms.keys() {
                *frequency.entry(term.clone()).or_insert(0) += 1;
            }
            total_length += length;
            windows.push(Window { file, from, terms, length });
            if to == lines.len() {
                break;
            }
            from += STEP;
        }
    }
    let average = if windows.is_empty() { 1.0 } else { total_length as f64 / windows.len() as f64 };
    FindIndex { stamp: (count, newest, size), files: kept, windows, frequency, average }
}

fn find_index(root: &Path) -> std::sync::Arc<FindIndex> {
    let files = list_files(root);
    // Whether the files are the same as when the index was built: how many, the newest change, their total size.
    let (mut count, mut newest, mut size) = (0usize, 0u64, 0u64);
    for relative in &files {
        if let Ok(meta) = std::fs::metadata(root.join(relative)) {
            count += 1;
            size += meta.len();
            newest = newest.max(meta.modified().ok().and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0));
        }
    }
    let mut known = find_cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(index) = known.get(root).filter(|index| index.stamp == (count, newest, size)) {
        return index.clone();
    }
    let built = std::sync::Arc::new(build_find_index(root, &files));
    if known.len() > 8 {
        known.clear();
    }
    known.insert(root.to_path_buf(), built.clone());
    built
}

/// The windows most about `query`, best first: `path:from–to` and the line of the window that matches most.
pub fn repo_find(root: &Path, query: &str, hot: &HashSet<String>) -> String {
    let wanted: Vec<String> = {
        let mut seen: HashSet<String> = HashSet::new();
        words(query).into_iter().filter(|word| seen.insert(word.clone())).collect()
    };
    if wanted.is_empty() {
        return "query'de aranacak sözcük yok.".into();
    }
    let index = find_index(root);
    if index.windows.is_empty() {
        return "Bu klasörde aranacak metin dosyası bulunamadı.".into();
    }
    let total = index.windows.len() as f64;
    let (k1, b) = (1.2_f64, 0.75_f64);
    let mut scored: Vec<(f64, usize)> = Vec::new();
    for (position, window) in index.windows.iter().enumerate() {
        let mut score = 0.0;
        for word in &wanted {
            let Some(&tf) = window.terms.get(word) else { continue };
            let df = index.frequency.get(word).copied().unwrap_or(1) as f64;
            let idf = ((total - df + 0.5) / (df + 0.5) + 1.0).ln();
            let tf = f64::from(tf);
            score += idf * (tf * (k1 + 1.0)) / (tf + k1 * (1.0 - b + b * window.length as f64 / index.average));
        }
        if score > 0.0 {
            // Where earlier work in this project happened is likelier to be where this one is.
            if hot.contains(&index.files[window.file]) {
                score *= 1.25;
            }
            scored.push((score, position));
        }
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    // One result per file (its best window), so ten windows of one big file do not hide the other places.
    let mut seen_files: HashSet<usize> = HashSet::new();
    let mut lines: Vec<String> = Vec::new();
    for (score, position) in scored {
        let window = &index.windows[position];
        if !seen_files.insert(window.file) {
            continue;
        }
        let path = &index.files[window.file];
        let text = std::fs::read_to_string(root.join(path)).unwrap_or_default();
        let best = text.lines().enumerate().skip(window.from).take(WINDOW).max_by_key(|(_, line)| words(line).iter().filter(|word| wanted.contains(word)).count());
        let (row, shown) = best.map(|(row, line)| (row + 1, line.trim().chars().take(140).collect::<String>())).unwrap_or((window.from + 1, String::new()));
        lines.push(format!("{path}:{}-{}  ({score:.1})  {row}: {shown}", window.from + 1, window.from + WINDOW));
        if lines.len() >= MOST_FOUND {
            break;
        }
    }
    if lines.is_empty() {
        return format!("\"{query}\" için eşleşen bir yer bulunamadı; başka sözcüklerle (eş anlamlı, kodda geçen ad) dene.");
    }
    format!("En çok ilgili yerler (sıralı; dosyayı satır aralığıyla oku):\n{}", lines.join("\n"))
}

#[cfg(test)]
mod find_tests {
    use super::*;

    #[test]
    fn a_question_is_answered_with_the_place_that_is_about_it_even_when_no_symbol_has_its_name() {
        let root = std::env::temp_dir().join(format!("wiqo-find-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).expect("folder");
        std::fs::write(root.join("src/fiyat.py"), "def indirimOrani(adet):\n    if adet >= 100:\n        return 0.30\n\ndef kuponDus(ara, kupon):\n    # kupon bir tutardir\n    return ara - kupon\n").expect("file");
        std::fs::write(root.join("src/liste.py"), "def sirala(items):\n    return sorted(items)\n").expect("file");
        std::fs::write(root.join("README.md"), "Bu proje fiyat hesaplar.\n").expect("file");
        let found = repo_find(&root, "kupon indirim tutarı nerede düşülüyor", &HashSet::new());
        assert!(found.lines().nth(1).is_some_and(|line| line.starts_with("src/fiyat.py")), "{found}");
        assert!(repo_find(&root, "indirim orani", &HashSet::new()).contains("src/fiyat.py"), "camelCase names are found by their words");
        assert!(repo_find(&root, "zzzzqqq", &HashSet::new()).contains("bulunamadı"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
