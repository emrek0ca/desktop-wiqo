//! A task grant: the phone allows the server's agent to work here for a few minutes, inside named folders, on file work only. The permanent "Tam yetki"
//! switch allows everything; a grant allows only this. When either changes, the desktop registers again so the server knows at once.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Notify;

const MAX_MINUTES: u64 = 30;
const MAX_FOLDERS: usize = 4;

/// What may run under a task grant: files and documents; never a delete, never applications, the terminal or the screen.
const TASK_TYPES: [&str; 15] = [
    "ping",
    "getSystemStatus",
    "listDesktopFiles",
    "readDesktopFile",
    "listFiles",
    "searchFiles",
    "readFile",
    "sendFileToPhone",
    "moveFile",
    "copyFile",
    "convertFile",
    "runFilePlan",
    "writeFile",
    "readDocument",
    "fileUtility",
];

#[derive(Debug, Clone)]
pub struct Grant {
    pub folders: Vec<String>,
    pub label: String,
    expires: Instant,
}

impl Grant {
    pub fn seconds_left(&self) -> u64 {
        self.expires
            .saturating_duration_since(Instant::now())
            .as_secs()
    }
}

/// The current grant and the signal that something the server must hear about changed.
#[derive(Default)]
pub struct Access {
    grant: Mutex<Option<Grant>>,
    changed: Arc<Notify>,
}

impl Access {
    /// Notified when the grant (or another registration-visible setting) changes.
    pub fn changed(&self) -> Arc<Notify> {
        self.changed.clone()
    }

    pub fn announce(&self) {
        self.changed.notify_one();
    }

    /// Starts a grant of `minutes` for these folders; 0 minutes withdraws it.
    pub fn grant(self: &Arc<Self>, minutes: u64, folders: Vec<String>, label: String) {
        let minutes = minutes.min(MAX_MINUTES);
        let next = (minutes > 0).then(|| Grant {
            folders: folders.into_iter().take(MAX_FOLDERS).collect(),
            label,
            expires: Instant::now() + Duration::from_secs(minutes * 60),
        });
        *self.grant.lock().expect("grant lock") = next;
        self.announce();
        if minutes > 0 {
            let access = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(minutes * 60) + Duration::from_millis(200))
                    .await;
                if access.active().is_none() {
                    access.announce();
                }
            });
        }
    }

    pub fn active(&self) -> Option<Grant> {
        let mut grant = self.grant.lock().expect("grant lock");
        if grant
            .as_ref()
            .is_some_and(|current| current.expires <= Instant::now())
        {
            *grant = None;
        }
        grant.clone()
    }

    /// Why a command of the server's agent may not run under the grant, or `None` when it may.
    pub fn denial(&self, kind: &str, payload: &Value) -> Option<String> {
        let Some(grant) = self.active() else {
            return Some("PC görev izni bitti; telefondan yeniden onayla.".into());
        };
        if !TASK_TYPES.contains(&kind) {
            return Some("Bu işlem görev izninin kapsamında değil.".into());
        }
        let roots: Vec<String> = grant
            .folders
            .iter()
            .map(|folder| absolute(folder))
            .collect();
        let inside = |raw: &str| {
            let path = absolute(raw);
            roots
                .iter()
                .any(|root| path == *root || path.starts_with(&format!("{root}/")))
        };
        let home = home();
        let own_folders_covered = ["Desktop", "Documents", "Downloads"].iter().all(|own| {
            let own = format!("{home}/{own}");
            roots
                .iter()
                .any(|root| own == *root || own.starts_with(&format!("{root}/")))
        });
        let value = &payload["value"];
        let outside = match payload["kind"].as_str().unwrap_or("") {
            "fileOperation" => {
                !inside(value["path"].as_str().unwrap_or(""))
                    || value["destination"]
                        .as_str()
                        .is_some_and(|destination| !inside(destination))
            }
            "fileSearch" => match value["folder"].as_str() {
                Some(folder) => !inside(folder),
                None => !own_folders_covered,
            },
            "fileName" => !inside("~/Desktop"),
            // Reversible chores only (the phone's `isTaskWritable` list), every path inside the grant; restoring from the Trash names no path.
            "fileUtility" => {
                const REVERSIBLE: [&str; 16] = [
                    "createFolder",
                    "archive",
                    "extract",
                    "mergePDF",
                    "splitPDF",
                    "compressImage",
                    "organize",
                    "batchRename",
                    "moveDocuments",
                    "setTags",
                    "restoreFromTrash",
                    "openWith",
                    "reveal",
                    "inspect",
                    "diskUsage",
                    "findDuplicates",
                ];
                let action = value["action"].as_str().unwrap_or("");
                let mut paths: Vec<&str> = [&value["path"], &value["destination"]]
                    .into_iter()
                    .filter_map(Value::as_str)
                    .collect();
                paths.extend(
                    value["paths"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                );
                !REVERSIBLE.contains(&action)
                    || paths.iter().any(|path| !inside(path))
                    || (paths.is_empty() && action != "restoreFromTrash")
            }
            "filePlan" => value["steps"].as_array().is_some_and(|steps| {
                steps.iter().any(|step| {
                    step["path"]
                        .as_str()
                        .is_some_and(|path| !path.starts_with('$') && !inside(path))
                        || step["destination"]
                            .as_str()
                            .is_some_and(|destination| !inside(destination))
                        || match step["folder"].as_str() {
                            Some(folder) => !inside(folder),
                            None => step["op"] == "search" && !own_folders_covered,
                        }
                })
            }),
            _ => false,
        };
        outside.then(|| "Bu yol görev izninin klasörlerinin dışında.".to_string())
    }
}

/// Reads the server's agent may do without a phone in the loop (the "Okuma adımlarını telefonsuz çalıştır" setting): looking, never
/// changing, and only inside the person's own folders (Desktop, Documents, Downloads) with no sensitive name. The phone auto-approves the
/// same reads; this only spares the round trip.
pub fn read_without_phone(kind: &str, payload: &Value) -> bool {
    const ANYWHERE: [&str; 3] = ["ping", "getSystemStatus", "listRunningApplications"];
    const FILE_READS: [&str; 6] = [
        "listDesktopFiles",
        "readDesktopFile",
        "listFiles",
        "searchFiles",
        "readFile",
        "readDocument",
    ];
    if ANYWHERE.contains(&kind) {
        return true;
    }
    if !FILE_READS.contains(&kind) {
        return false;
    }
    let value = &payload["value"];
    let own = |raw: &str| {
        if wiqo_contract::FilePolicy::get().is_sensitive(raw) {
            return false;
        }
        let path = absolute(raw);
        let home = absolute(&home());
        // Windows paths are case-insensitive ("c:/users/Ada" is the same folder).
        let (path, home) = if cfg!(windows) {
            (path.to_lowercase(), home.to_lowercase())
        } else {
            (path, home)
        };
        let Some(rest) = path.strip_prefix(&format!("{home}/")) else {
            return false;
        };
        let first = rest.split('/').next().unwrap_or("");
        [
            "Desktop",
            "Documents",
            "Downloads",
            "Masaüstü",
            "Belgeler",
            "İndirilenler",
        ]
        .iter()
        .any(|name| {
            if cfg!(windows) {
                name.to_lowercase() == first
            } else {
                *name == first
            }
        })
    };
    match payload["kind"].as_str().unwrap_or("") {
        "fileOperation" => value["path"].as_str().is_some_and(own),
        // A search names a folder, or searches the own folders by default.
        "fileSearch" => value["everywhere"] != true && value["folder"].as_str().is_none_or(own),
        // readDocument and the Desktop file reads name a file on the Desktop.
        "fileName" | "none" => true,
        _ => false,
    }
}

/// The home folder with forward slashes ("C:/Users/ada" on Windows), the form every path here is compared in.
fn home() -> String {
    dirs::home_dir()
        .map(|home| home.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| "/".into())
}

/// `raw` as an absolute path with forward slashes and `.`/`..` resolved; a Windows drive ("C:") is kept at the front.
fn absolute(raw: &str) -> String {
    let home = home();
    let raw = raw.replace('\\', "/");
    let expanded = if raw == "~" {
        home
    } else if let Some(rest) = raw.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        raw
    };
    let (drive, rest) = match expanded.as_bytes() {
        [letter, b':', ..] if letter.is_ascii_alphabetic() => expanded.split_at(2),
        _ => ("", expanded.as_str()),
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("{drive}/{}", parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn windows_paths_are_compared_with_forward_slashes() {
        assert_eq!(
            absolute("C:\\Users\\ada\\Desktop\\a.txt"),
            "C:/Users/ada/Desktop/a.txt"
        );
        assert_eq!(
            absolute("C:\\Users\\ada\\Desktop\\..\\.ssh"),
            "C:/Users/ada/.ssh"
        );
        assert_eq!(absolute("/Users/ada/./Desktop"), "/Users/ada/Desktop");
    }

    #[test]
    fn reads_without_a_phone_stay_in_the_own_folders() {
        let file = |path: &str| json!({ "kind": "fileOperation", "value": { "path": path } });
        assert!(read_without_phone("readFile", &file("~/Desktop/rapor.pdf")));
        assert!(read_without_phone("listFiles", &file("~/Belgeler")));
        assert!(!read_without_phone("readFile", &file("~/.ssh/id_rsa")));
        assert!(!read_without_phone("readFile", &file("/etc/hosts")));
        assert!(!read_without_phone(
            "readFile",
            &file("~/Desktop/../.ssh/id_rsa")
        ));
        assert!(!read_without_phone("deleteFile", &file("~/Desktop/x.txt")));
        assert!(read_without_phone(
            "searchFiles",
            &json!({ "kind": "fileSearch", "value": { "query": "fatura" } })
        ));
        assert!(!read_without_phone(
            "searchFiles",
            &json!({ "kind": "fileSearch", "value": { "query": "x", "everywhere": true } })
        ));
        assert!(read_without_phone(
            "getSystemStatus",
            &json!({ "kind": "none" })
        ));
    }

    #[tokio::test]
    async fn a_grant_allows_file_work_inside_its_folders_only() {
        let access = Arc::new(Access::default());
        assert!(
            access
                .denial(
                    "readFile",
                    &json!({ "kind": "fileOperation", "value": { "path": "~/Desktop/a.txt" } })
                )
                .is_some(),
            "no grant, no agent"
        );
        access.grant(10, vec!["~/Desktop".into()], "Rapor".into());
        assert!(access
            .denial(
                "readFile",
                &json!({ "kind": "fileOperation", "value": { "path": "~/Desktop/a.txt" } })
            )
            .is_none());
        assert!(access.denial("moveFile", &json!({ "kind": "fileOperation", "value": { "path": "~/Desktop/a.txt", "destination": "~/Documents/a.txt" } })).is_some());
        assert!(access
            .denial(
                "readFile",
                &json!({ "kind": "fileOperation", "value": { "path": "~/Desktop/../.ssh/id_rsa" } })
            )
            .is_some());
        assert!(
            access
                .denial(
                    "deleteFile",
                    &json!({ "kind": "fileOperation", "value": { "path": "~/Desktop/a.txt" } })
                )
                .is_some(),
            "never a delete"
        );
        assert!(
            access
                .denial(
                    "searchFiles",
                    &json!({ "kind": "fileSearch", "value": { "query": "x" } })
                )
                .is_some(),
            "a search of all own folders needs all of them granted"
        );
        assert!(access
            .denial(
                "searchFiles",
                &json!({ "kind": "fileSearch", "value": { "query": "x", "folder": "~/Desktop" } })
            )
            .is_none());
        access.grant(0, vec![], String::new());
        assert!(access.active().is_none());
    }
}
