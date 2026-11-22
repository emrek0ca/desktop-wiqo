//! Long jobs that outlive a chat turn: a record of each on disk (`jobs.json`), the events the server hears (`pcJobEvent`), and the two things a job can
//! watch without a shell: a file that appears and stops growing, and a program that ends. The server never pushes work here; this computer owns the job
//! and says when it started and when it ended.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use wiqo_contract::schema_generated::{PcJobEvent, PcJobEventPhase, PcJobKind, PcJobStatus};
use wiqo_core::engine::JobEvents;

/// A job that runs longer than this is cut (6 hours), grant or no grant: it was started on purpose, but never open-ended.
pub const LONG_JOB_MAX_SECONDS: u64 = 6 * 60 * 60;
const KEPT: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub job_id: String,
    pub label: String,
    /// command | file | process
    pub kind: String,
    /// running | ok | failed | cancelled | unknown
    pub status: String,
    pub exit_code: Option<i64>,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub notify: bool,
    pub long: bool,
    #[serde(default)]
    pub conversation_id: Option<String>,
}

pub struct JobStore {
    path: PathBuf,
    records: Mutex<Vec<Record>>,
    events: Arc<JobEvents>,
}

impl JobStore {
    pub fn open(dir: &Path, events: Arc<JobEvents>) -> JobStore {
        let path = dir.join("jobs.json");
        let records = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        JobStore {
            path,
            records: Mutex::new(records),
            events,
        }
    }

    fn save(&self, records: &[Record]) {
        let Ok(bytes) = serde_json::to_vec(records) else {
            return;
        };
        let temporary = self.path.with_extension("tmp");
        if std::fs::write(&temporary, bytes).is_ok() {
            let _ = std::fs::rename(&temporary, &self.path);
        }
    }

    pub fn started(&self, record: Record) {
        let mut records = self.records.lock().expect("jobs lock");
        if records.iter().any(|existing| existing.job_id == record.job_id) {
            return;
        }
        records.push(record.clone());
        prune(&mut records);
        self.save(&records);
        if record.notify {
            self.emit(&record, PcJobEventPhase::Started);
        }
    }

    pub fn finished(&self, job_id: &str, status: &str, exit_code: Option<i64>) {
        let mut records = self.records.lock().expect("jobs lock");
        let Some(record) = records.iter_mut().find(|record| record.job_id == job_id) else {
            return;
        };
        if record.status != "running" {
            return;
        }
        record.status = status.to_string();
        record.exit_code = exit_code;
        record.finished_at_ms = Some(Utc::now().timestamp_millis());
        let snapshot = record.clone();
        prune(&mut records);
        self.save(&records);
        if snapshot.notify {
            self.emit(&snapshot, PcJobEventPhase::Finished);
        }
    }

    /// Jobs the previous run left "running": the process is gone, what it did is not known.
    pub fn recover(&self) {
        let mut records = self.records.lock().expect("jobs lock");
        let mut changed = false;
        for record in records.iter_mut().filter(|record| record.status == "running") {
            record.status = "unknown".into();
            record.finished_at_ms = Some(Utc::now().timestamp_millis());
            changed = true;
            if record.notify {
                self.emit(record, PcJobEventPhase::Finished);
            }
        }
        if changed {
            prune(&mut records);
            self.save(&records);
        }
    }

    #[cfg(test)]
    pub fn list(&self) -> Vec<Record> {
        self.records.lock().expect("jobs lock").clone()
    }

    fn emit(&self, record: &Record, phase: PcJobEventPhase) {
        let kind = match record.kind.as_str() {
            "file" => PcJobKind::File,
            "process" => PcJobKind::Process,
            _ => PcJobKind::Command,
        };
        let status = match record.status.as_str() {
            "running" => PcJobStatus::Running,
            "ok" => PcJobStatus::Ok,
            "failed" => PcJobStatus::Failed,
            "cancelled" => PcJobStatus::Cancelled,
            _ => PcJobStatus::Unknown,
        };
        let duration_ms = record.finished_at_ms.unwrap_or(record.started_at_ms)
            .saturating_sub(record.started_at_ms).max(0);
        let event = PcJobEvent {
            phase, job_id: record.job_id.clone(), kind: Some(kind), status: Some(status),
            exit_code: record.exit_code, duration_ms: Some(duration_ms),
        };
        if let Ok(value) = serde_json::to_value(event) {
            self.events.emit(value);
        }
    }
}

fn prune(records: &mut Vec<Record>) {
    let mut excess = records.len().saturating_sub(KEPT);
    records.retain(|record| {
        if excess > 0 && record.status != "running" {
            excess -= 1;
            false
        } else {
            true
        }
    });
}

/// What a watch waits for.
#[derive(Debug, Clone)]
pub enum Target {
    /// A file in `folder` whose name matches `pattern` (`*` and `?`), which has appeared and stopped growing.
    File { folder: PathBuf, pattern: String },
    /// A program, by name or process id, that is running and then is not.
    Process { name: Option<String>, pid: Option<u32> },
}

/// How a watch ended.
pub struct Watched {
    pub status: &'static str,
    pub message: String,
    pub paths: Vec<String>,
}

/// Waits for the target. Checks `cancel` every second; gives up (status `unknown`) after `seconds`.
pub fn watch(target: &Target, seconds: u64, cancel: &AtomicBool) -> Watched {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    match target {
        Target::File { folder, pattern } => {
            let mut last: Option<(PathBuf, u64)> = None;
            let mut steady = 0;
            while Instant::now() < deadline {
                if cancel.load(Ordering::SeqCst) {
                    return Watched { status: "cancelled", message: "İzleme durduruldu.".into(), paths: vec![] };
                }
                let found = newest_match(folder, pattern);
                match (&found, &last) {
                    (Some((path, size)), Some((before, before_size))) if path == before && size == before_size => steady += 1,
                    _ => steady = 0,
                }
                last = found;
                // Two checks in a row (about 4 s) without growth: the download or export is over.
                if steady >= 2 {
                    let path = last.as_ref().map(|(path, _)| path.to_string_lossy().to_string()).unwrap_or_default();
                    return Watched { status: "ok", message: format!("Dosya hazır: {path}"), paths: vec![path] };
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            Watched { status: "unknown", message: "Dosya süre içinde görünmedi.".into(), paths: vec![] }
        }
        Target::Process { name, pid } => {
            // A program that is not running at the start is waited for as well: it may be about to open, so it must be seen once before its end counts.
            let mut seen = false;
            let mut grace = 0;
            while Instant::now() < deadline {
                if cancel.load(Ordering::SeqCst) {
                    return Watched { status: "cancelled", message: "İzleme durduruldu.".into(), paths: vec![] };
                }
                if running(name.as_deref(), *pid) {
                    seen = true;
                    grace = 0;
                } else if seen {
                    return Watched { status: "ok", message: "İşlem bitti.".into(), paths: vec![] };
                } else {
                    grace += 1;
                    if grace >= 15 {
                        return Watched { status: "failed", message: "İzlenen işlem çalışmıyor.".into(), paths: vec![] };
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            Watched { status: "unknown", message: "İşlem süre içinde bitmedi.".into(), paths: vec![] }
        }
    }
}

fn matches(pattern: &str, name: &str) -> bool {
    fn go(pattern: &[char], name: &[char]) -> bool {
        match pattern.split_first() {
            None => name.is_empty(),
            Some(('*', rest)) => (0..=name.len()).any(|skip| go(rest, &name[skip..])),
            Some(('?', rest)) => !name.is_empty() && go(rest, &name[1..]),
            Some((c, rest)) => name.first().is_some_and(|n| n.eq_ignore_ascii_case(c)) && go(rest, &name[1..]),
        }
    }
    go(&pattern.chars().collect::<Vec<_>>(), &name.chars().collect::<Vec<_>>())
}

/// The newest file of the folder whose name fits, with its size. A browser's half-finished download (`.crdownload`, `.part`, `.download`) never counts.
fn newest_match(folder: &Path, pattern: &str) -> Option<(PathBuf, u64)> {
    let mut best: Option<(std::time::SystemTime, PathBuf, u64)> = None;
    for entry in std::fs::read_dir(folder).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_lowercase();
        if [".crdownload", ".part", ".download", ".tmp", ".partial"].iter().any(|end| lower.ends_with(end)) || !matches(pattern, &name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(at, _, _)| modified > *at) {
            best = Some((modified, entry.path(), meta.len()));
        }
    }
    best.map(|(_, path, size)| (path, size))
}

fn running(name: Option<&str>, pid: Option<u32>) -> bool {
    use std::process::{Command, Stdio};
    let ok = |mut command: Command| {
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .map(|out| (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string()))
            .unwrap_or((false, String::new()))
    };
    if cfg!(windows) {
        let mut command = Command::new("tasklist");
        match (pid, name) {
            (Some(pid), _) => command.args(["/FI", &format!("PID eq {pid}"), "/NH"]),
            (None, Some(name)) => command.args(["/FI", &format!("IMAGENAME eq {name}"), "/NH"]),
            _ => return false,
        };
        let (_, text) = ok(command);
        // tasklist prints an info line instead of a row when nothing matches.
        return !text.to_lowercase().contains("no tasks") && !text.trim().is_empty() && !text.contains("INFO:");
    }
    match (pid, name) {
        (Some(pid), _) => {
            let mut command = Command::new("kill");
            command.args(["-0", &pid.to_string()]);
            ok(command).0
        }
        (None, Some(name)) => {
            let mut command = Command::new("pgrep");
            command.args(["-x", name]);
            ok(command).0
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_match_names_without_regard_to_case() {
        assert!(matches("*.mp4", "Render.MP4"));
        assert!(matches("rapor-??.pdf", "rapor-01.pdf"));
        assert!(!matches("*.mp4", "render.mov"));
    }

    #[test]
    fn a_half_finished_download_is_not_a_match() {
        let dir = std::env::temp_dir().join(format!("wiqo-jobs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("film.mkv.crdownload"), "x").unwrap();
        assert!(newest_match(&dir, "*").is_none());
        std::fs::write(dir.join("film.mkv"), "xx").unwrap();
        assert_eq!(newest_match(&dir, "*").unwrap().1, 2);
    }

    #[test]
    fn a_file_that_stops_growing_ends_the_watch() {
        let dir = std::env::temp_dir().join(format!("wiqo-jobs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("out.zip"), "abc").unwrap();
        let target = Target::File { folder: dir.clone(), pattern: "*.zip".into() };
        let done = watch(&target, 30, &AtomicBool::new(false));
        assert_eq!(done.status, "ok");
        assert_eq!(done.paths.len(), 1);
    }

    #[test]
    fn jobs_left_running_are_told_as_not_known_after_a_restart() {
        let dir = std::env::temp_dir().join(format!("wiqo-jobs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let events = Arc::new(JobEvents::default());
        let (sender, mut reader) = tokio::sync::mpsc::unbounded_channel();
        events.attach(sender);
        let store = JobStore::open(&dir, events.clone());
        store.started(Record {
            job_id: "j1".into(), label: "Render".into(), kind: "command".into(), status: "running".into(), exit_code: None,
            started_at_ms: 0, finished_at_ms: None, notify: true, long: true, conversation_id: None,
        });
        let started = reader.try_recv().unwrap();
        assert_eq!(started["phase"], "started");
        assert!(started.get("label").is_none());
        let again = JobStore::open(&dir, events);
        again.recover();
        let told = reader.try_recv().unwrap();
        assert_eq!(told["status"], "unknown");
        assert_eq!(told["phase"], "finished");
        assert_eq!(again.list()[0].status, "unknown");
        let _ = std::fs::remove_dir_all(dir);
    }
}
