//! What a run changed and what it actually checked, kept from the tool results themselves (not from what the model says), so the answer ends
//! with three plain lists: done (files written), verified (a test, build, lint or API call that passed after the last change) and not
//! verified (no check after the last change, or one that failed). The model cannot mark its own work as working.

use std::collections::HashMap;

use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

struct Check {
    label: String,
    passed: bool,
    /// How many file changes the run had made when this check ran: a check counts only for the changes before it.
    after: usize,
}

#[derive(Default)]
pub(super) struct Ledger {
    changed: Vec<String>,
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
}

fn check_command() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\b(test|tests|jest|vitest|mocha|pytest|unittest|tsc|typecheck|type-check|lint|eslint|biome|ruff|mypy|clippy|build|compile|check|vet|migrate|migration|prisma\s+(migrate|db)|knex|alembic|go\s+(test|build|vet)|cargo\s+(test|build|check|clippy)|swift\s+(test|build)|dotnet\s+(test|build)|gradle|mvn|make)\b|curl\s+[^|;&]*(localhost|127\.0\.0\.1)")
            .expect("check pattern")
    })
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
    PATTERN.get_or_init(|| Regex::new(r"HTTP/[\d.]+ 4\d\d\b|(^|[^\d.])(400|401|403|404|409|413|415|422)([^\d]|$)").expect("4xx pattern")).is_match(output)
}

/// Running a script the run itself wrote (`python3 report.py`, `node cli.js data.csv`) checks it too.
fn runs_changed_file(command: &str, changed: &[String]) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let interpreter = PATTERN.get_or_init(|| Regex::new(r"(?i)(^|[\s;&|(])(python3?|node|deno|bun|tsx|ts-node|ruby|bash|sh|php|go\s+run|uv\s+run)\s").expect("interpreter pattern"));
    interpreter.is_match(command)
        && changed.iter().filter_map(|path| path.rsplit('/').next()).filter(|name| name.contains('.')).any(|name| command.contains(name))
}

fn exit_code(output: &str) -> Option<i64> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| Regex::new(r#"exitCode\\?"\s*:\s*(-?\d+)"#).expect("exit pattern"));
    pattern.captures(output).and_then(|found| found[1].parse().ok())
}

fn local_server() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"localhost|127\.0\.0\.1|0\.0\.0\.0").expect("local pattern"))
}

fn short(command: &str) -> String {
    let line = command.lines().next().unwrap_or("").trim();
    if line.chars().count() > 80 { format!("{}…", line.chars().take(80).collect::<String>()) } else { line.to_string() }
}

impl Ledger {
    /// One finished tool call: `ok` is the call's own status; a command's exit code decides whether a check passed.
    pub(super) fn record(&mut self, name: &str, args: &Value, ok: bool, output: &str, job: Option<&str>) {
        let text = |key: &str| args[key].as_str().unwrap_or("").to_string();
        match name {
            "write_file" | "edit_file" if ok => self.change(text("path")),
            "apply_patch" if ok => {
                for edit in args["edits"].as_array().into_iter().flatten() {
                    self.change(format!("{}/{}", text("workdir").trim_end_matches('/'), edit["path"].as_str().unwrap_or("")));
                }
            }
            "shell" => {
                let command = text("command");
                if !check_command().is_match(&command) && !runs_changed_file(&command, &self.changed) {
                    return;
                }
                if let Some(job) = job {
                    self.jobs.insert(job.to_string(), command);
                    return;
                }
                let label = short(&command);
                if command.contains("curl") && local_server().is_match(&command) {
                    // A deliberate bad request answered with a 4xx is a passing probe even when `curl -f` exits 22; a 5xx never is.
                    let client_error = got_client_error(output);
                    self.probes += 1;
                    self.bad_probe |= client_error;
                    let server_error = Regex::new(r"HTTP/[\d.]+ 5\d\d\b").is_ok_and(|pattern| pattern.is_match(output));
                    let reached = ok && (exit_code(output).is_none_or(|code| code == 0) || (client_error && exit_code(output) == Some(22)));
                    self.checked(label, reached && !server_error);
                    return;
                }
                if ran_no_tests(output) {
                    self.checked(format!("{label} (hiç test çalışmadı)"), false);
                    return;
                }
                self.checked(label, ok && exit_code(output).is_none_or(|code| code == 0));
            }
            "job" if args["action"] == "wait" => {
                let Some(command) = self.jobs.get(&text("job_id")).cloned() else { return };
                // A job still running has no verdict yet.
                if output.contains("\\\"running\\\"") || output.contains("\"status\":\"running\"") {
                    return;
                }
                if let Some(code) = exit_code(output) {
                    self.jobs.remove(&text("job_id"));
                    if ran_no_tests(output) {
                        self.checked(format!("{} (hiç test çalışmadı)", short(&command)), false);
                    } else {
                        self.checked(short(&command), ok && code == 0);
                    }
                }
            }
            "api" if args["operation"] == "call" => {
                let label = format!("{} {}{}", args["method"].as_str().unwrap_or("GET"), text("name"), text("path"));
                self.checked(label, ok);
            }
            _ => {}
        }
    }

    fn change(&mut self, path: String) {
        self.edits += 1;
        // Probes before this change say nothing about the new code.
        self.probes = 0;
        self.bad_probe = false;
        if !path.is_empty() && !self.changed.contains(&path) {
            self.changed.push(path);
        }
    }

    fn checked(&mut self, label: String, passed: bool) {
        self.checks.push(Check { label, passed, after: self.edits });
    }

    /// Files were changed and nothing has checked them since the last change.
    pub(super) fn unchecked_changes(&self) -> bool {
        self.edits > 0 && self.checks.last().is_none_or(|check| check.after < self.edits || !check.passed)
    }

    /// Why a "done" answer goes back once more, from what the tool results show: nothing ran after the last change (twice at most, the
    /// second time naming the script it wrote), or a server was only called with requests that succeed (once).
    pub(super) fn turn_back(&mut self) -> Option<String> {
        if self.unchecked_changes() && self.nudges < 2 {
            self.nudges += 1;
            let ran_nothing = self.checks.iter().all(|check| check.after < self.edits);
            let script = self.changed.iter().find(|path| [".py", ".js", ".mjs", ".ts", ".sh", ".rb", ".go"].iter().any(|ext| path.ends_with(ext)));
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
            why.push_str(" Ortam gerçekten yoksa (araç kurulu değil, veritabanı yok) çalıştırmadan bitir ve yanıtında neyin doğrulanmadığını açıkça yaz; çalışıyor deme.");
            return Some(why);
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
            let shown: Vec<&str> = self.changed.iter().take(8).map(String::as_str).collect();
            lines.push(format!("Yapıldı: {} dosya değişti — {}{}", self.changed.len(), shown.join(", "), if self.changed.len() > 8 { ", …" } else { "" }));
        }
        // Only the latest result of each check, and only checks run after the last change prove the current code.
        let mut latest: Vec<&Check> = Vec::new();
        for check in self.checks.iter().rev() {
            if !latest.iter().any(|seen| seen.label == check.label) {
                latest.push(check);
            }
        }
        latest.reverse();
        let current: Vec<&&Check> = latest.iter().filter(|check| check.after == self.edits).collect();
        let passed: Vec<&str> = current.iter().filter(|check| check.passed).map(|check| check.label.as_str()).collect();
        let failed: Vec<&str> = current.iter().filter(|check| !check.passed).map(|check| check.label.as_str()).collect();
        if !passed.is_empty() {
            lines.push(format!("Doğrulandı: {}", passed.join("; ")));
        }
        if !failed.is_empty() {
            lines.push(format!("Doğrulanmadı (başarısız): {}", failed.join("; ")));
        }
        if self.edits > 0 && current.is_empty() {
            lines.push("Doğrulanmadı: son değişiklikten sonra test, derleme ya da API denemesi çalıştırılmadı.".into());
        }
        Some(lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_checks_after_the_last_change_count_and_exit_codes_decide() {
        let mut ledger = Ledger::default();
        ledger.record("write_file", &json!({ "path": "/p/a.ts" }), true, "", None);
        ledger.record("shell", &json!({ "command": "npm test" }), true, r#"{"output":"{\"exitCode\":0}"}"#, None);
        assert!(!ledger.unchecked_changes());
        ledger.record("edit_file", &json!({ "path": "/p/b.ts" }), true, "", None);
        assert!(ledger.unchecked_changes(), "a change after the test is not covered by it");
        ledger.record("shell", &json!({ "command": "npm run build" }), true, r#"{"output":"{\"exitCode\":2}"}"#, None);
        let report = ledger.report().unwrap();
        assert!(report.contains("Yapıldı: 2 dosya"), "{report}");
        assert!(report.contains("Doğrulanmadı (başarısız): npm run build"), "{report}");
        assert!(!report.contains("Doğrulandı: npm test"), "the earlier test does not prove the later change: {report}");
        ledger.record("shell", &json!({ "command": "npm test" }), true, "", Some("job-1"));
        ledger.record("job", &json!({ "action": "wait", "job_id": "job-1" }), true, r#"{"output":"{\"status\":\"exited\",\"exitCode\":0}"}"#, None);
        assert!(ledger.report().unwrap().contains("Doğrulandı: npm test"));
        ledger.record("shell", &json!({ "command": "ls -la" }), true, "", None);
        assert_eq!(ledger.checks.len(), 3, "listing a folder is not a check");
    }

    #[test]
    fn zero_tests_do_not_verify_and_running_a_written_script_does() {
        let mut ledger = Ledger::default();
        ledger.record("write_file", &json!({ "path": "/p/tests/test_count.py" }), true, "", None);
        ledger.record("shell", &json!({ "command": "python3 -m unittest" }), true, r#"{"output":"{\"exitCode\":0,\"stderr\":\"Ran 0 tests in 0.000s\\nOK\"}"}"#, None);
        assert!(ledger.unchecked_changes(), "Ran 0 tests proves nothing");
        assert!(ledger.report().unwrap().contains("hiç test çalışmadı"));
        let first = ledger.turn_back().unwrap();
        assert!(first.contains("Ran 0 tests"), "{first}");

        let mut ledger = Ledger::default();
        ledger.record("write_file", &json!({ "path": "/p/report.py" }), true, "", None);
        assert!(ledger.turn_back().unwrap().contains("hiçbir şey çalıştırmadın"));
        assert!(ledger.turn_back().unwrap().contains("/p/report.py"), "the second turn-back names the script");
        assert!(ledger.turn_back().is_none(), "two turn-backs at most");
        ledger.record("shell", &json!({ "command": "python3 report.py sales.csv" }), true, r#"{"output":"{\"exitCode\":0}"}"#, None);
        assert!(!ledger.unchecked_changes(), "running the script it wrote is a check");
    }

    #[test]
    fn a_server_tried_only_with_good_requests_goes_back_once_for_bad_ones() {
        let mut ledger = Ledger::default();
        ledger.record("write_file", &json!({ "path": "/p/server.js" }), true, "", None);
        ledger.record("shell", &json!({ "command": "curl -s localhost:3000/notes" }), true, r#"{"output":"{\"exitCode\":0,\"stdout\":\"[]\"}"}"#, None);
        assert!(ledger.turn_back().unwrap().contains("bozuk JSON"));
        assert!(ledger.turn_back().is_none(), "once");

        let mut ledger = Ledger::default();
        ledger.record("write_file", &json!({ "path": "/p/server.js" }), true, "", None);
        ledger.record("shell", &json!({ "command": "curl -sf -d '{bad' localhost:3000/notes" }), true, r#"{"output":"{\"exitCode\":22,\"stdout\":\"HTTP/1.1 400 Bad Request\"}"}"#, None);
        assert!(!ledger.unchecked_changes(), "a 4xx to a bad request is a passing probe");
        assert!(ledger.turn_back().is_none());
        ledger.record("shell", &json!({ "command": "curl -si localhost:3000/notes/x" }), true, r#"{"output":"{\"exitCode\":0,\"stdout\":\"HTTP/1.1 500 Internal\"}"}"#, None);
        assert!(ledger.unchecked_changes(), "a 5xx is a failure");
    }
}
