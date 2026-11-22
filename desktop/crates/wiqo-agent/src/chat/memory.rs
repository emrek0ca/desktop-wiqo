//! What this computer remembers about each project it has worked in, kept on this computer only.
//!
//! The agent used to meet every project as a stranger: which command had passed last time, what had broken and how it was fixed, what the person had said not to
//! touch, all of it was gone with the conversation. The one note that existed was written by the model when it thought of it (almost never). This keeps a
//! small, plain record per project, written by the run's own ledger (not by the model's say-so):
//!
//! * `runs.jsonl`: one line per finished run (the request, the files touched, how it ended, the errors it ended on, the git commit it left the project at);
//! * `commands.json`: the check commands that passed or failed in this project, counted, with the last error of a failing one;
//! * `kurallar.md`: what the person said to do or not do ("testlere dokunma"), a plain file they can open, edit and delete lines from;
//! * `notlar.md`: the note the model writes (`project_notes`), as before.
//!
//! It all goes into the project brief the next run starts with, with its dates and a word that it must be checked again today: a memory is a lead, not a fact.
//! Nothing here leaves the computer; what is written passes through the vault's stand-ins like everything else the model sees.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::Digest;

/// How many runs are kept per project, and how many the brief shows.
const KEEP_RUNS: usize = 60;
const SHOWN_RUNS: usize = 3;
const SHOWN_COMMANDS: usize = 6;
const RULES_CHARS: usize = 1_500;
const BRIEF_CHARS: usize = 3_500;

/// One finished run, as the ledger knows it.
pub struct RunRecord {
    pub goal: String,
    /// "doğrulandı" · "doğrulanmadı" · "değişiklik yok" · "durduruldu"
    pub outcome: &'static str,
    /// Files the run changed, relative to the project.
    pub files: Vec<String>,
    /// The checks that ran (the command as the ledger labelled it) and whether each passed.
    pub checks: Vec<(String, bool)>,
    /// The errors the run's last failing build or test ended on, a few lines.
    pub errors: Vec<String>,
    pub minutes: u64,
}

pub struct Memory {
    folder: PathBuf,
}

fn day(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .map(|time| time.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn clip(text: &str, most: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(most).collect()
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl Memory {
    /// The memory of one project: `<data>/project-memory/<folder name>-<hash of its path>/`, outside the project so its files and its git stay as they are.
    pub fn of(data: &Path, root: &Path) -> Option<Memory> {
        let digest = sha2::Sha256::digest(root.to_string_lossy().as_bytes());
        let hash: String = digest
            .iter()
            .take(4)
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let name: String = root
            .file_name()?
            .to_string_lossy()
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .take(40)
            .collect();
        let folder = data.join("project-memory").join(format!("{name}-{hash}"));
        std::fs::create_dir_all(&folder).ok()?;
        Some(Memory { folder })
    }

    pub fn notes_file(&self) -> PathBuf {
        self.folder.join("notlar.md")
    }

    pub fn rules_file(&self) -> PathBuf {
        self.folder.join("kurallar.md")
    }

    fn runs(&self) -> Vec<Value> {
        std::fs::read_to_string(self.folder.join("runs.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn commands(&self) -> BTreeMap<String, Value> {
        std::fs::read_to_string(self.folder.join("commands.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Writes a finished run: the line, the command counts, and the commit the project was left at.
    pub fn record(&self, root: &Path, run: &RunRecord) {
        let now = chrono::Utc::now().timestamp();
        let mut runs = self.runs();
        runs.push(json!({
            "at": now, "goal": clip(&run.goal, 240), "outcome": run.outcome, "files": run.files.iter().take(12).collect::<Vec<_>>(),
            "errors": run.errors.iter().take(3).map(|error| clip(error, 160)).collect::<Vec<_>>(), "minutes": run.minutes,
            "head": git(root, &["rev-parse", "--short", "HEAD"]), "dirty": git(root, &["status", "--porcelain"]).map(|text| text.lines().count()).unwrap_or(0)
        }));
        let from = runs.len().saturating_sub(KEEP_RUNS);
        let kept: Vec<String> = runs[from..].iter().map(Value::to_string).collect();
        let _ = std::fs::write(self.folder.join("runs.jsonl"), kept.join("\n") + "\n");

        let mut commands = self.commands();
        for (label, passed) in &run.checks {
            let entry = commands
                .entry(label.clone())
                .or_insert_with(|| json!({ "ok": 0, "fail": 0 }));
            let key = if *passed { "ok" } else { "fail" };
            entry[key] = json!(entry[key].as_i64().unwrap_or(0) + 1);
            entry[if *passed { "last_ok" } else { "last_fail" }] = json!(now);
            if !*passed {
                entry["last_error"] = json!(run
                    .errors
                    .first()
                    .map(|error| clip(error, 160))
                    .unwrap_or_default());
            }
        }
        // The least used are dropped first when there are too many.
        if commands.len() > 40 {
            let mut keys: Vec<(String, i64)> = commands
                .iter()
                .map(|(label, entry)| {
                    (
                        label.clone(),
                        entry["ok"].as_i64().unwrap_or(0) + entry["fail"].as_i64().unwrap_or(0),
                    )
                })
                .collect();
            keys.sort_by_key(|(_, uses)| *uses);
            for (label, _) in keys.into_iter().take(commands.len() - 40) {
                commands.remove(&label);
            }
        }
        let _ = std::fs::write(
            self.folder.join("commands.json"),
            serde_json::to_string_pretty(&commands).unwrap_or_default(),
        );
    }

    /// The commands that have passed here, best first (passes minus failures, then the newest): what a project's own checks turned out to be.
    pub fn proven_commands(&self) -> Vec<String> {
        let mut found: Vec<(i64, i64, String)> = self
            .commands()
            .into_iter()
            .filter(|(label, entry)| {
                entry["ok"].as_i64().unwrap_or(0) > entry["fail"].as_i64().unwrap_or(0)
                    && !label.contains("curl ")
                    && !label.contains("hiç test çalışmadı")
            })
            .map(|(label, entry)| {
                (
                    entry["ok"].as_i64().unwrap_or(0) - entry["fail"].as_i64().unwrap_or(0),
                    entry["last_ok"].as_i64().unwrap_or(0),
                    label,
                )
            })
            .collect();
        found.sort_by(|a, b| b.cmp(a));
        found
            .into_iter()
            .map(|(_, _, label)| label)
            .take(SHOWN_COMMANDS)
            .collect()
    }

    /// The files this project's runs touched most (relative to the project), most first: where the work tends to be.
    pub fn hot_files(&self, most: usize) -> Vec<String> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for run in self.runs() {
            for file in run["files"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                *counts.entry(file.to_string()).or_insert(0) += 1;
            }
        }
        let mut ranked: Vec<(usize, String)> = counts
            .into_iter()
            .map(|(file, count)| (count, file))
            .collect();
        ranked.sort_by(|a, b| b.cmp(a));
        ranked
            .into_iter()
            .map(|(_, file)| file)
            .take(most)
            .collect()
    }

    /// Adds what the person said to do or not do, once each (the same words are not written twice); the ones that were new.
    pub fn add_rules(&self, rules: &[String]) -> Vec<String> {
        if rules.is_empty() {
            return Vec::new();
        }
        let file = self.rules_file();
        let mut text = std::fs::read_to_string(&file).unwrap_or_else(|_| "# Bu projede senin kuralların\n\nBu dosyayı açıp düzenleyebilir, satır silebilirsin; Wiqo her işte okur.\n\n".to_string());
        let mut added = Vec::new();
        for rule in rules {
            if !text.to_lowercase().contains(&rule.to_lowercase()) {
                text.push_str(&format!(
                    "- {rule} ({})\n",
                    day(chrono::Utc::now().timestamp())
                ));
                added.push(rule.clone());
            }
        }
        if !added.is_empty() {
            let _ = std::fs::write(file, text);
        }
        added
    }

    pub fn rules(&self) -> String {
        let text = std::fs::read_to_string(self.rules_file()).unwrap_or_default();
        let lines: Vec<&str> = text
            .lines()
            .filter(|line| line.trim_start().starts_with("- "))
            .collect();
        lines.join("\n").chars().take(RULES_CHARS).collect()
    }

    /// What the next run starts with: its rules first, then what happened here before (dated), what passed and what keeps failing, and what changed since.
    pub fn brief(&self, root: &Path) -> String {
        let mut parts: Vec<String> = Vec::new();
        let rules = self.rules();
        if !rules.is_empty() {
            parts.push(format!(
                "SENİN KURALLARIN (kurallar.md; her zaman uy, değiştirmek için sor):\n{rules}"
            ));
        }
        let runs = self.runs();
        if !runs.is_empty() {
            let lines: Vec<String> = runs
                .iter()
                .rev()
                .take(SHOWN_RUNS)
                .map(|run| {
                    let files: Vec<&str> = run["files"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .take(4)
                        .collect();
                    let errors: Vec<&str> = run["errors"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .take(1)
                        .collect();
                    format!(
                        "- {}: {} → {}{}{}",
                        day(run["at"].as_i64().unwrap_or(0)),
                        clip(run["goal"].as_str().unwrap_or(""), 90),
                        run["outcome"].as_str().unwrap_or(""),
                        if files.is_empty() {
                            String::new()
                        } else {
                            format!("; dosyalar: {}", files.join(", "))
                        },
                        if errors.is_empty() {
                            String::new()
                        } else {
                            format!("; son hata: {}", errors[0])
                        }
                    )
                })
                .collect();
            parts.push(format!(
                "BU PROJEDE ÖNCEKİ ÇALIŞMALAR (bir ipucu: bugün yeniden doğrula):\n{}",
                lines.join("\n")
            ));
        }
        let hot = self.hot_files(8);
        if !hot.is_empty() {
            parts.push(format!(
                "BU PROJEDE EN ÇOK DOKUNULAN DOSYALAR (önceki çalışmalardan): {}",
                hot.join(", ")
            ));
        }
        let proven = self.proven_commands();
        if !proven.is_empty() {
            parts.push(format!(
                "BU PROJEDE DAHA ÖNCE GEÇEN KOMUTLAR (önce bunları dene):\n{}",
                proven
                    .iter()
                    .map(|command| format!("  {command}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        let mut cautions: Vec<String> = self
            .commands()
            .into_iter()
            .filter(|(_, entry)| {
                entry["fail"].as_i64().unwrap_or(0) >= 2
                    && entry["last_fail"].as_i64().unwrap_or(0)
                        >= entry["last_ok"].as_i64().unwrap_or(0)
            })
            .map(|(label, entry)| {
                format!(
                    "  {label}: {} kez başarısız{}",
                    entry["fail"],
                    entry["last_error"]
                        .as_str()
                        .filter(|error| !error.is_empty())
                        .map(|error| format!(" ({error})"))
                        .unwrap_or_default()
                )
            })
            .collect();
        cautions.truncate(3);
        if !cautions.is_empty() {
            parts.push(format!(
                "DİKKAT (son denemelerde hep başarısız; aynısını körlemesine yineleme):\n{}",
                cautions.join("\n")
            ));
        }
        if let Some(since) = self.since_last_run(root, &runs) {
            parts.push(since);
        }
        let text = parts.join("\n\n");
        text.chars().take(BRIEF_CHARS).collect()
    }

    /// What changed in the project since the last run left it (git): commits and files, so the person's own edits in between are seen, not written over.
    fn since_last_run(&self, root: &Path, runs: &[Value]) -> Option<String> {
        let head = runs.last()?["head"].as_str()?.to_string();
        let now = git(root, &["rev-parse", "--short", "HEAD"])?;
        let mut lines: Vec<String> = Vec::new();
        if now != head {
            if let Some(log) = git(
                root,
                &["log", "--oneline", "-n", "6", &format!("{head}..HEAD")],
            )
            .filter(|log| !log.is_empty())
            {
                lines.push(format!(
                    "{} yeni commit:\n{}",
                    log.lines().count(),
                    log.lines()
                        .map(|line| format!("  {line}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
        }
        if let Some(stat) = git(root, &["diff", "--stat", "--stat-width=100", &head])
            .filter(|stat| !stat.is_empty())
        {
            let last = stat.lines().last().unwrap_or("").trim().to_string();
            lines.push(format!("Son çalışmadan bu yana dosya değişikliği: {last}"));
        }
        (!lines.is_empty()).then(|| format!("SON ÇALIŞMADAN BU YANA (sen ya da başkası değiştirmiş olabilir; üzerine yazma, önce oku):\n{}", lines.join("\n")))
    }
}

/// The sentences of a request that say what must not (or must) be done: "testlere dokunma", "dosyayı silme", "only change src/", "do not touch the lockfile".
/// They are pinned: they go on every context's card and into `kurallar.md`.
pub fn constraints_of(text: &str) -> Vec<String> {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let cue = PATTERN.get_or_init(|| {
        regex::Regex::new(r"(?i)dokunma|silme\b|silmeden|değiştirme|yapma\b|yazma\b|sakın|\bsadece\b|\byalnız(ca)?\b|hariç|dışında|olmadan|\bonly\b|don'?t|do not|never|without|leave .* alone|must not")
            .expect("constraint pattern")
    });
    let mut found: Vec<String> = Vec::new();
    for sentence in text.split(['.', '!', '?', '\n', ';']) {
        let sentence = sentence.trim();
        if sentence.chars().count() < 8 || !cue.is_match(sentence) {
            continue;
        }
        let clipped: String = clip(sentence, 160);
        if !found.contains(&clipped) {
            found.push(clipped);
        }
        if found.len() >= 6 {
            break;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("wiqo-memory-{name}-{}", std::process::id()));
        let root = base.join("proje");
        std::fs::create_dir_all(&root).expect("project");
        (base, root)
    }

    #[test]
    fn a_run_is_remembered_and_the_next_brief_says_what_passed_what_failed_and_the_rules() {
        let (base, root) = scratch("brief");
        let memory = Memory::of(&base, &root).expect("memory");
        memory.add_rules(&["testlere dokunma".to_string()]);
        memory.add_rules(&["Testlere dokunma".to_string()]);
        for _ in 0..2 {
            memory.record(
                &root,
                &RunRecord {
                    goal: "testleri düzelt".into(),
                    outcome: "doğrulanmadı",
                    files: vec!["a.js".into()],
                    checks: vec![
                        ("npm test".into(), false),
                        ("npx tsc --noEmit".into(), true),
                    ],
                    errors: vec!["a.js:3 TS2322: tip".into()],
                    minutes: 2,
                },
            );
        }
        let brief = memory.brief(&root);
        assert!(
            brief.contains("testlere dokunma") && brief.matches("testlere dokunma").count() == 1,
            "{brief}"
        );
        assert!(
            brief.contains("testleri düzelt") && brief.contains("doğrulanmadı"),
            "{brief}"
        );
        assert!(brief.contains("npx tsc --noEmit"), "{brief}");
        assert!(
            brief.contains("npm test: 2 kez başarısız (a.js:3 TS2322: tip)"),
            "{brief}"
        );
        assert_eq!(
            memory.proven_commands(),
            vec!["npx tsc --noEmit".to_string()]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_sentences_that_set_a_limit_are_pinned() {
        let pins = constraints_of("Bu projede testleri düzelt, testlere dokunma. Sadece src klasöründe çalış. Güzel bir arayüz yap.");
        assert!(
            pins.iter().any(|pin| pin.contains("testlere dokunma"))
                && pins.iter().any(|pin| pin.contains("Sadece src")),
            "{pins:?}"
        );
        assert!(!pins.iter().any(|pin| pin.contains("arayüz")), "{pins:?}");
        assert!(constraints_of("Please do not touch the lockfile")
            .iter()
            .any(|pin| pin.contains("lockfile")));
    }
}
