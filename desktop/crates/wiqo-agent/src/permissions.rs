//! The system permissions Wiqo needs on this computer, read without bothering the person and asked for one click at a time. Only macOS gates these
//! (Screen Recording, Accessibility, Automation per app, the Desktop/Documents/Downloads folders, Full Disk Access); Windows and Linux need none.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    Granted,
    Denied,
    NotDetermined,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Permission {
    pub id: &'static str,
    pub title: &'static str,
    pub why: &'static str,
    pub state: State,
    /// Not needed for the everyday tasks.
    pub optional: bool,
    /// "5/7 uygulama" for the grouped Automation row.
    pub detail: Option<String>,
    /// Granted, but takes effect only after Wiqo restarts (Screen Recording).
    pub needs_restart: bool,
}

/// What was asked before: a folder or app the person already answered for can be read without a new dialog.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Asked {
    folders: bool,
    automation: BTreeMap<String, bool>,
    screen_granted_at_launch: Option<bool>,
    /// Screen Recording was asked for once: from then on a missing permission is a refusal, not "not asked yet".
    #[serde(default)]
    screen_asked: bool,
}

/// The apps Wiqo drives with AppleScript (bundle id, name).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const AUTOMATION: [(&str, &str); 7] = [
    ("com.apple.systemevents", "Sistem Olayları"),
    ("com.apple.finder", "Finder"),
    ("com.apple.mail", "Mail"),
    ("com.apple.MobileSMS", "Mesajlar"),
    ("com.apple.Notes", "Notlar"),
    ("com.apple.iCal", "Takvim"),
    ("com.apple.reminders", "Anımsatıcılar"),
];

/// Which permission an operation's error code points at, so the window can open on it.
pub fn for_error(code: &str) -> Option<&'static str> {
    match code {
        "SCREEN_RECORDING_PERMISSION_REQUIRED" => Some("screen"),
        "ACCESSIBILITY_PERMISSION" | "ACCESSIBILITY_PERMISSION_REQUIRED" => Some("accessibility"),
        "FULL_DISK_ACCESS_REQUIRED" => Some("folders"),
        "LOCATION_PERMISSION_REQUIRED" => Some("location"),
        code if code.ends_with("_PERMISSION_DENIED")
            || code == "AUTOMATION_PERMISSION"
            || code == "SHORTCUT_PERMISSION_REQUIRED" =>
        {
            Some("automation")
        }
        _ => None,
    }
}

pub struct Permissions {
    store: PathBuf,
}

impl Permissions {
    pub fn new(store: PathBuf) -> Permissions {
        let permissions = Permissions { store };
        let mut asked = permissions.asked();
        asked.screen_granted_at_launch = Some(sys::screen());
        permissions.save(&asked);
        permissions
    }

    fn asked(&self) -> Asked {
        std::fs::read(&self.store)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, asked: &Asked) {
        if let Ok(bytes) = serde_json::to_vec_pretty(asked) {
            let _ = std::fs::write(&self.store, bytes);
        }
    }

    /// Every permission and where it stands. Never shows a system dialog.
    pub fn list(&self) -> Vec<Permission> {
        if !cfg!(target_os = "macos") {
            return Vec::new();
        }
        let asked = self.asked();
        let screen = sys::screen();
        let automation: Vec<Option<bool>> = AUTOMATION
            .iter()
            .map(|(bundle, _)| {
                sys::automation(bundle, false).or_else(|| asked.automation.get(*bundle).copied())
            })
            .collect();
        let granted = automation
            .iter()
            .filter(|state| **state == Some(true))
            .count();
        let automation_state = if granted == AUTOMATION.len() {
            State::Granted
        } else if automation.contains(&Some(false)) {
            State::Denied
        } else {
            State::NotDetermined
        };
        let folders = if asked.folders {
            if sys::folders_readable() {
                State::Granted
            } else {
                State::Denied
            }
        } else {
            State::NotDetermined
        };
        vec![
            Permission {
                id: "screen",
                title: "Ekran Kaydı",
                why: "Ekran görüntüsü alıp telefona gönderebilmek için.",
                state: if screen { State::Granted } else { State::NotDetermined },
                optional: false,
                detail: None,
                needs_restart: screen && asked.screen_granted_at_launch == Some(false),
            },
            Permission {
                id: "accessibility",
                title: "Erişilebilirlik",
                why: "Pencereleri dizmek ve uygulamaları öne getirmek için.",
                state: if sys::accessibility() { State::Granted } else { State::NotDetermined },
                optional: false,
                detail: None,
                needs_restart: false,
            },
            Permission {
                id: "automation",
                title: "Uygulamaları kontrol (Otomasyon)",
                why: "Mesaj, e-posta, not, takvim ve hatırlatıcı işleri için. Her uygulama için macOS bir kez sorar.",
                state: automation_state,
                optional: false,
                detail: Some(format!("{granted}/{} uygulama", AUTOMATION.len())),
                needs_restart: false,
            },
            Permission {
                id: "folders",
                title: "Masaüstü, Belgeler, İndirilenler",
                why: "Bu klasörlerdeki dosyaları bulmak, okumak ve düzenlemek için.",
                state: folders,
                optional: false,
                detail: None,
                needs_restart: false,
            },
            Permission {
                id: "location",
                title: "Konum",
                why: "İsteğe bağlı: hava durumu gibi yere bağlı sorularda bulunduğun şehri bilmek için. Yalnızca şehir adı gönderilir.",
                state: match crate::location::access() {
                    crate::location::Access::Granted => State::Granted,
                    crate::location::Access::NotDetermined => State::NotDetermined,
                    _ => State::Denied,
                },
                optional: true,
                detail: None,
                needs_restart: false,
            },
            Permission {
                id: "files",
                title: "Tam Disk Erişimi",
                why: "İsteğe bağlı: bu üç klasörün dışındaki dosyalarda da arama ve okuma için.",
                state: if sys::full_disk() { State::Granted } else { State::NotDetermined },
                optional: true,
                detail: None,
                needs_restart: false,
            },
        ]
    }

    /// Capabilities a refused or missing permission blocks, with the sentence the person reads (the server shows it instead of trying).
    /// Only what is known: an Automation permission the person has not been asked for yet is not a blocker (the first use asks).
    pub fn blockers(&self) -> std::collections::HashMap<String, String> {
        let mut blocked = std::collections::HashMap::new();
        if !cfg!(target_os = "macos") {
            return blocked;
        }
        let list = self.list();
        let state = |id: &str| {
            list.iter()
                .find(|permission| permission.id == id)
                .map(|permission| permission.state)
        };
        // Never asked yet: the first screenshot asks (the system dialog), so it is not blocked in advance.
        if state("screen") != Some(State::Granted) && self.asked().screen_asked {
            blocked.insert("device.screen.capture".into(), "PC'de Ekran Kaydı izni yok: bilgisayardaki Wiqo › Ayarlar › Sistem izinleri › Ekran Kaydı'na tek tıkla izin ver.".into());
        }
        if state("folders") == Some(State::Denied) {
            for capability in [
                "device.files.list",
                "device.files.read",
                "device.files.write",
                "device.files.move",
                "device.files.copy",
                "device.files.delete",
                "device.files.utility",
            ] {
                blocked.insert(capability.into(), "PC'de Masaüstü/Belgeler/İndirilenler izni kapalı: Wiqo › Ayarlar › Sistem izinleri'nden ver.".into());
            }
        }
        let asked = self.asked();
        let refused = |bundle: &str| {
            sys::automation(bundle, false).or_else(|| asked.automation.get(bundle).copied())
                == Some(false)
        };
        let apps: [(&str, &str, &[&str]); 5] = [
            ("com.apple.mail", "Mail", &["mail.send"]),
            (
                "com.apple.MobileSMS",
                "Mesajlar",
                &["device.messaging.send"],
            ),
            ("com.apple.Notes", "Notlar", &["notes.write"]),
            (
                "com.apple.iCal",
                "Takvim",
                &["calendar.read", "calendar.write"],
            ),
            (
                "com.apple.reminders",
                "Anımsatıcılar",
                &["reminders.read", "reminders.write"],
            ),
        ];
        for (bundle, name, capabilities) in apps {
            if refused(bundle) {
                for capability in capabilities {
                    blocked.insert((*capability).into(), format!("PC'de Wiqo'nun {name} izni kapalı: Wiqo › Ayarlar › Sistem izinleri › Uygulamaları kontrol."));
                }
            }
        }
        blocked
    }

    /// A step failed for a missing permission: asks for exactly that one right away. A refused Automation answer goes straight to its
    /// Settings page (macOS never asks twice); the others show the system dialog when it can still be shown. Blocks while a dialog is open.
    pub fn request_for_error(&self, code: &str) -> Option<&'static str> {
        let id = for_error(code)?;
        if id == "automation" {
            sys::open_settings("Privacy_Automation");
        } else {
            self.request(id);
        }
        Some(id)
    }

    /// Asks for one permission: the system dialog when macOS still can show one, otherwise the right page of System Settings. Blocks while a
    /// dialog is open (call it off the UI thread).
    pub fn request(&self, id: &str) {
        let current = self
            .list()
            .into_iter()
            .find(|permission| permission.id == id);
        let undecided = current
            .as_ref()
            .is_some_and(|permission| permission.state == State::NotDetermined);
        match id {
            "screen" => {
                let mut asked = self.asked();
                let first_time = !asked.screen_asked;
                asked.screen_asked = true;
                self.save(&asked);
                // The first call shows the system dialog; after that macOS only lists Wiqo in Settings.
                if !sys::request_screen() && !first_time {
                    sys::open_settings("Privacy_ScreenCapture");
                }
            }
            "location" => match crate::location::access() {
                crate::location::Access::NotDetermined => {
                    let _ = crate::location::current_city(std::time::Duration::from_secs(30));
                }
                crate::location::Access::Granted => {}
                _ => sys::open_settings("Privacy_LocationServices"),
            },
            "accessibility" => {
                if !sys::request_accessibility() {
                    sys::open_settings("Privacy_Accessibility");
                }
            }
            "automation" => {
                let mut asked = self.asked();
                let mut refused = false;
                for (bundle, _) in AUTOMATION {
                    if asked.automation.get(bundle) == Some(&true)
                        && sys::automation(bundle, false) != Some(false)
                    {
                        continue;
                    }
                    let answer = sys::request_automation(bundle);
                    if let Some(answer) = answer {
                        asked.automation.insert(bundle.to_string(), answer);
                        refused |= !answer;
                    }
                    self.save(&asked);
                }
                if refused || !undecided {
                    sys::open_settings("Privacy_Automation");
                }
            }
            "folders" => {
                let mut asked = self.asked();
                let first_time = !asked.folders;
                asked.folders = true;
                self.save(&asked);
                // Reading each folder once is what makes macOS ask; after an answer it never asks again.
                if !sys::read_folders() && !first_time {
                    sys::open_settings("Privacy_FilesAndFolders");
                }
            }
            "files" => sys::open_settings("Privacy_AllFiles"),
            _ => {}
        }
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::c_void;
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        static kAXTrustedCheckOptionPrompt: *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            count: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;
        fn CFRelease(object: *const c_void);
        static kCFBooleanTrue: *const c_void;
        static kCFTypeDictionaryKeyCallBacks: u8;
        static kCFTypeDictionaryValueCallBacks: u8;
    }

    /// `AEDesc` is declared under `#pragma pack(2)`.
    #[repr(C, packed(2))]
    struct AEDesc {
        descriptor_type: u32,
        data_handle: *mut c_void,
    }

    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        fn AECreateDesc(
            type_code: u32,
            data: *const c_void,
            size: isize,
            result: *mut AEDesc,
        ) -> i16;
        fn AEDisposeDesc(desc: *mut AEDesc) -> i16;
        fn AEDeterminePermissionToAutomateTarget(
            target: *const AEDesc,
            event_class: u32,
            event_id: u32,
            ask: u8,
        ) -> i32;
    }

    const TYPE_BUNDLE_ID: u32 = u32::from_be_bytes(*b"bund");
    const WILDCARD: u32 = u32::from_be_bytes(*b"****");

    pub fn screen() -> bool {
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    /// Shows the dialog the first time; returns whether access is already granted.
    pub fn request_screen() -> bool {
        unsafe { CGRequestScreenCaptureAccess() }
    }

    pub fn accessibility() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    /// Puts Wiqo in the Accessibility list and shows the system prompt that leads there.
    pub fn request_accessibility() -> bool {
        unsafe {
            let keys = [kAXTrustedCheckOptionPrompt];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                std::ptr::addr_of!(kCFTypeDictionaryKeyCallBacks).cast(),
                std::ptr::addr_of!(kCFTypeDictionaryValueCallBacks).cast(),
            );
            let trusted = AXIsProcessTrustedWithOptions(options);
            if !options.is_null() {
                CFRelease(options);
            }
            trusted
        }
    }

    /// `Some(true/false)` once the person answered for this app; `None` when undecided or the app is not running (macOS cannot tell then).
    pub fn automation(bundle: &str, ask: bool) -> Option<bool> {
        unsafe {
            let mut target = AEDesc {
                descriptor_type: 0,
                data_handle: std::ptr::null_mut(),
            };
            if AECreateDesc(
                TYPE_BUNDLE_ID,
                bundle.as_ptr().cast(),
                bundle.len() as isize,
                &mut target,
            ) != 0
            {
                return None;
            }
            let status =
                AEDeterminePermissionToAutomateTarget(&target, WILDCARD, WILDCARD, ask as u8);
            AEDisposeDesc(&mut target);
            match status {
                0 => Some(true),
                -1743 => Some(false),
                _ => None,
            }
        }
    }

    fn running(bundle: &str) -> bool {
        Command::new("/usr/bin/osascript")
            .args(["-e", &format!("application id \"{bundle}\" is running")])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim() == "true")
            .unwrap_or(false)
    }

    /// Opens the app in the background if needed (macOS asks only about a running app), then asks.
    pub fn request_automation(bundle: &str) -> Option<bool> {
        if !running(bundle) {
            let _ = Command::new("/usr/bin/open")
                .args(["-g", "-j", "-b", bundle])
                .status();
            let deadline = Instant::now() + Duration::from_secs(8);
            while !running(bundle) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        automation(bundle, true)
    }

    fn own_folders() -> Vec<std::path::PathBuf> {
        let home = dirs::home_dir().unwrap_or_default();
        ["Desktop", "Documents", "Downloads"]
            .iter()
            .map(|name| home.join(name))
            .collect()
    }

    pub fn folders_readable() -> bool {
        own_folders()
            .iter()
            .all(|folder| std::fs::read_dir(folder).is_ok())
    }

    /// Reads every folder (each one asks once), not stopping at the first refusal.
    pub fn read_folders() -> bool {
        let answers: Vec<bool> = own_folders()
            .iter()
            .map(|folder| std::fs::read_dir(folder).is_ok())
            .collect();
        answers.into_iter().all(|ok| ok)
    }

    pub fn full_disk() -> bool {
        let home = dirs::home_dir().unwrap_or_default();
        // These are readable only with Full Disk Access, and reading them never shows a dialog.
        ["Library/Safari", "Library/Mail", "Library/Messages"]
            .iter()
            .map(|path| home.join(path))
            .filter(|path| path.exists())
            .any(|path| std::fs::read_dir(path).is_ok())
    }

    pub fn open_settings(anchor: &str) {
        let _ = Command::new("/usr/bin/open")
            .arg(format!(
                "x-apple.systempreferences:com.apple.preference.security?{anchor}"
            ))
            .status();
    }
}

#[cfg(not(target_os = "macos"))]
mod sys {
    pub fn screen() -> bool {
        true
    }
    pub fn request_screen() -> bool {
        true
    }
    pub fn accessibility() -> bool {
        true
    }
    pub fn request_accessibility() -> bool {
        true
    }
    pub fn automation(_bundle: &str, _ask: bool) -> Option<bool> {
        Some(true)
    }
    pub fn request_automation(_bundle: &str) -> Option<bool> {
        Some(true)
    }
    pub fn folders_readable() -> bool {
        true
    }
    pub fn read_folders() -> bool {
        true
    }
    pub fn full_disk() -> bool {
        true
    }
    pub fn open_settings(_anchor: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_point_at_the_permission_that_fixes_them() {
        assert_eq!(
            for_error("SCREEN_RECORDING_PERMISSION_REQUIRED"),
            Some("screen")
        );
        assert_eq!(for_error("NOTE_PERMISSION_DENIED"), Some("automation"));
        assert_eq!(for_error("FILE_NOT_FOUND"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn listing_never_asks_and_covers_every_permission() {
        let store =
            std::env::temp_dir().join(format!("wiqo-permissions-{}.json", std::process::id()));
        let list = Permissions::new(store).list();
        let ids: Vec<&str> = list.iter().map(|permission| permission.id).collect();
        assert_eq!(
            ids,
            [
                "screen",
                "accessibility",
                "automation",
                "folders",
                "location",
                "files"
            ]
        );
    }
}
