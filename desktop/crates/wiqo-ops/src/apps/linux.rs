//! Linux: applications are the `.desktop` files, running ones are processes whose program a desktop file launches.

use std::fs;
use std::path::{Path, PathBuf};

use sysinfo::System;
use wiqo_core::wire::WireError;

use super::{Installed, Running, Target};

fn desktop_folders() -> Vec<PathBuf> {
    let mut folders = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        PathBuf::from("/var/lib/snapd/desktop/applications"),
    ];
    if let Some(data) = dirs::data_dir() {
        folders.push(data.join("applications"));
    }
    folders
}

struct Entry {
    name: String,
    exec: String,
}

/// The `Name=` and `Exec=` of an application entry; `None` for entries a person does not open (hidden, not an application).
fn parse_desktop(text: &str) -> Option<Entry> {
    let (mut name, mut exec, mut kind, mut hidden) = (None, None, "Application".to_string(), false);
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(value) = line.strip_prefix("Name=") {
            name.get_or_insert_with(|| value.to_string());
        } else if let Some(value) = line.strip_prefix("Exec=") {
            exec.get_or_insert_with(|| value.to_string());
        } else if let Some(value) = line.strip_prefix("Type=") {
            kind = value.to_string();
        } else if line == "NoDisplay=true" || line == "Hidden=true" {
            hidden = true;
        }
    }
    (kind == "Application" && !hidden).then_some(Entry {
        name: name?,
        exec: exec?,
    })
}

/// The program an `Exec=` line starts: the first word that is neither `env` nor a `NAME=value` setting before it.
fn program_of(exec: &str) -> String {
    let first = exec
        .split_whitespace()
        .find(|part| {
            (!part.contains('=') || part.starts_with('/'))
                && Path::new(part).file_name().and_then(|name| name.to_str()) != Some("env")
        })
        .unwrap_or("");
    Path::new(first.trim_matches('"'))
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

pub(super) fn installed() -> Vec<Installed> {
    let mut found: Vec<Installed> = Vec::new();
    for folder in desktop_folders() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for item in entries.flatten() {
            let path = item.path();
            if path.extension().is_none_or(|ext| ext != "desktop") {
                continue;
            }
            let Some(entry) = fs::read_to_string(&path)
                .ok()
                .and_then(|text| parse_desktop(&text))
            else {
                continue;
            };
            let id = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
                .unwrap_or_default();
            if found
                .iter()
                .any(|app| app.bundle.as_deref() == Some(id.as_str()))
            {
                continue;
            }
            found.push(Installed {
                name: entry.name,
                file_name: program_of(&entry.exec),
                bundle: Some(id),
                path: path.to_string_lossy().to_string(),
                category: None,
            });
        }
    }
    found
}

fn active_pid() -> Option<u32> {
    let output = crate::process::command("xdotool")
        .args(["getactivewindow", "getwindowpid"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

pub(super) fn running() -> Vec<Running> {
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let active = active_pid();
    let mut groups: Vec<Running> = Vec::new();
    for app in installed() {
        let program = app.file_name.clone();
        if program.is_empty() {
            continue;
        }
        let pids: Vec<u32> = system
            .processes()
            .iter()
            .filter(|(_, process)| process.name().to_string_lossy() == program)
            .map(|(pid, _)| pid.as_u32())
            .collect();
        if pids.is_empty() {
            continue;
        }
        groups.push(Running {
            name: app.name,
            bundle: app.bundle,
            is_active: active.is_some_and(|pid| pids.contains(&pid)),
            pids,
        });
    }
    groups
}

pub(super) fn launch(target: &Target) -> Result<(), String> {
    let failed = || format!("{} açılamadı.", target.name);
    if let Some(id) = &target.bundle {
        for launcher in [
            ("gtk-launch", vec![id.clone()]),
            (
                "gio",
                vec!["launch".into(), target.path.clone().unwrap_or_default()],
            ),
        ] {
            if crate::process::command(launcher.0)
                .args(&launcher.1)
                .status()
                .is_ok_and(|status| status.success())
            {
                return Ok(());
            }
        }
    }
    Err(failed())
}

fn signal(app: &Running, force: bool) -> bool {
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let mut sent = false;
    for pid in &app.pids {
        if let Some(process) = system.process(sysinfo::Pid::from_u32(*pid)) {
            sent |= if force {
                process.kill()
            } else {
                process.kill_with(sysinfo::Signal::Term).unwrap_or(false)
            };
        }
    }
    sent
}

pub(super) fn activate(app: &Running) -> bool {
    let Some(pid) = app.pids.first() else {
        return false;
    };
    crate::process::command("xdotool")
        .args(["search", "--pid", &pid.to_string(), "windowactivate"])
        .status()
        .is_ok_and(|status| status.success())
}

pub(super) fn terminate(app: &Running) -> bool {
    signal(app, false)
}

pub(super) fn force_terminate(app: &Running) -> bool {
    signal(app, true)
}

pub(super) fn protected(_target: &Target) -> Option<WireError> {
    None
}

pub(super) fn open_blank_tab(target: &Target) -> Result<(), String> {
    Err(format!(
        "{} için yeni sekme isteği bu sistemde desteklenmiyor.",
        target.name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_desktop_entry_gives_its_name_and_program_and_hidden_ones_are_skipped() {
        let entry = parse_desktop("[Desktop Entry]\nType=Application\nName=Firefox\nName[tr]=Firefox Tarayıcı\nExec=/usr/lib/firefox/firefox %u\n").unwrap();
        assert_eq!(entry.name, "Firefox");
        assert_eq!(program_of(&entry.exec), "firefox");
        assert!(parse_desktop(
            "[Desktop Entry]\nType=Application\nName=Gizli\nExec=gizli\nNoDisplay=true\n"
        )
        .is_none());
        assert_eq!(program_of("env GTK_THEME=x gimp-2.10 %F"), "gimp-2.10");
    }
}
