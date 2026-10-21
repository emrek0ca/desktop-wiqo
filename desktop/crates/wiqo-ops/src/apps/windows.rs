//! Windows: Start Menu shortcuts are the installed applications, the visible top-level windows show which run, `start` launches, `taskkill` closes.

use std::fs;
use std::path::{Path, PathBuf};

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
            if !name.is_empty()
                && !name.to_lowercase().contains("uninstall")
                && !name.to_lowercase().contains("kaldır")
            {
                out.push(Installed {
                    name: name.clone(),
                    file_name: name,
                    bundle: None,
                    path: path.to_string_lossy().to_string(),
                    category: None,
                });
            }
        }
    }
}

pub(super) fn installed() -> Vec<Installed> {
    let mut found = Vec::new();
    for folder in start_menu_folders() {
        scan(&folder, 3, &mut found);
    }
    found
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
        EnumWindows, GetWindow, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible,
        GW_OWNER,
    };

    unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> BOOL {
        let pids = &mut *(data as *mut Vec<u32>);
        if IsWindowVisible(window) != 0
            && GetWindow(window, GW_OWNER).is_null()
            && GetWindowTextLengthW(window) > 0
        {
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
    let mut groups: Vec<Running> = Vec::new();
    for pid in pids.into_iter().filter(|pid| *pid != own) {
        let Some(name) = image_name(pid) else {
            continue;
        };
        // The desktop shell and system hosts are not applications the person runs.
        if [
            "explorer",
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
        match groups
            .iter_mut()
            .find(|group| group.name.eq_ignore_ascii_case(&name))
        {
            Some(group) => group.pids.push(pid),
            None => groups.push(Running {
                name,
                bundle: None,
                is_active: false,
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

pub(super) fn activate(_app: &Running) -> bool {
    false
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
