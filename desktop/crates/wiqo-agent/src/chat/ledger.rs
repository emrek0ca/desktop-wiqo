//! What a run changed and what it actually checked, kept from the tool results themselves (not from what the model says), so the answer ends
//! with three plain lists: done (files written), verified (a test, build, lint or API call that passed after the last change) and not
//! verified (no check after the last change, or one that failed). The model cannot mark its own work as working.

use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;

#[derive(Deserialize, Serialize)]
struct Check {
    label: String,
    passed: bool,
    /// How many file changes the run had made when this check ran: a check counts only for the changes before it.
    after: usize,
    /// The files this check speaks for; None is the whole project (a test run, a build, a type check). Running one script speaks for that script; fetching a
    /// page from a dev server does not speak for a browser script that nothing parsed.
    covers: Option<Vec<String>>,
}

#[derive(Default, Deserialize, Serialize)]
pub(super) struct Ledger {
    changed: Vec<String>,
    /// The edit counter at each file's last change: a check covers a file only when it ran after that.
    last_edit: HashMap<String, usize>,
    edits: usize,
    checks: Vec<Check>,
    /// Background jobs a check command became (job id → command), so their finish counts too.
    jobs: HashMap<String, String>,
    /// Calls to a local server after the last change, and whether any of them got a 4xx back (a bad request tried, not only the happy path).
    probes: usize,
    bad_probe: bool,
    /// How often the run was turned back for missing checks (at most twice), and whether for missing bad-request probes (once).
    nudges: usize,
    edge_nudged: bool,
    /// Folders the request names to be created ("wiqo-test adında bir klasör"): files written elsewhere go back once, so a file is not left beside
    /// the folder and called "inside" it.
    expected_folders: Vec<String>,
    placement_nudged: bool,
    #[serde(default)]
    last_inspection_ok: bool,
    #[serde(default)]
    attempted_effect: bool,
}

fn check_command() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\b(test|tests|jest|vitest|mocha|pytest|unittest|tsc|typecheck|type-check|lint|eslint|biome|ruff|mypy|clippy|build|compile|check|vet|migrate|migration|prisma\s+(migrate|db)|knex|alembic|go\s+(test|build|vet)|cargo\s+(test|build|check|clippy)|swift\s+(test|build)|dotnet\s+(test|build)|gradle|mvn|make)\b|curl\s+[^|;&]*(localhost|127\.0\.0\.1)")
            .expect("check pattern")
    })
}

/// A command that only parses or formats (node --check, bash -n, py_compile, swiftc -parse, php -l, prettier --check): it says a file is well formed, not that it works.
fn syntax_only(command: &str) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| Regex::new(r"(?i)--check\b|\bnode\s+-c\b|py_compile|\bbash\s+-n\b|\bsh\s+-n\b|swiftc\s+-parse|\bphp\s+-l\b").expect("syntax-only pattern"))
        .is_match(command)
}

/// A script that runs in a browser from a plain page (no package.json near it): fetching its page proves nothing about it, because nothing parsed it.
fn static_script(path: &str) -> bool {
    if !(path.ends_with(".js") || path.ends_with(".mjs")) {
        return false;
    }
    let mut folder = std::path::Path::new(path).parent();
    for _ in 0..4 {
        let Some(dir) = folder else { break };
        if dir.join("package.json").exists() {
            return false;
        }
        folder = dir.parent();
    }
    true
}

/// A test runner that ran nothing ("Ran 0 tests", "collected 0 items", "no tests found"…) exits 0 but proves nothing.
fn ran_no_tests(output: &str) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| {
            Regex::new(r"(?i)\bRan 0 tests?\b|collected 0 items|no tests ran|no tests? (were )?found|no test files found|\b0 passing\b|Tests:\s+0 total|# tests 0\b|\brunning 0 tests\b|test result: ok\. 0 passed")
                .expect("no-tests pattern")
        })
        .is_match(output)
}

/// A local server answered with a 4xx: `curl -i` shows "HTTP/1.1 400", `-w '%{http_code}'` prints the bare code.
fn got_client_error(output: &str) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| {
            Regex::new(r"HTTP/[\d.]+ 4\d\d\b|(^|[^\d.])(400|401|403|404|409|413|415|422)([^\d]|$)")
                .expect("4xx pattern")
        })
        .is_match(output)
}

/// Running a script the run itself wrote (`python3 report.py`, `node cli.js data.csv`) checks it too.
fn runs_changed_file(command: &str, changed: &[String]) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let interpreter = PATTERN.get_or_init(|| Regex::new(r"(?i)(^|[\s;&|(])(python3?|node|deno|bun|tsx|ts-node|ruby|bash|sh|php|go\s+run|uv\s+run)\s").expect("interpreter pattern"));
    // A script the run wrote into a scratch folder (/tmp, the system's temp folders) is its own test harness, usually a copy of part of the work: running it says
    // nothing about the files the person asked for (a copy of the functions passed while the real file did not even parse).
    let scratch = |path: &str| {
        path.starts_with("/tmp/")
            || path.starts_with("/private/tmp/")
            || path.starts_with("/var/folders/")
            || path.contains("/T/")
    };
    interpreter.is_match(command)
        && changed
            .iter()
            .filter(|path| !scratch(path))
            .filter_map(|path| path.rsplit('/').next())
            .filter(|name| name.contains('.'))
            .any(|name| command.contains(name))
}

/// A call to an API, not a page: a method other than GET, a body or form, or a path that is not the page, its scripts or its assets. A dev server
/// answering for its page (`curl localhost:5173`, `/src/main.jsx`) is checked by the answer being 200 and right, not by bad requests: asking a
/// static app for a 422 sent a counter app into an API analysis.
fn api_like_curl(command: &str) -> bool {
    static PATTERN: OnceLock<(Regex, Regex)> = OnceLock::new();
    let (flags, url) = PATTERN.get_or_init(|| {
        (
            Regex::new(r"(^|\s)(-X\s*\S+|--request\b|-d\b|--data\S*|--json\b|-F\b|--form\b|-T\b|--upload-file\b)").expect("flags"),
            Regex::new(r#"(?:https?://)?(?:localhost|127\.0\.0\.1|0\.0\.0\.0)(?::\d+)?(/[^\s'"?#]*)?"#).expect("url"),
        )
    });
    if flags.is_match(command) {
        return true;
    }
    let Some(path) = url
        .captures(command)
        .and_then(|found| found.get(1))
        .map(|path| path.as_str())
    else {
        return false;
    };
    let page = path == "/"
        || path.starts_with("/@")
        || path.starts_with("/src/")
        || path.starts_with("/assets/")
        || path.starts_with("/node_modules/")
        || path.starts_with("/static/")
        || path.rsplit('/').next().is_some_and(|name| {
            [
                ".html", ".js", ".mjs", ".css", ".svg", ".png", ".jpg", ".ico", ".map", ".txt",
                ".woff2",
            ]
            .iter()
            .any(|ext| name.ends_with(ext))
        });
    !page
}

/// Folders the request asks to create by name: "wiqo-test adında bir klasör", "a folder named notes". Only the explicit forms, so a folder that is
/// merely mentioned ("indirilenler klasöründeki dosyalar") never turns a run back.
fn folders_named(goal: &str) -> Vec<String> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        Regex::new(r"(?i)([\p{L}\p{N}._-]+)\s+(?:adında|adlı|isimli|isminde)\s+(?:bir\s+)?klas[öo]r|folder\s+(?:named|called)\s+([\p{L}\p{N}._-]+)|(?:named|called)\s+([\p{L}\p{N}._-]+)\s+folder").expect("folders")
    });
    let mut found: Vec<String> = Vec::new();
    for caps in pattern.captures_iter(goal) {
        if let Some(name) = (1..=3).find_map(|index| caps.get(index)).map(|name| {
            name.as_str()
                .trim_matches(|c| c == '.' || c == '-')
                .to_string()
        }) {
            if name.chars().count() >= 2 && !found.contains(&name) {
                found.push(name);
            }
        }
    }
    found
}

/// Whether a path the run wrote is there now (`~` is the home folder); a relative path cannot be judged here and counts as there.
fn is_there(path: &str) -> bool {
    let full = match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map(|home| home.join(rest)),
        None => Some(std::path::PathBuf::from(path)),
    };
    match full {
        Some(full) if full.is_absolute() => full.exists(),
        _ => true,
    }
}

fn exit_code(output: &str) -> Option<i64> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern =
        PATTERN.get_or_init(|| Regex::new(r#"exitCode\\?"\s*:\s*(-?\d+)"#).expect("exit pattern"));
    pattern
        .captures(output)
        .and_then(|found| found[1].parse().ok())
}

fn local_server() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"localhost|127\.0\.0\.1|0\.0\.0\.0").expect("local pattern"))
}

fn short(command: &str) -> String {
    let line = command.lines().next().unwrap_or("").trim();
    if line.chars().count() > 80 {
        format!("{}…", line.chars().take(80).collect::<String>())
    } else {
        line.to_string()
    }
}

impl Ledger {
    pub(super) fn record_inspection(&mut self, effectful: bool, inspection: Option<bool>) {
        self.attempted_effect |= effectful;
        if let Some(ok) = inspection {
            self.last_inspection_ok = ok;
        }
    }

    pub(super) fn verified_inspection(&self) -> bool {
        self.last_inspection_ok && !self.attempted_effect && self.changed.is_empty()
    }

    /// The request's own words: folders it names to be created are expected to hold what the run writes.
    pub(super) fn expect_from(&mut self, goal: &str) {
        self.expected_folders = folders_named(goal);
    }

    /// One finished tool call: `ok` is the call's own status; a command's exit code decides whether a check passed.
    pub(super) fn record(
        &mut self,
        name: &str,
        args: &Value,
        ok: bool,
        output: &str,
        job: Option<&str>,
    ) {
        let text = |key: &str| args[key].as_str().unwrap_or("").to_string();
        match name {
            "write_file" | "edit_file" if ok => self.change(text("path")),
            "apply_patch" if ok => {
                for edit in args["edits"].as_array().into_iter().flatten() {
                    self.change(format!(
                        "{}/{}",
                        text("workdir").trim_end_matches('/'),
                        edit["path"].as_str().unwrap_or("")
                    ));
                }
            }
            "shell" => {
                let command = text("command");
                let reads_written_text_file = command.split_whitespace().next() == Some("cat")
                    && self.changed.iter().any(|path| {
                        path.ends_with(".txt")
                            && command
                                .split_whitespace()
                                .any(|word| path.rsplit('/').next() == Some(word))
                    });
                if syntax_only(&command)
                    || (!check_command().is_match(&command)
                        && !runs_changed_file(&command, &self.changed)
                        && !reads_written_text_file)
                {
                    return;
                }
                let covers = self.coverage_of(&command);
                if let Some(job) = job {
                    self.jobs.insert(job.to_string(), command);
                    return;
                }
                let label = short(&command);
                if command.contains("curl") && local_server().is_match(&command) {
                    // A deliberate bad request answered with a 4xx is a passing probe even when `curl -f` exits 22; a 5xx never is.
                    let client_error = got_client_error(output);
                    // Only calls to an API count towards "tried it with good requests only": a page answering 200 is its own check.
                    if api_like_curl(&command) {
                        self.probes += 1;
                        self.bad_probe |= client_error;
                    }
                    let server_error = Regex::new(r"HTTP/[\d.]+ 5\d\d\b")
                        .is_ok_and(|pattern| pattern.is_match(output));
                    let reached = ok
                        && (exit_code(output).is_none_or(|code| code == 0)
                            || (client_error && exit_code(output) == Some(22)));
                    self.checked_for(label, reached && !server_error, covers);
                    return;
                }
                if ran_no_tests(output) {
                    self.checked(format!("{label} (hiç test çalışmadı)"), false);
                    return;
                }
                self.checked_for(
                    label,
                    ok && exit_code(output).is_none_or(|code| code == 0),
                    covers,
                );
            }
            "job" if args["action"] == "wait" => {
                let Some(command) = self.jobs.get(&text("job_id")).cloned() else {
                    return;
                };
                // A job still running has no verdict yet.
                if output.contains("\\\"running\\\"") || output.contains("\"status\":\"running\"") {
                    return;
                }
                if let Some(code) = exit_code(output) {
                    self.jobs.remove(&text("job_id"));
                    let covers = self.coverage_of(&command);
                    if ran_no_tests(output) {
                        self.checked_for(
                            format!("{} (hiç test çalışmadı)", short(&command)),
                            false,
                            covers,
                        );
                    } else {
                        self.checked_for(short(&command), ok && code == 0, covers);
                    }
                }
            }
            // A page opened in a browser and used: it speaks for the web files of the work (the page, its styles and scripts), when nothing went wrong.
            "browser_check" => {
                // `ok` is the report's own: nothing in the console, nothing failed to load, every step held.
                let passed = ok;
                let web: Vec<String> = self
                    .changed
                    .iter()
                    .filter(|path| {
                        [".html", ".htm", ".css", ".js", ".mjs", ".svg", ".json"]
                            .iter()
                            .any(|ext| path.ends_with(ext))
                    })
                    .cloned()
                    .collect();
                self.checked_for(
                    format!("sayfa denemesi: {}", short(&text("url"))),
                    passed,
                    Some(web),
                );
            }
            "api" if args["operation"] == "call" => {
                let label = format!(
                    "{} {}{}",
                    args["method"].as_str().unwrap_or("GET"),
                    text("name"),
                    text("path")
                );
                self.checked(label, ok);
            }
            _ => {}
        }
    }

    pub(super) fn record_plan_check(&mut self, command: &str, passed: bool, output: &str) -> bool {
        let command = command.trim();
        let no_op = matches!(command, "" | ":" | "true" | "exit 0" | "exit 0;" | "$true");
        let passed = passed && !no_op && !ran_no_tests(output);
        self.checked_for(short(command), passed, self.coverage_of(command));
        passed
    }

    /// A file the run just wrote does not parse (its own language's syntax check said so): a failed check after the change, so the report says it and a "done" cannot stand over it.
    pub(super) fn syntax_failed(&mut self, path: &str) {
        let name = path.rsplit('/').next().unwrap_or(path);
        self.checked_for(format!("{name}: sözdizimi hatalı"), false, Some(Vec::new()));
    }

    fn change(&mut self, path: String) {
        self.edits += 1;
        // Probes before this change say nothing about the new code.
        self.probes = 0;
        self.bad_probe = false;
        if !path.is_empty() {
            self.last_edit.insert(path.clone(), self.edits);
            if !self.changed.contains(&path) {
                self.changed.push(path);
            }
        }
    }

    fn checked(&mut self, label: String, passed: bool) {
        self.checked_for(label, passed, None);
    }

    fn checked_for(&mut self, label: String, passed: bool, covers: Option<Vec<String>>) {
        self.checks.push(Check {
            label,
            passed,
            after: self.edits,
            covers,
        });
    }

    /// What a check command speaks for: running a changed script speaks for that script; a page fetched from a local server for everything but plain browser scripts;
    /// a test run, build or type check for the whole project.
    fn coverage_of(&self, command: &str) -> Option<Vec<String>> {
        if command.contains("curl") && local_server().is_match(command) {
            return Some(
                self.changed
                    .iter()
                    .filter(|path| {
                        if !static_script(path) {
                            return true;
                        }
                        api_like_curl(command)
                            && std::path::Path::new(path)
                                .file_stem()
                                .and_then(|name| name.to_str())
                                .is_some_and(|name| matches!(name, "server" | "api" | "routes"))
                    })
                    .cloned()
                    .collect(),
            );
        }
        if check_command().is_match(command) {
            return None;
        }
        // A changed script that was run by name (scratch files are its own harness, not the work).
        let scratch = |path: &str| {
            path.starts_with("/tmp/")
                || path.starts_with("/private/tmp/")
                || path.starts_with("/var/folders/")
        };
        Some(
            self.changed
                .iter()
                .filter(|path| !scratch(path))
                .filter(|path| {
                    path.rsplit('/')
                        .next()
                        .is_some_and(|name| name.contains('.') && command.contains(name))
                })
                .cloned()
                .collect(),
        )
    }

    /// How the run ended, for the project's memory: from the checks, not from the model's words.
    pub(super) fn outcome(&self) -> &'static str {
        if self.edits == 0 {
            "değişiklik yok"
        } else if self.unchecked_changes() {
            "doğrulanmadı"
        } else {
            "doğrulandı"
        }
    }

    pub(super) fn changed_paths(&self) -> &[String] {
        &self.changed
    }

    /// The latest result of each check that ran.
    pub(super) fn checks_summary(&self) -> Vec<(String, bool)> {
        let mut latest: Vec<(String, bool)> = Vec::new();
        for check in self.checks.iter().rev() {
            if !latest.iter().any(|(label, _)| *label == check.label) {
                latest.push((check.label.clone(), check.passed));
            }
        }
        latest
    }

    /// The files changed that no passed check after their last change speaks for.
    pub(super) fn uncovered(&self) -> Vec<&String> {
        self.changed
            .iter()
            .filter(|path| {
                let last = self.last_edit.get(*path).copied().unwrap_or(0);
                !self.checks.iter().any(|check| {
                    check.passed
                        && check.after >= last
                        && check
                            .covers
                            .as_ref()
                            .is_none_or(|files| files.iter().any(|file| file == *path))
                })
            })
            .collect()
    }

    /// Files were changed and nothing has checked them since the last change.
    pub(super) fn unchecked_changes(&self) -> bool {
        self.edits > 0
            && (self
                .checks
                .last()
                .is_none_or(|check| check.after < self.edits || !check.passed)
                || !self.uncovered().is_empty())
    }

    /// Why a "done" answer goes back once more, from what the tool results show: nothing ran after the last change (twice at most, the
    /// second time naming the script it wrote), or a server was only called with requests that succeed (once).
    pub(super) fn turn_back(&mut self) -> Option<String> {
        if self.unchecked_changes() && self.nudges < 2 {
            self.nudges += 1;
            let ran_nothing = self.checks.iter().all(|check| check.after < self.edits);
            let script = self.changed.iter().find(|path| {
                [".py", ".js", ".mjs", ".ts", ".sh", ".rb", ".go"]
                    .iter()
                    .any(|ext| path.ends_with(ext))
            });
            let mut why = if ran_nothing {
                "Dosyaları değiştirdin ama son değişiklikten sonra hiçbir şey çalıştırmadın. Bitirmeden önce gerçekten çalıştır: projenin testini/derlemesini, yazdığın betiği örnek girdiyle, sunucuyu başlatıp uç noktalarını curl ile.".to_string()
            } else {
                "Son değişiklikten sonra geçen bir doğrulama yok (başarısız oldu ya da hiç test çalışmadı). Çıktıyı oku, düzelt ve yeniden çalıştır; \"Ran 0 tests\" gibi bir sonuç doğrulama değildir, testlerin gerçekten bulunduğu komutu kullan.".to_string()
            };
            if self.nudges == 2 {
                if let Some(script) = script {
                    why.push_str(&format!(" Örneğin {script} dosyasını çalıştır ve çıktısının istenenle aynı olduğuna bak."));
                }
            }
            let uncovered = self.uncovered();
            if !uncovered.is_empty() {
                let names: Vec<&str> = uncovered
                    .iter()
                    .take(6)
                    .map(|path| path.rsplit('/').next().unwrap_or(path.as_str()))
                    .collect();
                why.push_str(&format!(" Şu dosyalar için hiçbir denetim çalışmadı: {}. Bir betiği doğrulamak için kendisini çalıştır ya da içe aktar (başka yere kopyalayıp değil); tarayıcıda çalışan sayfa ve betikler için browser_check kullan; yalnız sözdizimi denetimi (node --check) doğrulama sayılmaz.", names.join(", ")));
            }
            why.push_str(" Ortam gerçekten yoksa (araç kurulu değil, veritabanı yok) çalıştırmadan bitir ve yanıtında neyin doğrulanmadığını açıkça yaz; çalışıyor deme.");
            return Some(why);
        }
        if !self.placement_nudged && !self.changed.is_empty() {
            let missing: Vec<&String> = self
                .expected_folders
                .iter()
                .filter(|folder| {
                    !self
                        .changed
                        .iter()
                        .any(|path| path.split('/').any(|part| part == folder.as_str()))
                })
                .collect();
            if let Some(folder) = missing.first() {
                self.placement_nudged = true;
                let written: Vec<&str> = self.changed.iter().take(4).map(String::as_str).collect();
                return Some(format!("İstekte \"{folder}\" adında bir klasör geçiyor ama yazdığın dosyalar bu klasörün içinde değil ({}). Dosyayı istenen klasörün içine yaz ya da taşı, sonra `ls -la <tam yol>` ile gör; yanıtında dosyanın tam yolunu yaz. Klasör içinde olmayan bir dosyaya \"{folder} içinde\" deme.", written.join(", ")));
            }
        }
        if self.probes > 0 && !self.bad_probe && !self.edge_nudged && !self.unchecked_changes() {
            self.edge_nudged = true;
            return Some("Sunucuyu yalnız başarılı isteklerle denedin. Bitirmeden en az şunları dene ve durum kodunu gör (curl -s -o /dev/null -w '%{http_code}'): bozuk JSON gövdesi, eksik ya da yalnız boşluktan oluşan zorunlu alan, yanlış tipte değer, olmayan bir id. Sunucu çökmemeli, 400/404/422 gibi anlamlı bir kod dönmeli; çökerse ya da 500/200 dönerse düzelt ve yeniden dene.".to_string());
        }
        None
    }

    /// The three lists under the answer; nothing when the run neither changed nor checked anything.
    pub(super) fn report(&self) -> Option<String> {
        if self.changed.is_empty() && self.checks.is_empty() {
            return None;
        }
        let mut lines = vec!["DOĞRULAMA".to_string()];
        if !self.changed.is_empty() {
            // The paths are the ones the tools wrote, looked at again now: a file that is not at its path is said so, not listed as done.
            let shown: Vec<String> = self
                .changed
                .iter()
                .take(8)
                .map(|path| {
                    if is_there(path) {
                        path.clone()
                    } else {
                        format!("{path} (bulunamadı)")
                    }
                })
                .collect();
            lines.push(format!(
                "Yapıldı: {} dosya değişti — {}{}",
                self.changed.len(),
                shown.join(", "),
                if self.changed.len() > 8 { ", …" } else { "" }
            ));
        }
        // Only the latest result of each check, and only checks run after the last change prove the current code.
        let mut latest: Vec<&Check> = Vec::new();
        for check in self.checks.iter().rev() {
            if !latest.iter().any(|seen| seen.label == check.label) {
                latest.push(check);
            }
        }
        latest.reverse();
        let current: Vec<&&Check> = latest
            .iter()
            .filter(|check| check.after == self.edits)
            .collect();
        let passed: Vec<&str> = current
            .iter()
            .filter(|check| check.passed)
            .map(|check| check.label.as_str())
            .collect();
        let failed: Vec<&str> = current
            .iter()
            .filter(|check| !check.passed)
            .map(|check| check.label.as_str())
            .collect();
        if !passed.is_empty() {
            lines.push(format!("Doğrulandı: {}", passed.join("; ")));
        }
        if !failed.is_empty() {
            lines.push(format!("Doğrulanmadı (başarısız): {}", failed.join("; ")));
        }
        if self.edits > 0 && current.is_empty() {
            lines.push("Doğrulanmadı: son değişiklikten sonra test, derleme ya da API denemesi çalıştırılmadı.".into());
        }
        // The files no passed check speaks for, whatever else passed: a test run of one part says nothing of a browser script nobody ran.
        let uncovered = self.uncovered();
        if !uncovered.is_empty() {
            let names: Vec<&str> = uncovered
                .iter()
                .take(6)
                .map(|path| path.rsplit('/').next().unwrap_or(path.as_str()))
                .collect();
            lines.push(format!(
                "Doğrulanmayan dosyalar (hiçbir denetim bunları kapsamadı): {}",
                names.join(", ")
            ));
        }
        Some(lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_successful_no_op_or_empty_test_suite_does_not_verify_a_plan_step() {
        let mut ledger = Ledger::default();
        assert!(!ledger.record_plan_check("exit 0", true, ""));
        assert!(!ledger.record_plan_check("cargo test", true, "test result: ok. 0 passed"));
        assert!(ledger.record_plan_check("cargo test", true, "test result: ok. 1 passed"));
    }

    #[test]
    fn only_checks_after_the_last_change_count_and_exit_codes_decide() {
        let mut ledger = Ledger::default();
        ledger.record("write_file", &json!({ "path": "/p/a.ts" }), true, "", None);
        ledger.record(
            "shell",
            &json!({ "command": "npm test" }),
            true,
            r#"{"output":"{\"exitCode\":0}"}"#,
            None,
        );
        assert!(!ledger.unchecked_changes());
        ledger.record("edit_file", &json!({ "path": "/p/b.ts" }), true, "", None);
        assert!(
            ledger.unchecked_changes(),
            "a change after the test is not covered by it"
        );
        ledger.record(
            "shell",
            &json!({ "command": "npm run build" }),
            true,
            r#"{"output":"{\"exitCode\":2}"}"#,
            None,
        );
        let report = ledger.report().unwrap();
        assert!(report.contains("Yapıldı: 2 dosya"), "{report}");
        assert!(
            report.contains("Doğrulanmadı (başarısız): npm run build"),
            "{report}"
        );
        assert!(
            !report.contains("Doğrulandı: npm test"),
            "the earlier test does not prove the later change: {report}"
        );
        ledger.record(
            "shell",
            &json!({ "command": "npm test" }),
            true,
            "",
            Some("job-1"),
        );
        ledger.record(
            "job",
            &json!({ "action": "wait", "job_id": "job-1" }),
            true,
            r#"{"output":"{\"status\":\"exited\",\"exitCode\":0}"}"#,
            None,
        );
        assert!(ledger.report().unwrap().contains("Doğrulandı: npm test"));
        ledger.record("shell", &json!({ "command": "ls -la" }), true, "", None);
        assert_eq!(ledger.checks.len(), 3, "listing a folder is not a check");
    }

    #[test]
    fn a_browser_script_is_not_verified_by_a_page_answering_a_copy_a_syntax_check_or_a_failed_page_but_by_a_page_that_ran(
    ) {
        let mut ledger = Ledger::default();
        ledger.record(
            "write_file",
            &json!({ "path": "/Users/x/Desktop/matris/index.html" }),
            true,
            "",
            None,
        );
        ledger.record(
            "write_file",
            &json!({ "path": "/Users/x/Desktop/matris/script.js" }),
            true,
            "",
            None,
        );
        ledger.record(
            "write_file",
            &json!({ "path": "/tmp/matrix_test.js" }),
            true,
            "",
            None,
        );
        // The page answers, a copy of the functions passes, the file parses: none of that speaks for script.js.
        ledger.record(
            "shell",
            &json!({ "command": "curl -s http://localhost:8000/ | grep -c Matris" }),
            true,
            r#"{"output":"{\"exitCode\":0}"}"#,
            None,
        );
        ledger.record(
            "shell",
            &json!({ "command": "node /tmp/matrix_test.js" }),
            true,
            r#"{"output":"{\"exitCode\":0}"}"#,
            None,
        );
        ledger.record(
            "shell",
            &json!({ "command": "node --check /Users/x/Desktop/matris/script.js" }),
            true,
            r#"{"output":"{\"exitCode\":0}"}"#,
            None,
        );
        assert!(ledger.unchecked_changes());
        let report = ledger.report().unwrap();
        assert!(
            report.contains("Doğrulanmayan dosyalar") && report.contains("script.js"),
            "{report}"
        );
        let why = ledger.turn_back().unwrap();
        assert!(
            why.contains("script.js") && why.contains("browser_check"),
            "{why}"
        );
        // A page that threw does not verify it either; one that ran and held does.
        ledger.record(
            "browser_check",
            &json!({ "url": "http://localhost:8000/" }),
            false,
            "{\"ok\":false}",
            None,
        );
        assert!(ledger.unchecked_changes());
        ledger.record(
            "browser_check",
            &json!({ "url": "http://localhost:8000/" }),
            true,
            "{\"ok\":true}",
            None,
        );
        assert!(!ledger.unchecked_changes(), "{:?}", ledger.uncovered());
    }

    #[test]
    fn a_scratch_script_does_not_check_the_work_but_the_real_file_run_does() {
        let changed = vec![
            "/tmp/matrix_test.js".to_string(),
            "/Users/x/Desktop/matris/script.js".to_string(),
        ];
        assert!(!super::runs_changed_file(
            "node /tmp/matrix_test.js",
            &changed
        ));
        assert!(super::runs_changed_file(
            "node /Users/x/Desktop/matris/script.js",
            &changed
        ));
    }

    #[test]
    fn zero_tests_do_not_verify_and_running_a_written_script_does() {
        let mut ledger = Ledger::default();
        ledger.record(
            "write_file",
            &json!({ "path": "/p/tests/test_count.py" }),
            true,
            "",
            None,
        );
        ledger.record(
            "shell",
            &json!({ "command": "python3 -m unittest" }),
            true,
            r#"{"output":"{\"exitCode\":0,\"stderr\":\"Ran 0 tests in 0.000s\\nOK\"}"}"#,
            None,
        );
        assert!(ledger.unchecked_changes(), "Ran 0 tests proves nothing");
        assert!(ledger.report().unwrap().contains("hiç test çalışmadı"));
        let first = ledger.turn_back().unwrap();
        assert!(first.contains("Ran 0 tests"), "{first}");

        let mut ledger = Ledger::default();
        ledger.record(
            "write_file",
            &json!({ "path": "/p/report.py" }),
            true,
            "",
            None,
        );
        assert!(ledger
            .turn_back()
            .unwrap()
            .contains("hiçbir şey çalıştırmadın"));
        assert!(
            ledger.turn_back().unwrap().contains("/p/report.py"),
            "the second turn-back names the script"
        );
        assert!(ledger.turn_back().is_none(), "two turn-backs at most");
        ledger.record(
            "shell",
            &json!({ "command": "python3 report.py sales.csv" }),
            true,
            r#"{"output":"{\"exitCode\":0}"}"#,
            None,
        );
        assert!(
            !ledger.unchecked_changes(),
            "running the script it wrote is a check"
        );
    }

    #[test]
    fn a_page_on_a_dev_server_is_checked_by_its_answer_not_by_bad_requests() {
        let mut ledger = Ledger::default();
        ledger.record(
            "write_file",
            &json!({ "path": "/p/src/App.jsx" }),
            true,
            "",
            None,
        );
        for command in [
            "curl -s -o /dev/null -w '%{http_code}' http://localhost:5173",
            "curl -s localhost:5173/",
            "curl -s http://localhost:5173/src/main.jsx",
        ] {
            ledger.record(
                "shell",
                &json!({ "command": command }),
                true,
                r#"{"output":"{\"exitCode\":0,\"stdout\":\"200\"}"}"#,
                None,
            );
        }
        assert!(!ledger.unchecked_changes(), "the page answering is a check");
        assert!(
            ledger.turn_back().is_none(),
            "no bad-request nudge for a static page"
        );
        assert!(api_like_curl("curl -s localhost:3000/notes"));
        assert!(api_like_curl("curl -X POST localhost:3000/"));
        assert!(api_like_curl("curl -d '{}' http://127.0.0.1:8000"));
        assert!(!api_like_curl("curl -s localhost:5173"));
        assert!(!api_like_curl("curl -s localhost:5173/index.html"));
    }

    #[test]
    fn a_file_written_beside_the_folder_the_request_names_goes_back_once() {
        assert_eq!(
            folders_named(
                "Masaüstünde wiqo-test adında bir klasör oluştur ve içine merhaba.txt yaz"
            ),
            vec!["wiqo-test".to_string()]
        );
        assert_eq!(
            folders_named("create a folder named notes and put a.txt in it"),
            vec!["notes".to_string()]
        );
        assert!(
            folders_named("indirilenler klasöründeki dosyaları listele").is_empty(),
            "a folder only mentioned is not expected"
        );

        let mut ledger = Ledger::default();
        ledger.expect_from(
            "Masaüstünde wiqo-test adında bir klasör oluştur ve içine merhaba.txt yaz",
        );
        ledger.record(
            "write_file",
            &json!({ "path": "/Users/a/Desktop/merhaba.txt" }),
            true,
            "",
            None,
        );
        ledger.record(
            "shell",
            &json!({ "command": "cat merhaba.txt && ls" }),
            true,
            "",
            None,
        );
        let why = ledger.turn_back().expect("written beside the folder");
        assert!(
            why.contains("wiqo-test") && why.contains("/Users/a/Desktop/merhaba.txt"),
            "{why}"
        );
        assert!(ledger.turn_back().is_none(), "once");

        let mut ledger = Ledger::default();
        ledger.expect_from("wiqo-test adında bir klasör oluştur ve içine merhaba.txt yaz");
        ledger.record(
            "write_file",
            &json!({ "path": "/Users/a/Desktop/wiqo-test/merhaba.txt" }),
            true,
            "",
            None,
        );
        assert!(
            ledger
                .turn_back()
                .is_some_and(|why| why.contains("hiçbir şey çalıştırmadın")),
            "in the folder: only the unchecked-change nudge"
        );
    }

    #[test]
    fn the_report_says_when_a_written_file_is_not_at_its_path() {
        let mut ledger = Ledger::default();
        ledger.record(
            "write_file",
            &json!({ "path": "/definitely/not/here/merhaba.txt" }),
            true,
            "",
            None,
        );
        let report = ledger.report().unwrap();
        assert!(
            report.contains("/definitely/not/here/merhaba.txt (bulunamadı)"),
            "{report}"
        );
    }

    #[test]
    fn a_server_tried_only_with_good_requests_goes_back_once_for_bad_ones() {
        let mut ledger = Ledger::default();
        ledger.record(
            "write_file",
            &json!({ "path": "/p/server.js" }),
            true,
            "",
            None,
        );
        ledger.record(
            "shell",
            &json!({ "command": "curl -s localhost:3000/notes" }),
            true,
            r#"{"output":"{\"exitCode\":0,\"stdout\":\"[]\"}"}"#,
            None,
        );
        assert!(ledger.turn_back().unwrap().contains("bozuk JSON"));
        assert!(ledger.turn_back().is_none(), "once");

        let mut ledger = Ledger::default();
        ledger.record(
            "write_file",
            &json!({ "path": "/p/server.js" }),
            true,
            "",
            None,
        );
        ledger.record(
            "shell",
            &json!({ "command": "curl -sf -d '{bad' localhost:3000/notes" }),
            true,
            r#"{"output":"{\"exitCode\":22,\"stdout\":\"HTTP/1.1 400 Bad Request\"}"}"#,
            None,
        );
        assert!(
            !ledger.unchecked_changes(),
            "a 4xx to a bad request is a passing probe"
        );
        assert!(ledger.turn_back().is_none());
        ledger.record(
            "shell",
            &json!({ "command": "curl -si localhost:3000/notes/x" }),
            true,
            r#"{"output":"{\"exitCode\":0,\"stdout\":\"HTTP/1.1 500 Internal\"}"}"#,
            None,
        );
        assert!(ledger.unchecked_changes(), "a 5xx is a failure");
    }
}
