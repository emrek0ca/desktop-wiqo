//! macOS: NSWorkspace and NSRunningApplication for what is running, LaunchServices (`open`) to start something, bundles on disk for what is installed.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use objc2_app_kit::{
    NSApplicationActivationOptions, NSApplicationActivationPolicy, NSRunningApplication,
    NSWorkspace,
};
use wiqo_core::wire::WireError;

use super::{Installed, Running, Target};
use crate::op::permission;

const FINDER: &str = "com.apple.finder";

fn scan_folder(folder: &Path, depth: usize, out: &mut Vec<Installed>) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if name.ends_with(".app") {
            if let Some(app) = read_bundle(&path) {
                out.push(app);
            }
        } else if depth > 0 && path.is_dir() {
            scan_folder(&path, depth - 1, out);
        }
    }
}

fn read_bundle(path: &Path) -> Option<Installed> {
    let file_name = path.file_stem()?.to_string_lossy().to_string();
    let info = plist::Value::from_file(path.join("Contents/Info.plist"))
        .ok()?
        .into_dictionary()?;
    let text = |key: &str| {
        info.get(key)
            .and_then(|value| value.as_string())
            .map(str::to_string)
    };
    // A background-only bundle (helpers, agents) is not something a person opens.
    if info
        .get("LSBackgroundOnly")
        .and_then(|value| value.as_boolean())
        == Some(true)
        || info.get("LSUIElement").and_then(|value| value.as_boolean()) == Some(true)
    {
        return None;
    }
    let name = text("CFBundleDisplayName")
        .or_else(|| text("CFBundleName"))
        .unwrap_or_else(|| file_name.clone());
    let category =
        text("LSApplicationCategoryType").map(|raw| raw.replace("public.app-category.", ""));
    Some(Installed {
        name,
        file_name,
        bundle: text("CFBundleIdentifier"),
        path: path.to_string_lossy().to_string(),
        category,
    })
}

/// Applications on disk, remembered for 20 seconds (scanning every bundle on each command would be slow and is never needed that often).
/// The same list without waiting (nothing here is slow to read).
pub(super) fn installed_now() -> Vec<Installed> {
    installed()
}

pub(super) fn installed() -> Vec<Installed> {
    static CACHE: Mutex<Option<(Instant, Vec<Installed>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().expect("installed cache");
    if let Some((at, apps)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(20) {
            return apps.clone();
        }
    }
    let home = crate::fsx::home();
    let mut found = Vec::new();
    for folder in [
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
        home.join("Applications"),
    ] {
        scan_folder(&folder, 2, &mut found);
    }
    // A bundle identifier belongs to one application even when it sits in two folders.
    let mut seen: HashMap<String, ()> = HashMap::new();
    found.retain(|app| {
        app.bundle
            .as_ref()
            .is_none_or(|bundle| seen.insert(bundle.to_lowercase(), ()).is_none())
    });
    *cache = Some((Instant::now(), found.clone()));
    found
}

fn visible_applications() -> Vec<objc2::rc::Retained<NSRunningApplication>> {
    let workspace = NSWorkspace::sharedWorkspace();
    workspace
        .runningApplications()
        .iter()
        .filter(|app| {
            // The list refreshes on the main run loop; a process that is already gone can still be in it for a moment, so ask the kernel too.
            let alive = unsafe { libc::kill(app.processIdentifier(), 0) } == 0;
            alive
                && !app.isTerminated()
                && (app.activationPolicy() == NSApplicationActivationPolicy::Regular
                    || app
                        .bundleIdentifier()
                        .is_some_and(|bundle| bundle.to_string() == FINDER))
        })
        .collect()
}

pub(super) fn running() -> Vec<Running> {
    let mut groups: Vec<Running> = Vec::new();
    for app in visible_applications() {
        let bundle = app.bundleIdentifier().map(|bundle| bundle.to_string());
        let name = app
            .localizedName()
            .map(|name| name.to_string())
            .or_else(|| bundle.clone())
            .unwrap_or_else(|| "Uygulama".into());
        let pid = app.processIdentifier() as u32;
        let active = app.isActive();
        match groups
            .iter_mut()
            .find(|group| bundle.is_some() && group.bundle == bundle)
        {
            Some(group) => {
                group.pids.push(pid);
                group.is_active |= active;
            }
            None => groups.push(Running {
                name,
                bundle,
                is_active: active,
                pids: vec![pid],
            }),
        }
    }
    groups
}

fn instances(app: &Running) -> Vec<objc2::rc::Retained<NSRunningApplication>> {
    visible_applications()
        .into_iter()
        .filter(|candidate| app.pids.contains(&(candidate.processIdentifier() as u32)))
        .collect()
}

pub(super) fn launch(target: &Target) -> Result<(), String> {
    let status = match (&target.path, &target.bundle) {
        (Some(path), _) => crate::process::command("/usr/bin/open").arg(path).status(),
        (None, Some(bundle)) => crate::process::command("/usr/bin/open")
            .args(["-b", bundle])
            .status(),
        (None, None) => return Err(format!("{} açılamadı.", target.name)),
    };
    match status {
        Ok(status) if status.success() => Ok(()),
        _ => Err(format!("{} açılamadı.", target.name)),
    }
}

/// The process in front, read live from Launch Services (`lsappinfo`): AppKit's own answer goes stale in a process without a running event loop.
pub(super) fn front_pid() -> Option<u32> {
    let front = crate::process::command("/usr/bin/lsappinfo")
        .arg("front")
        .output()
        .ok()?;
    let asn = String::from_utf8_lossy(&front.stdout).trim().to_string();
    let info = crate::process::command("/usr/bin/lsappinfo")
        .args(["info", "-only", "pid", &asn])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&info.stdout).to_string();
    text.split("pid =")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Brings an app forward through Launch Services (`open -b`), which the system honours from a background process too.
pub(super) fn bring_forward(app: &Running) -> bool {
    match &app.bundle {
        Some(bundle) => crate::process::command("/usr/bin/open")
            .args(["-b", bundle])
            .status()
            .is_ok_and(|status| status.success()),
        None => activate(app),
    }
}

pub(super) fn activate(app: &Running) -> bool {
    instances(app).iter().any(|instance| {
        instance.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows)
    })
}

pub(super) fn terminate(app: &Running) -> bool {
    let mut asked = false;
    for instance in instances(app) {
        asked |= instance.terminate();
    }
    asked
}

pub(super) fn force_terminate(app: &Running) -> bool {
    let mut forced = false;
    for instance in instances(app) {
        forced |= instance.forceTerminate();
    }
    forced
}

pub(super) fn protected(target: &Target) -> Option<WireError> {
    (target.bundle.as_deref() == Some(FINDER)).then(|| permission("APPLICATION_SYSTEM_PROTECTED", "Finder sistem tarafından yeniden başlatıldığı için uygulama gibi kapatılamaz; pencere kapatma eylemi kullanın."))
}

/// A browser is handed a blank page, which every browser opens as a new tab of the running instance.
pub(super) fn open_blank_tab(target: &Target) -> Result<(), String> {
    let path = target
        .path
        .as_ref()
        .ok_or_else(|| format!("{} için yeni sekme açılamadı.", target.name))?;
    let status = crate::process::command("/usr/bin/open")
        .args(["-a", path, "about:blank"])
        .status()
        .map_err(|_| format!("{} için yeni sekme açılamadı.", target.name))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} için yeni sekme açılamadı.", target.name))
    }
}

/// Closes every tab of the browser's front window except one: the tab whose title or address contains `keep`, else the tab in front. It goes through
/// the browser's own scripting dictionary (Chromium's or Safari's), so it needs the Automation permission the first time; Firefox and Arc have none.
pub(super) fn close_other_tabs(
    target: &Target,
    keep: &str,
) -> Result<super::ClosedTabs, (&'static str, String)> {
    let name = &target.name;
    let unsupported = || {
        ("APPLICATION_ACTION_UNAVAILABLE", format!("{name} sekmelerini kapatmaya izin vermiyor; diğer sekmeleri ekrandan (sekmeye sağ tık > Diğer sekmeleri kapat) kapatmak gerekir."))
    };
    let bundle = target.bundle.as_deref().ok_or_else(unsupported)?;
    let safari = bundle.starts_with("com.apple.Safari");
    let (active, set_active, title) = if safari {
        (
            "index of current tab of w",
            "set current tab of w to tab found of w",
            "name",
        )
    } else {
        (
            "active tab index of w",
            "set active tab index of w to found",
            "title",
        )
    };
    if !super::TAB_SCRIPTABLE.contains(&bundle) {
        return Err(unsupported());
    }
    let script = format!(
        r#"on run argv
set keepText to item 1 of argv
tell application id "{bundle}"
if (count of windows) is 0 then error "WIQO_NO_WINDOW"
set w to front window
set keepIndex to {active}
if keepText is not "" then
set found to 0
repeat with i from 1 to (count of tabs of w)
set t to tab i of w
if ({title} of t contains keepText) or (URL of t contains keepText) then
set found to i
exit repeat
end if
end repeat
if found is 0 then error "WIQO_TAB_NOT_FOUND"
{set_active}
set keepIndex to found
end if
set keptTitle to {title} of tab keepIndex of w
set closed to 0
repeat with i from (count of tabs of w) to 1 by -1
if i is not keepIndex then
close tab i of w
set closed to closed + 1
end if
end repeat
return (closed as text) & linefeed & keptTitle
end tell
end run"#
    );
    let mut command = crate::process::command("/usr/bin/osascript");
    command.args(["-e", &script, keep]);
    let output =
        crate::process::run_with_timeout(command, Duration::from_secs(20)).ok_or_else(|| {
            (
                "OUTCOME_UNKNOWN",
                format!("{name} sekmeleri kapatırken zamanında yanıt vermedi."),
            )
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        let (count, kept) = stdout.split_once('\n').unwrap_or((stdout.as_str(), ""));
        return Ok(super::ClosedTabs {
            count: count.trim().parse().unwrap_or(0),
            kept: kept.trim().to_string(),
        });
    }
    Err(if stderr.contains("WIQO_NO_WINDOW") {
        (
            "BROWSER_NO_WINDOW",
            format!("{name} açık ama penceresi yok; kapatılacak sekme yok."),
        )
    } else if stderr.contains("WIQO_TAB_NOT_FOUND") {
        ("TAB_NOT_FOUND", format!("{name} ön penceresinde başlığında ya da adresinde “{keep}” geçen sekme yok; hiçbir sekme kapatılmadı."))
    } else if crate::applescript::refused(&stderr) {
        ("AUTOMATION_PERMISSION_REQUIRED", format!("Wiqo'nun {name} sekmelerini yönetmesine izin verilmemiş. Sistem Ayarları > Gizlilik ve Güvenlik > Otomasyon'da Wiqo altında {name}'u aç."))
    } else {
        ("OUTCOME_UNKNOWN", format!("{name} sekmeleri kapatılamadı."))
    })
}

/// Opens an address in the browser that is in front when it can open web addresses (Safari just opened by the previous step, say), so "Safari'yi aç,
/// YouTube'da ara" does not land in Chrome. Returns the browser's name, or `None` to let the default browser handle it.
pub(crate) fn open_url_in_frontmost_browser(raw: &str) -> Option<String> {
    use objc2_foundation::{NSString, NSURL};
    let workspace = NSWorkspace::sharedWorkspace();
    let front = workspace.frontmostApplication()?;
    let front_url = front.bundleURL()?.path()?.to_string();
    let target = NSURL::URLWithString(&NSString::from_str(raw))?;
    let browsers: Vec<String> = workspace
        .URLsForApplicationsToOpenURL(&target)
        .iter()
        .filter_map(|url| url.path().map(|path| path.to_string()))
        .collect();
    if !browsers.contains(&front_url) {
        return None;
    }
    let status = crate::process::command("/usr/bin/open")
        .args(["-a", &front_url, raw])
        .status()
        .ok()?;
    status.success().then(|| {
        front
            .localizedName()
            .map(|name| name.to_string())
            .unwrap_or_else(|| "tarayıcı".into())
    })
}
