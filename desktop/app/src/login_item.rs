//! Starting with the computer. On macOS this is a launch agent inside the app bundle (Contents/Library/LaunchAgents), registered through
//! SMAppService: System Settings and the "background item" notice show it as Wiqo, with Wiqo's icon, and it starts Wiqo hidden in the tray.
//! (A plain ~/Library/LaunchAgents file shows the signing developer's name instead.) Windows and Linux use the autostart plugin.

#[cfg(target_os = "macos")]
mod imp {
    use objc2_foundation::NSString;
    use objc2_service_management::{SMAppService, SMAppServiceStatus};

    const PLIST: &str = "com.wiqo.desktop.login.plist";

    fn service() -> objc2::rc::Retained<SMAppService> {
        unsafe { SMAppService::agentServiceWithPlistName(&NSString::from_str(PLIST)) }
    }

    pub fn is_enabled(_handle: &tauri::AppHandle) -> bool {
        unsafe { service().status() == SMAppServiceStatus::Enabled }
    }

    pub fn set(handle: &tauri::AppHandle, on: bool) -> Result<(), String> {
        // The earlier versions wrote ~/Library/LaunchAgents/Wiqo.plist through the autostart plugin: it goes, so only this one starts Wiqo.
        {
            use tauri_plugin_autostart::ManagerExt;
            let _ = handle.autolaunch().disable();
        }
        let service = service();
        let result = unsafe {
            if on {
                service.registerAndReturnError()
            } else {
                service.unregisterAndReturnError()
            }
        };
        result.map_err(|error| error.localizedDescription().to_string())
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use tauri_plugin_autostart::ManagerExt;

    pub fn is_enabled(handle: &tauri::AppHandle) -> bool {
        handle.autolaunch().is_enabled().unwrap_or(false)
    }

    pub fn set(handle: &tauri::AppHandle, on: bool) -> Result<(), String> {
        let launcher = handle.autolaunch();
        if on {
            launcher.enable()
        } else {
            launcher.disable()
        }
        .map_err(|error| error.to_string())
    }
}

pub use imp::{is_enabled, set};

/// The first launch turns it on; after that it is the person's switch. Moving from the old launch agent to the bundled one happens once too.
pub fn first_launch(handle: &tauri::AppHandle) {
    let Some(marker) = wiqo_agent::data_file("login-item-v2") else {
        return;
    };
    if marker.exists() {
        return;
    }
    let was_on = wiqo_agent::data_file("autostart-chosen").is_some_and(|old| old.exists());
    let _ = was_on;
    if set(handle, true).is_ok() {
        let _ = std::fs::write(marker, b"1");
    }
}
