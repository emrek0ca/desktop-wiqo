//! The folders on this computer that a cloud drive keeps (OneDrive, Google Drive, Dropbox). The server keeps the list with the device, so while the PC is
//! off a request about a file in one of them is done on the cloud copy (the person's connected account) and reaches the PC through that drive's own sync.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

struct Found(Vec<(&'static str, &'static str, PathBuf)>);

impl Found {
    fn add(&mut self, provider: &'static str, account: &'static str, path: PathBuf) {
        if path.is_dir() && !self.0.iter().any(|(_, _, known)| known == &path) {
            self.0.push((provider, account, path));
        }
    }
}

/// `[{ provider: "onedrive" | "googledrive" | "dropbox", account: "personal" | "business", local: "<absolute path>" }]`.
pub fn cloud_folders() -> Value {
    let mut found = Found(Vec::new());
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    // Windows: OneDrive sets these for the signed-in user (OneDrive = the first account).
    for (variable, account) in [
        ("OneDriveConsumer", "personal"),
        ("OneDriveCommercial", "business"),
        ("OneDrive", "personal"),
    ] {
        if let Some(value) = std::env::var_os(variable) {
            found.add("onedrive", account, PathBuf::from(value));
        }
    }
    // Windows: Google Drive for desktop mounts a drive letter with "My Drive" in it.
    if cfg!(windows) {
        for letter in 'D'..='Z' {
            found.add(
                "googledrive",
                "personal",
                PathBuf::from(format!("{letter}:\\My Drive")),
            );
        }
    }
    // macOS: the File Provider drives live under ~/Library/CloudStorage (OneDrive-<account>, GoogleDrive-<email>, Dropbox[-<team>]).
    if let Some(home) = &home {
        if let Ok(entries) = std::fs::read_dir(home.join("Library").join("CloudStorage")) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(rest) = name.strip_prefix("OneDrive") {
                    found.add(
                        "onedrive",
                        if rest.is_empty() || rest.contains("Personal") {
                            "personal"
                        } else {
                            "business"
                        },
                        entry.path(),
                    );
                } else if name.starts_with("GoogleDrive") {
                    // The account folder holds "My Drive" (its name follows the system language) and "Shared drives": My Drive is the folder that is not the shared ones.
                    if let Ok(inner) = std::fs::read_dir(entry.path()) {
                        for drive in inner.flatten().filter(|drive| drive.path().is_dir()) {
                            let label = drive.file_name().to_string_lossy().to_lowercase();
                            if !label.contains("shared")
                                && !label.contains("ortak")
                                && !label.starts_with('.')
                            {
                                found.add("googledrive", "personal", drive.path());
                            }
                        }
                    }
                } else if let Some(rest) = name.strip_prefix("Dropbox") {
                    found.add(
                        "dropbox",
                        if rest.is_empty() {
                            "personal"
                        } else {
                            "business"
                        },
                        entry.path(),
                    );
                }
            }
        }
        // Dropbox says where it keeps its folders in info.json (macOS/Linux: ~/.dropbox, Windows: %APPDATA% or %LOCALAPPDATA%).
        let candidates = [
            Some(home.join(".dropbox").join("info.json")),
            std::env::var_os("APPDATA")
                .map(|dir| Path::new(&dir).join("Dropbox").join("info.json")),
            std::env::var_os("LOCALAPPDATA")
                .map(|dir| Path::new(&dir).join("Dropbox").join("info.json")),
        ];
        for file in candidates.into_iter().flatten() {
            if let Ok(text) = std::fs::read_to_string(file) {
                if let Ok(info) = serde_json::from_str::<Value>(&text) {
                    for account in ["personal", "business"] {
                        if let Some(path) = info[account]["path"].as_str() {
                            found.add("dropbox", account, PathBuf::from(path));
                        }
                    }
                }
            }
        }
    }
    Value::Array(
        found
            .0
            .into_iter()
            .take(10)
            .map(|(provider, account, path)| json!({ "provider": provider, "account": account, "local": path.to_string_lossy() }))
            .collect(),
    )
}
