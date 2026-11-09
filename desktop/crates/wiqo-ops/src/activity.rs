//! What is in front of the person right now: the front application, its window's title, the address of the tab in front when it is a browser, and
//! what is selected in Finder. One reading, used by the `context` report ("bunu özetle" finds what "bunu" is), by the screen memory (what was on the
//! screen, and where) and by the recorder (a task the person shows once). Read only; nothing here needs more than the permissions those already ask.

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Front {
    pub app: String,
    pub window: String,
    /// The tab in front, for Safari, Chrome, Edge, Brave and Arc on macOS.
    pub url: Option<String>,
    /// Finder's selection, as paths.
    pub selection: Vec<String>,
}

impl Front {
    /// A private window says so in its title; nothing of it is remembered or recorded.
    pub fn is_private(&self) -> bool {
        let title = self.window.to_lowercase();
        ["private browsing", "gizli pencere", "gizli sekme", "inprivate", "incognito", "özel tarama"]
            .iter()
            .any(|mark| title.contains(mark))
    }
}

#[cfg(target_os = "macos")]
pub fn front() -> Option<Front> {
    use crate::applescript::FIELD;
    let source = format!(
        r#"
set sep to (ASCII character {field})
set appName to ""
set winTitle to ""
tell application "System Events"
  set frontProcess to first application process whose frontmost is true
  set appName to name of frontProcess
  try
    set winTitle to name of front window of frontProcess
  end try
end tell
set pageAddress to ""
try
  if appName is "Safari" then
    tell application "Safari" to set pageAddress to URL of current tab of front window
  else if appName is in {{"Google Chrome", "Microsoft Edge", "Brave Browser", "Arc", "Chromium"}} then
    tell application appName to set pageAddress to URL of active tab of front window
  end if
end try
set picked to ""
if appName is "Finder" then
  try
    tell application "Finder"
      repeat with item_ in (selection as alias list)
        set picked to picked & POSIX path of item_ & linefeed
      end repeat
    end tell
  end try
end if
return appName & sep & winTitle & sep & pageAddress & sep & picked
"#,
        field = FIELD as u32
    );
    let mut command = crate::process::command("/usr/bin/osascript");
    command.args(["-e", &source]);
    let out = crate::process::run_with_timeout(command, std::time::Duration::from_secs(8))?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string();
    let mut parts = text.split(FIELD);
    let app = parts.next().unwrap_or_default().trim().to_string();
    if app.is_empty() {
        return None;
    }
    let window = parts.next().unwrap_or_default().trim().to_string();
    let url = parts.next().map(str::trim).filter(|url| !url.is_empty()).map(str::to_string);
    let selection = parts
        .next()
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(20)
        .map(str::to_string)
        .collect();
    Some(Front { app, window, url, selection })
}

#[cfg(windows)]
pub fn front() -> Option<Front> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId};
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return None;
        }
        let mut title = [0u16; 512];
        let length = GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32).max(0) as usize;
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        let mut app = String::new();
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if !process.is_null() {
            let mut path = [0u16; 1024];
            let mut size = path.len() as u32;
            if QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut size) != 0 {
                let full = String::from_utf16_lossy(&path[..size as usize]);
                app = std::path::Path::new(&full)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
                    .unwrap_or_default();
            }
            CloseHandle(process);
        }
        if app.is_empty() {
            return None;
        }
        Some(Front {
            app,
            window: String::from_utf16_lossy(&title[..length]),
            url: None,
            selection: Vec::new(),
        })
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn front() -> Option<Front> {
    None
}

/// The reading as the lines of the `context` report.
pub fn lines(front: &Front) -> String {
    let mut out = format!("Ön plandaki uygulama: {}\nPencere: {}", front.app, front.window);
    if let Some(url) = &front.url {
        out.push_str(&format!("\nSekme adresi: {url}"));
    }
    if !front.selection.is_empty() {
        out.push_str(&format!("\nFinder'da seçili:\n{}", front.selection.join("\n")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_private_window_is_known_by_its_title() {
        let window = |title: &str| Front { app: "Safari".into(), window: title.into(), ..Default::default() };
        assert!(window("Gizli Pencere — Banka").is_private());
        assert!(window("Yeni sekme - Google Chrome (Incognito)").is_private());
        assert!(!window("Faturalar — Turkcell").is_private());
    }

    #[test]
    fn the_report_lists_what_there_is() {
        let front = Front { app: "Finder".into(), window: "İndirilenler".into(), url: None, selection: vec!["/Users/a/x.pdf".into()] };
        assert_eq!(lines(&front), "Ön plandaki uygulama: Finder\nPencere: İndirilenler\nFinder'da seçili:\n/Users/a/x.pdf");
    }
}
