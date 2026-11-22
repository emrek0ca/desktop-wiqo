//! Wiqo inside the Microsoft Store package (MSIX). The Store installs and updates the package itself and keeps its files read-only, so the built-in
//! updater must stay off, and "start with Windows" is the package's startup task (the autostart plugin's registry entry is not started from a package).
//! Everywhere else (macOS, Linux, the installer build on Windows) none of this applies.

#[cfg(windows)]
mod imp {
    use windows::core::HSTRING;
    use windows::ApplicationModel::Activation::ActivationKind;
    use windows::ApplicationModel::{AppInstance, StartupTask, StartupTaskState};

    /// Must be the TaskId of the startupTask extension in windows/msix/AppxManifest.xml.
    const STARTUP_TASK: &str = "WiqoStartup";

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentPackageFullName(length: *mut u32, name: *mut u16) -> i32;
    }

    /// True when the process runs with a package identity (installed from the Store package).
    pub fn is_packaged() -> bool {
        const APPMODEL_ERROR_NO_PACKAGE: i32 = 15700;
        let mut length: u32 = 0;
        // With no buffer the call reports the missing package or the needed length (ERROR_INSUFFICIENT_BUFFER, 122).
        unsafe {
            GetCurrentPackageFullName(&mut length, std::ptr::null_mut())
                != APPMODEL_ERROR_NO_PACKAGE
        }
    }

    fn task() -> Option<StartupTask> {
        StartupTask::GetAsync(&HSTRING::from(STARTUP_TASK))
            .ok()?
            .get()
            .ok()
    }

    pub fn startup_enabled() -> bool {
        task()
            .and_then(|task| task.State().ok())
            .is_some_and(|state| {
                state == StartupTaskState::Enabled || state == StartupTaskState::EnabledByPolicy
            })
    }

    pub fn set_startup(on: bool) -> Result<(), String> {
        let task = task().ok_or_else(|| "Başlangıç görevi bulunamadı.".to_string())?;
        if on {
            let state = task
                .RequestEnableAsync()
                .and_then(|operation| operation.get())
                .map_err(|error| error.message())?;
            if state == StartupTaskState::DisabledByUser {
                return Err(
                    "Windows Ayarları > Uygulamalar > Başlangıç bölümünde Wiqo kapalı; oradan aç."
                        .into(),
                );
            }
            Ok(())
        } else {
            task.Disable().map_err(|error| error.message())
        }
    }

    /// Windows started Wiqo at sign-in (the startup task cannot pass `--hidden`).
    pub fn started_by_windows() -> bool {
        AppInstance::GetActivatedEventArgs()
            .and_then(|args| args.Kind())
            .is_ok_and(|kind| kind == ActivationKind::StartupTask)
    }
}

#[cfg(not(windows))]
#[allow(dead_code)] // the callers that need these are the Windows build
mod imp {
    pub fn is_packaged() -> bool {
        false
    }
    pub fn startup_enabled() -> bool {
        false
    }
    pub fn set_startup(_on: bool) -> Result<(), String> {
        Ok(())
    }
    pub fn started_by_windows() -> bool {
        false
    }
}

#[allow(unused_imports)]
pub use imp::{is_packaged, set_startup, started_by_windows, startup_enabled};
