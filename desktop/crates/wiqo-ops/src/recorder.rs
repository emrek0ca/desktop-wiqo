//! "Bir kez göster, Wiqo öğrensin": the person starts a recording in this computer's panel, does a task once, and stops it. What is recorded is
//! what the task looked like from outside: which application and window came to the front, which address the browser tab moved to, which file
//! appeared in Downloads, Desktop or Documents, moved between them, changed or went away, and that something was copied (its length, never its
//! text). Keys typed and the clipboard's content are never recorded, and a private window is skipped. The person reads the steps before they
//! leave the computer; the server's model turns them into a recipe they can run again with one tap (rova-backend agent/skills.ts).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::Serialize;

const SAMPLE_EVERY: Duration = Duration::from_millis(900);
const FILES_EVERY: u32 = 3;
const MAX_EVENTS: usize = 300;
const MAX_MINUTES: i64 = 20;
const MAX_FILES_PER_FOLDER: usize = 3_000;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecordedEvent {
    pub at: DateTime<Utc>,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
}

impl RecordedEvent {
    fn new(kind: &'static str) -> RecordedEvent {
        RecordedEvent {
            at: Utc::now(),
            kind,
            app: None,
            title: None,
            url: None,
            path: None,
            change: None,
        }
    }
}

struct Recording {
    started: DateTime<Utc>,
    events: Arc<Mutex<Vec<RecordedEvent>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

static RECORDING: Mutex<Option<Recording>> = Mutex::new(None);

/// When the recording in progress started, if one is.
pub fn recording_since() -> Option<DateTime<Utc>> {
    RECORDING
        .lock()
        .expect("recorder lock")
        .as_ref()
        .map(|recording| recording.started)
}

/// Starts watching. A second start while one runs keeps the first.
pub fn start() -> DateTime<Utc> {
    let mut slot = RECORDING.lock().expect("recorder lock");
    if let Some(recording) = slot.as_ref() {
        return recording.started;
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let started = Utc::now();
    let thread = {
        let (events, stop) = (events.clone(), stop.clone());
        std::thread::Builder::new()
            .name("wiqo-recorder".into())
            .spawn(move || watch(events, stop, started))
            .ok()
    };
    *slot = Some(Recording {
        started,
        events,
        stop,
        thread,
    });
    started
}

/// Stops watching and hands back what was seen, in order.
pub fn stop() -> Vec<RecordedEvent> {
    let Some(mut recording) = RECORDING.lock().expect("recorder lock").take() else {
        return Vec::new();
    };
    recording.stop.store(true, Ordering::SeqCst);
    if let Some(thread) = recording.thread.take() {
        let _ = thread.join();
    }
    let events = recording
        .events
        .lock()
        .expect("recorder events lock")
        .clone();
    events
}

/// Stops and throws away what was seen.
pub fn cancel() {
    let _ = stop();
}

/// The folders whose files a task is watched in: Downloads, Desktop and Documents, their top level only.
fn watched_folders() -> Vec<(PathBuf, &'static str)> {
    [
        (dirs::download_dir(), "İndirilenler"),
        (dirs::desktop_dir(), "Masaüstü"),
        (dirs::document_dir(), "Belgeler"),
    ]
    .into_iter()
    .filter_map(|(dir, name)| dir.filter(|dir| dir.is_dir()).map(|dir| (dir, name)))
    .collect()
}

type Snapshot = HashMap<PathBuf, (u64, SystemTime)>;

fn snapshot(folders: &[(PathBuf, &'static str)]) -> Snapshot {
    let mut files = Snapshot::new();
    for (folder, _) in folders {
        let Ok(entries) = fs::read_dir(folder) else {
            continue;
        };
        for entry in entries.flatten().take(MAX_FILES_PER_FOLDER) {
            let name = entry.file_name().to_string_lossy().to_string();
            // Half-downloaded and hidden files are noise until they are whole.
            if name.starts_with('.')
                || [".crdownload", ".download", ".part", ".tmp"]
                    .iter()
                    .any(|end| name.ends_with(end))
            {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    files.insert(
                        entry.path(),
                        (
                            meta.len(),
                            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        ),
                    );
                }
            }
        }
    }
    files
}

/// What changed between two looks at the folders: a file that appeared (or was downloaded), moved between the folders, changed or went away.
fn file_events(
    before: &Snapshot,
    after: &Snapshot,
    downloads: Option<&PathBuf>,
) -> Vec<RecordedEvent> {
    let mut events = Vec::new();
    let appeared: Vec<&PathBuf> = after
        .keys()
        .filter(|path| !before.contains_key(*path))
        .collect();
    let gone: Vec<&PathBuf> = before
        .keys()
        .filter(|path| !after.contains_key(*path))
        .collect();
    let mut moved_from: Vec<&PathBuf> = Vec::new();
    for path in &appeared {
        let name = path.file_name();
        let size = after[*path].0;
        let origin = gone.iter().find(|old| {
            old.file_name() == name && before[**old].0 == size && !moved_from.contains(old)
        });
        let mut event = RecordedEvent::new(
            if origin.is_none() && downloads.is_some_and(|dir| path.starts_with(dir)) {
                "download"
            } else {
                "file"
            },
        );
        event.path = Some(crate::fsx::abbreviate(path));
        event.change = Some(match origin {
            Some(old) => {
                moved_from.push(old);
                format!(
                    "taşındı: {} → {}",
                    crate::fsx::abbreviate(old),
                    crate::fsx::abbreviate(path)
                )
            }
            None => "oluştu".to_string(),
        });
        events.push(event);
    }
    for path in gone.iter().filter(|path| !moved_from.contains(path)) {
        let mut event = RecordedEvent::new("file");
        event.path = Some(crate::fsx::abbreviate(path));
        event.change = Some("kaldırıldı ya da başka yere taşındı".into());
        events.push(event);
    }
    for (path, (size, modified)) in after {
        if let Some((old_size, old_modified)) = before.get(path) {
            if old_size != size || old_modified != modified {
                let mut event = RecordedEvent::new("file");
                event.path = Some(crate::fsx::abbreviate(path));
                event.change = Some("değişti".into());
                events.push(event);
            }
        }
    }
    events
}

fn clipboard_length() -> Option<usize> {
    arboard::Clipboard::new()
        .ok()?
        .get_text()
        .ok()
        .map(|text| text.chars().count())
}

fn watch(events: Arc<Mutex<Vec<RecordedEvent>>>, stop: Arc<AtomicBool>, started: DateTime<Utc>) {
    let push = |event: RecordedEvent| {
        let mut list = events.lock().expect("recorder events lock");
        if list.len() < MAX_EVENTS {
            list.push(event);
        }
    };
    let folders = watched_folders();
    let downloads = folders
        .first()
        .filter(|(_, name)| *name == "İndirilenler")
        .map(|(dir, _)| dir.clone());
    let mut files = snapshot(&folders);
    let mut last_front: Option<crate::activity::Front> = None;
    let mut last_clipboard = clipboard_length();
    let mut round: u32 = 0;
    while !stop.load(Ordering::SeqCst) && (Utc::now() - started).num_minutes() < MAX_MINUTES {
        if let Some(front) = crate::activity::front() {
            if !front.app.to_lowercase().contains("wiqo") {
                let private = front.is_private();
                let previous = last_front.as_ref();
                let app_changed = previous.is_none_or(|previous| previous.app != front.app);
                let window_changed =
                    previous.is_none_or(|previous| previous.window != front.window);
                let url_changed = front.url.is_some()
                    && previous.is_none_or(|previous| previous.url != front.url);
                if app_changed || window_changed || url_changed {
                    let mut event = RecordedEvent::new(if app_changed {
                        "app"
                    } else if url_changed {
                        "url"
                    } else {
                        "window"
                    });
                    event.app = Some(front.app.clone());
                    event.title = Some(if private {
                        "Gizli pencere (kaydedilmedi)".into()
                    } else {
                        front.window.clone()
                    });
                    if !private {
                        event.url = front.url.clone();
                    }
                    push(event);
                }
                last_front = Some(front);
            }
        }
        round += 1;
        if round.is_multiple_of(FILES_EVERY) {
            let now = snapshot(&folders);
            for event in file_events(&files, &now, downloads.as_ref()) {
                push(event);
            }
            files = now;
            let clipboard = clipboard_length();
            if clipboard.is_some() && clipboard != last_clipboard {
                let mut event = RecordedEvent::new("clipboard");
                event.change = Some(format!(
                    "metin kopyalandı ({} karakter)",
                    clipboard.unwrap_or(0)
                ));
                push(event);
                last_clipboard = clipboard;
            }
        }
        std::thread::sleep(SAMPLE_EVERY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_moves_between_folders_is_one_move_and_a_new_download_is_a_download() {
        let downloads = PathBuf::from("/u/Downloads");
        let time = SystemTime::UNIX_EPOCH;
        let before: Snapshot = [(downloads.join("fatura.pdf"), (10, time))]
            .into_iter()
            .collect();
        let after: Snapshot = [
            (PathBuf::from("/u/Documents/fatura.pdf"), (10, time)),
            (downloads.join("ekstre.pdf"), (5, time)),
        ]
        .into_iter()
        .collect();
        let events = file_events(&before, &after, Some(&downloads));
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|event| event.kind == "download"
            && event
                .path
                .as_deref()
                .is_some_and(|path| path.ends_with("ekstre.pdf"))));
        assert!(events.iter().any(|event| event
            .change
            .as_deref()
            .is_some_and(|change| change.starts_with("taşındı"))));
    }

    #[test]
    fn a_changed_file_is_one_change() {
        let file = PathBuf::from("/u/Desktop/rapor.xlsx");
        let before: Snapshot = [(file.clone(), (10, SystemTime::UNIX_EPOCH))]
            .into_iter()
            .collect();
        let after: Snapshot = [(file, (12, SystemTime::UNIX_EPOCH))].into_iter().collect();
        let events = file_events(&before, &after, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].change.as_deref(), Some("değişti"));
    }
}
