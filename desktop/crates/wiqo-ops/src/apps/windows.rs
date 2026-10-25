//! Windows: the installed applications are what the Start menu shows (`Get-StartApps`: desktop programs and Store apps alike, with the
//! programs' .exe from their Start-menu shortcuts), the visible top-level windows show which run, a desktop program starts from its .exe and a Store
//! app from `shell:AppsFolder`, `taskkill` closes. Each application carries its program's file name ("chrome", "spotify") as its identity, so the
//! running process and the installed entry are the same application.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use wiqo_core::wire::WireError;

use super::{Installed, Running, Target};

fn start_menu_folders() -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Some(common) = std::env::var_os("ProgramData") {
        folders.push(PathBuf::from(common).join("Microsoft/Windows/Start Menu/Programs"));
    }
    if let Some(user) = std::env::var_os("APPDATA") {
        folders.push(PathBuf::from(user).join("Microsoft/Windows/Start Menu/Programs"));
    }
    folders
}

fn skipped(name: &str) -> bool {
    let lowered = name.to_lowercase();
    name.is_empty() || lowered.contains("uninstall") || lowered.contains("kaldır")
}

/// The identity of a program: its .exe name without the extension, lower case ("chrome").
fn identity_of(exe: &str) -> Option<String> {
    let path = Path::new(exe.trim().trim_matches('"'));
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        .then(|| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().to_lowercase())
        })
        .flatten()
}

/// A Store app has no .exe to name it; its name without spaces stands in ("WhatsApp" -> "whatsapp"), which is also how its process is usually called.
fn identity_of_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

/// The Start-menu shortcuts alone: available at once, used until the full list is read.
fn scan(folder: &Path, depth: usize, out: &mut Vec<Installed>) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth > 0 {
            scan(&path, depth - 1, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
        {
            let name = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
                .unwrap_or_default();
            if !skipped(&name) {
                out.push(Installed {
                    name: name.clone(),
                    file_name: name.clone(),
                    bundle: Some(identity_of_name(&name)),
                    path: path.to_string_lossy().to_string(),
                    category: None,
                });
            }
        }
    }
}

fn shortcuts() -> Vec<Installed> {
    let mut found = Vec::new();
    for folder in start_menu_folders() {
        scan(&folder, 3, &mut found);
    }
    found
}

#[derive(Deserialize)]
struct StartApp {
    /// Name as the Start menu shows it.
    n: String,
    /// AppUserModelID or the program path (`Get-StartApps`).
    id: String,
    /// The .exe its Start-menu shortcut points at, when there is one.
    #[serde(default)]
    t: Option<String>,
}

const LIST_SCRIPT: &str = r#"[Console]::OutputEncoding=[Text.Encoding]::UTF8
$shell = New-Object -ComObject WScript.Shell
$links = @{}
foreach ($root in @("$env:ProgramData\Microsoft\Windows\Start Menu\Programs", "$env:APPDATA\Microsoft\Windows\Start Menu\Programs")) {
  Get-ChildItem -LiteralPath $root -Filter *.lnk -Recurse -ErrorAction SilentlyContinue | ForEach-Object {
    try { $target = $shell.CreateShortcut($_.FullName).TargetPath; if ($target) { $links[$_.BaseName] = $target } } catch {}
  }
}
@(Get-StartApps | ForEach-Object { [pscustomobject]@{ n = $_.Name; id = $_.AppID; t = $links[$_.Name] } }) | ConvertTo-Json -Compress"#;

fn read_start_apps() -> Option<Vec<Installed>> {
    let mut command = crate::process::command("powershell");
    command.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        LIST_SCRIPT,
    ]);
    let output = crate::process::run_with_timeout(command, Duration::from_secs(25))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim().trim_start_matches('\u{feff}');
    let apps: Vec<StartApp> = match serde_json::from_str::<serde_json::Value>(text).ok()? {
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|item| serde_json::from_value(item).ok())
            .collect(),
        single => vec![serde_json::from_value(single).ok()?],
    };
    let mut seen = std::collections::HashSet::new();
    let mut found = Vec::new();
    for app in apps {
        if skipped(&app.n) || !seen.insert(app.n.to_lowercase()) {
            continue;
        }
        // A desktop program starts from its .exe; a Store app (or one without a usable shortcut) through the shell's application folder.
        let exe = app
            .t
            .as_deref()
            .filter(|target| identity_of(target).is_some() && Path::new(target).exists())
            .map(str::to_string)
            .or_else(|| {
                Some(app.id.clone())
                    .filter(|id| identity_of(id).is_some() && Path::new(id).exists())
            });
        let bundle = exe
            .as_deref()
            .and_then(identity_of)
            .or_else(|| identity_of(&app.id))
            .unwrap_or_else(|| identity_of_name(&app.n));
        let file_name = exe
            .as_deref()
            .and_then(|exe| {
                Path::new(exe)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| app.n.clone());
        found.push(Installed {
            name: app.n.clone(),
            file_name,
            bundle: Some(bundle),
            path: exe.unwrap_or_else(|| format!("shell:AppsFolder\\{}", app.id)),
            category: None,
        });
    }
    (!found.is_empty()).then_some(found)
}

struct Cache {
    apps: Option<Vec<Installed>>,
    read_at: Option<Instant>,
    reading: bool,
}

fn cache() -> &'static (Mutex<Cache>, Condvar) {
    static CACHE: OnceLock<(Mutex<Cache>, Condvar)> = OnceLock::new();
    CACHE.get_or_init(|| {
        (
            Mutex::new(Cache {
                apps: None,
                read_at: None,
                reading: false,
            }),
            Condvar::new(),
        )
    })
}

/// Reads the full list in the background (a few seconds of PowerShell), at most every ten minutes.
pub(crate) fn warm() {
    let (lock, ready) = cache();
    {
        let mut state = lock.lock().expect("apps cache");
        let fresh = state
            .read_at
            .is_some_and(|at| at.elapsed() < Duration::from_secs(600));
        if state.reading || fresh {
            return;
        }
        state.reading = true;
    }
    std::thread::spawn(move || {
        let apps = read_start_apps();
        let mut state = lock.lock().expect("apps cache");
        if apps.is_some() {
            state.apps = apps;
        }
        state.read_at = Some(Instant::now());
        state.reading = false;
        ready.notify_all();
    });
}

/// What is installed, without waiting: the full list once read, the Start-menu shortcuts before that.
pub(super) fn installed_now() -> Vec<Installed> {
    warm();
    cache()
        .0
        .lock()
        .expect("apps cache")
        .apps
        .clone()
        .unwrap_or_else(shortcuts)
}

/// What is installed for opening or closing one by name: waits (at most ten seconds) for the full list the first time, so a Store app is found.
pub(super) fn installed() -> Vec<Installed> {
    warm();
    let (lock, ready) = cache();
    let state = lock.lock().expect("apps cache");
    let (state, _) = ready
        .wait_timeout_while(state, Duration::from_secs(10), |state| {
            state.apps.is_none() && state.reading
        })
        .expect("apps cache");
    state.apps.clone().unwrap_or_else(shortcuts)
}

/// Program identity -> the name the Start menu shows ("chrome" -> "Google Chrome"), for naming running processes.
fn display_names() -> HashMap<String, String> {
    cache()
        .0
        .lock()
        .expect("apps cache")
        .apps
        .iter()
        .flatten()
        .filter_map(|app| Some((app.bundle.clone()?, app.name.clone())))
        .collect()
}

/// Processes with a visible, titled top-level window, read straight from the window list (EnumWindows): milliseconds. `tasklist /v` asked every
/// window for its title and owner and could take a minute or hang on a window that does not answer, which held the registration and every
/// chat turn ("Bağlanıyor…", "Hazırlanıyor…").
pub(super) fn running() -> Vec<Running> {
    use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetForegroundWindow, GetWindow, GetWindowTextLengthW,
        GetWindowThreadProcessId, IsWindowVisible, GW_OWNER,
    };

    unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> BOOL {
        let pids = &mut *(data as *mut Vec<u32>);
        if IsWindowVisible(window) != 0
            && GetWindow(window, GW_OWNER).is_null()
            && GetWindowTextLengthW(window) > 0
        {
            // The desktop and the taskbar are explorer.exe's windows too; only a folder window should make explorer "running".
            let mut class = [0u16; 64];
            let length = GetClassNameW(window, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
            let class = String::from_utf16_lossy(&class[..length]);
            if ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"].contains(&class.as_str()) {
                return 1;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(window, &mut pid);
            if pid != 0 && !pids.contains(&pid) {
                pids.push(pid);
            }
        }
        1
    }

    fn image_name(pid: u32) -> Option<String> {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let mut buffer = [0u16; 1024];
            let mut size = buffer.len() as u32;
            let ok = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                buffer.as_mut_ptr(),
                &mut size,
            );
            CloseHandle(handle);
            if ok == 0 {
                return None;
            }
            let path = String::from_utf16_lossy(&buffer[..size as usize]);
            Path::new(&path)
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
        }
    }

    let mut pids: Vec<u32> = Vec::new();
    unsafe {
        EnumWindows(Some(visit), &mut pids as *mut Vec<u32> as LPARAM);
    }
    let own = std::process::id();
    let names = display_names();
    let foreground_pid = unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(GetForegroundWindow(), &mut pid);
        pid
    };
    let mut groups: Vec<Running> = Vec::new();
    for pid in pids.into_iter().filter(|pid| *pid != own) {
        let Some(name) = image_name(pid) else {
            continue;
        };
        // The desktop shell and system hosts are not applications the person runs.
        if [
            "ApplicationFrameHost",
            "TextInputHost",
            "SystemSettings",
            "ShellExperienceHost",
            "SearchHost",
            "StartMenuExperienceHost",
        ]
        .iter()
        .any(|system| system.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        let identity = name.to_lowercase();
        match groups
            .iter_mut()
            .find(|group| group.bundle.as_deref() == Some(identity.as_str()))
        {
            Some(group) => {
                group.is_active |= pid == foreground_pid;
                group.pids.push(pid);
            }
            None => groups.push(Running {
                // "Google Chrome", not "chrome", when the Start menu knows it.
                name: names.get(&identity).cloned().unwrap_or(name),
                bundle: Some(identity),
                is_active: pid == foreground_pid,
                pids: vec![pid],
            }),
        }
    }
    groups
}

pub(super) fn launch(target: &Target) -> Result<(), String> {
    let path = target
        .path
        .as_ref()
        .ok_or_else(|| format!("{} açılamadı.", target.name))?;
    // A Store app opens through the shell's application folder; explorer.exe reports 1 even when it worked, so only a failure to start counts.
    if path.starts_with("shell:AppsFolder") {
        return crate::process::command("explorer.exe")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|_| format!("{} açılamadı.", target.name));
    }
    match crate::process::command("cmd")
        .args(["/C", "start", "", path])
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        _ => Err(format!("{} açılamadı.", target.name)),
    }
}

fn kill(app: &Running, force: bool) -> bool {
    let mut done = false;
    for pid in &app.pids {
        let mut command = crate::process::command("taskkill");
        command.args(["/PID", &pid.to_string()]);
        if force {
            command.arg("/F");
        }
        done |= command.status().is_ok_and(|status| status.success());
    }
    done
}

/// Brings the application's first visible window to the front; Windows may refuse a background process, which the caller's check then sees.
pub(super) fn activate(app: &Running) -> bool {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic,
        IsWindowVisible, SetForegroundWindow, ShowWindow, GW_OWNER, SW_RESTORE,
    };

    struct Search {
        pids: Vec<u32>,
        found: bool,
    }

    unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> BOOL {
        let search = &mut *(data as *mut Search);
        if IsWindowVisible(window) == 0
            || !GetWindow(window, GW_OWNER).is_null()
            || GetWindowTextLengthW(window) == 0
        {
            return 1;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        if !search.pids.contains(&pid) {
            return 1;
        }
        if IsIconic(window) != 0 {
            ShowWindow(window, SW_RESTORE);
        }
        search.found = SetForegroundWindow(window) != 0;
        0
    }

    let mut search = Search {
        pids: app.pids.clone(),
        found: false,
    };
    unsafe {
        EnumWindows(Some(visit), &mut search as *mut Search as LPARAM);
    }
    search.found
}

pub(super) fn terminate(app: &Running) -> bool {
    kill(app, false)
}

pub(super) fn force_terminate(app: &Running) -> bool {
    kill(app, true)
}

pub(super) fn protected(target: &Target) -> Option<WireError> {
    target.name.eq_ignore_ascii_case("explorer").then(|| {
        crate::op::permission(
            "APPLICATION_SYSTEM_PROTECTED",
            "Dosya Gezgini sistemin parçasıdır; uygulama gibi kapatılamaz.",
        )
    })
}

pub(super) fn open_blank_tab(target: &Target) -> Result<(), String> {
    Err(format!(
        "{} için yeni sekme isteği bu sistemde desteklenmiyor.",
        target.name
    ))
}
