//! `systemUtility`: small system chores the phone asks for in one sentence — dark mode, the busiest processes, battery health, two windows side by side,
//! a text read aloud into an audio file on the Desktop.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use wiqo_core::engine::{Context, Lane};

use crate::op::{data, error, payload, permission, tool_failure, Blocking, Done, Result};
use crate::process::run_with_timeout;
use crate::Op;

pub(crate) fn operations() -> Vec<Op> {
    vec![Arc::new(Blocking {
        name: "systemUtility",
        lane: Lane::SideEffect,
        run: system_utility,
    })]
}

/// `SystemUtilityPayload`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Request {
    action: String,
    mode: Option<String>,
    text: Option<String>,
    limit: usize,
}

impl Default for Request {
    fn default() -> Self {
        Request {
            action: String::new(),
            mode: None,
            text: None,
            limit: 10,
        }
    }
}

fn system_utility(_ctx: &Context, value: &Value) -> Result<Done> {
    let request: Request = payload(value, "systemUtility", "sistem işlemi")?;
    match request.action.as_str() {
        "darkMode" => dark_mode(request.mode.as_deref()),
        "topProcesses" => top_processes(request.limit.clamp(1, 50)),
        "battery" => battery(),
        "systemInfo" => system_info(),
        "overview" => overview(),
        "context" => context_report(),
        "disks" => disks_report(),
        "network" => network_report(),
        "arrangeWindows" => arrange_windows(request.mode.as_deref()),
        "speak" => speak(request.text.as_deref().unwrap_or("")),
        _ => Err(error(
            "INVALID_PAYLOAD",
            "Bu sistem işlemi tanınmıyor.",
            "validation",
        )),
    }
}

fn report(title: &str, text: &str) -> Value {
    let text: String = text.chars().take(8_000).collect();
    data(
        "fileData",
        json!({ "path": title, "mimeType": "text/plain", "text": text, "truncated": false }),
    )
}

fn run(program: &str, args: &[&str], seconds: u64) -> Option<std::process::Output> {
    let mut command = crate::process::command(program);
    command.args(args);
    run_with_timeout(command, Duration::from_secs(seconds))
}

/// Runs AppleScript; the error is the sentence for the person (a missing Automation permission names the setting).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn apple_script(source: &str) -> std::result::Result<String, String> {
    let Some(out) = run("/usr/bin/osascript", &["-e", source], 20) else {
        return Err("İşlem zaman aşımına uğradı.".into());
    };
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(
        if stderr.contains("-1743") || stderr.contains("-1719") || stderr.contains("not allowed") {
            "Bilgisayardaki Wiqo penceresinde İzinler bölümünden Erişilebilirlik ve Uygulamaları kontrol izinlerini ver."
                .into()
        } else {
            "İşlem yapılamadı.".into()
        },
    )
}

fn unsupported(what: &str) -> WireErrorResult {
    Err(tool_failure(
        "UNSUPPORTED_ON_PLATFORM",
        format!("{what} bu bilgisayarda henüz yapılamıyor."),
    ))
}

type WireErrorResult = Result<Done>;

fn dark_mode(mode: Option<&str>) -> Result<Done> {
    let message = match mode {
        Some("on") => "Koyu mod açıldı.",
        Some("off") => "Açık mod açıldı.",
        _ => "Görünüm değiştirildi.",
    };
    if cfg!(target_os = "macos") {
        let value = match mode {
            Some("on") => "true",
            Some("off") => "false",
            _ => "not dark mode",
        };
        apple_script(&format!(
            "tell application \"System Events\" to tell appearance preferences to set dark mode to {value}"
        ))
        .map_err(|message| permission("AUTOMATION_PERMISSION", message))?;
        return Ok(Done::message(message));
    }
    if cfg!(windows) {
        let key = r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
        let value = match mode {
            Some("on") => "0".to_string(),
            Some("off") => "1".to_string(),
            _ => format!("1 - (Get-ItemProperty '{key}').AppsUseLightTheme"),
        };
        let script = format!(
            "$v={value}; Set-ItemProperty '{key}' AppsUseLightTheme $v; Set-ItemProperty '{key}' SystemUsesLightTheme $v"
        );
        let out = run(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", &script],
            15,
        );
        if !out.is_some_and(|out| out.status.success()) {
            return Err(tool_failure("DARK_MODE_FAILED", "Görünüm değiştirilemedi."));
        }
        return Ok(Done::message(message));
    }
    // GNOME and its relatives; other desktops keep their own setting.
    let scheme = match mode {
        Some("off") => "default",
        _ => "prefer-dark",
    };
    let out = run(
        "gsettings",
        &["set", "org.gnome.desktop.interface", "color-scheme", scheme],
        10,
    );
    if !out.is_some_and(|out| out.status.success()) {
        return unsupported("Koyu mod");
    }
    Ok(Done::message(message))
}

fn top_processes(limit: usize) -> Result<Done> {
    let lines = process_lines(limit);
    if lines.len() <= 1 {
        return Err(tool_failure("PROCESS_LIST_FAILED", "Süreçler okunamadı."));
    }
    Ok(Done::new(
        format!("En çok kaynak kullanan {limit} süreç."),
        Some(report("top-processes", &lines.join("\n"))),
    ))
}

const GIB: f64 = 1_073_741_824.0;

/// System and version, processor, memory: what the computer is.
fn system_lines() -> Vec<String> {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    system.refresh_cpu_all();
    let cpus = system.cpus();
    let brand = cpus
        .first()
        .map(|cpu| cpu.brand().trim().to_string())
        .filter(|brand| !brand.is_empty())
        .unwrap_or_else(|| "bilinmiyor".into());
    let uptime = sysinfo::System::uptime();
    vec![
        format!(
            "Sistem: {} {}",
            sysinfo::System::name().unwrap_or_else(|| "bilinmiyor".into()),
            sysinfo::System::os_version().unwrap_or_default()
        ),
        format!(
            "Bilgisayar adı: {}",
            sysinfo::System::host_name().unwrap_or_else(|| "bilinmiyor".into())
        ),
        format!("İşlemci: {brand} ({} çekirdek)", cpus.len()),
        format!(
            "Bellek: {:.1} GB toplam, {:.1} GB kullanımda, {:.1} GB boş",
            system.total_memory() as f64 / GIB,
            system.used_memory() as f64 / GIB,
            system.available_memory() as f64 / GIB
        ),
        format!(
            "Açık kalma süresi: {} saat {} dakika",
            uptime / 3600,
            (uptime % 3600) / 60
        ),
    ]
}

/// The real disks (not the many views of one volume): total, free, and how full.
fn disk_lines() -> Vec<String> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut seen = std::collections::HashSet::new();
    let mut lines = Vec::new();
    for disk in disks.list() {
        let total = disk.total_space();
        if total < 1_000_000_000 || !seen.insert((total, disk.available_space())) {
            continue;
        }
        let free = disk.available_space();
        lines.push(format!(
            "Disk {}: {:.0} GB toplam, {:.0} GB boş (%{:.0} dolu)",
            disk.mount_point().to_string_lossy(),
            total as f64 / GIB,
            free as f64 / GIB,
            (total - free.min(total)) as f64 / total as f64 * 100.0
        ));
    }
    lines
}

/// The busiest processes by CPU, sampled twice so the numbers mean something.
fn process_lines(limit: usize) -> Vec<String> {
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    system.refresh_memory();
    let total_memory = system.total_memory().max(1) as f64;
    let mut processes: Vec<_> = system.processes().values().collect();
    processes.sort_by(|a, b| b.cpu_usage().total_cmp(&a.cpu_usage()));
    let mut lines = vec!["PID  %CPU  %MEM  AD".to_string()];
    lines.extend(processes.iter().take(limit).map(|process| {
        format!(
            "{}  {:.1}  {:.1}  {}",
            process.pid(),
            process.cpu_usage(),
            process.memory() as f64 / total_memory * 100.0,
            process.name().to_string_lossy()
        )
    }));
    lines
}

/// What the computer is: system and version, processor, memory, disks, uptime.
fn system_info() -> Result<Done> {
    let mut lines = system_lines();
    lines.extend(disk_lines());
    Ok(Done::new(
        "Bilgisayarın sistem bilgisi.",
        Some(report("system-info", &lines.join("\n"))),
    ))
}

/// Everything about the state of the computer in one call: system, memory, disks, battery and the busiest processes. One round trip instead of four.
fn overview() -> Result<Done> {
    let mut lines = vec!["== Sistem ==".to_string()];
    lines.extend(system_lines());
    lines.push("== Diskler ==".into());
    lines.extend(disk_lines());
    lines.push("== Pil ==".into());
    lines.push(match battery() {
        Ok(done) => done.message,
        Err(error) => error.message,
    });
    lines.push("== En çok kaynak kullanan süreçler ==".into());
    lines.extend(process_lines(8));
    Ok(Done::new(
        "Bilgisayarın genel durumu.",
        Some(report("overview", &lines.join("\n"))),
    ))
}

/// What the person is looking at right now: the front application and its window, what is selected in Finder, the address of the tab in front in a browser.
/// It is how "bunu özetle / şunu gönder" finds what "bunu" is. Read only; the clipboard is a different, explicitly permitted read.
fn context_report() -> Result<Done> {
    let lines = if cfg!(target_os = "macos") {
        let source = r#"
set out to ""
tell application "System Events"
  set frontProcess to first application process whose frontmost is true
  set appName to name of frontProcess
  set winTitle to ""
  try
    set winTitle to name of front window of frontProcess
  end try
end tell
set out to "Ön plandaki uygulama: " & appName & linefeed & "Pencere: " & winTitle
if appName is "Finder" then
  try
    tell application "Finder"
      set picked to selection as alias list
      set names to ""
      repeat with item_ in picked
        set names to names & POSIX path of item_ & linefeed
      end repeat
    end tell
    if names is not "" then set out to out & linefeed & "Finder'da seçili:" & linefeed & names
  end try
end if
if appName is "Safari" then
  try
    tell application "Safari" to set pageAddress to URL of current tab of front window
    set out to out & linefeed & "Sekme adresi: " & pageAddress
  end try
end if
if appName is "Google Chrome" then
  try
    tell application "Google Chrome" to set pageAddress to URL of active tab of front window
    set out to out & linefeed & "Sekme adresi: " & pageAddress
  end try
end if
return out
"#;
        vec![apple_script(source).map_err(|message| permission("AUTOMATION_PERMISSION", message))?]
    } else if cfg!(windows) {
        let script = r#"Add-Type @"
using System; using System.Runtime.InteropServices; using System.Text;
public class WiqoFront {
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern int GetWindowThreadProcessId(IntPtr h, out int p);
}
"@
$h = [WiqoFront]::GetForegroundWindow(); $sb = New-Object System.Text.StringBuilder 256
[void][WiqoFront]::GetWindowText($h, $sb, 256); $p = 0; [void][WiqoFront]::GetWindowThreadProcessId($h, [ref]$p)
"Ön plandaki uygulama: $((Get-Process -Id $p -ErrorAction SilentlyContinue).ProcessName)`nPencere: $($sb.ToString())""#;
        let out = run("powershell", &["-NoProfile", "-NonInteractive", "-Command", script], 15);
        match out {
            Some(out) if out.status.success() => vec![String::from_utf8_lossy(&out.stdout).trim().to_string()],
            _ => return Err(tool_failure("CONTEXT_FAILED", "Ön plandaki pencere okunamadı.")),
        }
    } else {
        return unsupported("Ön plandaki pencereyi okumak");
    };
    Ok(Done::new(
        "Şu an ekranda olan.",
        Some(report("context", &lines.join("\n"))),
    ))
}

/// Every disk with how much room is left.
fn disks_report() -> Result<Done> {
    let lines = disk_lines();
    if lines.is_empty() {
        return Err(tool_failure("DISKS_UNAVAILABLE", "Diskler okunamadı."));
    }
    Ok(Done::new(
        "Disklerin durumu.",
        Some(report("disks", &lines.join("\n"))),
    ))
}

/// The network interfaces with their addresses and what has gone through them since boot.
fn network_report() -> Result<Done> {
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let mut lines = Vec::new();
    for (name, data) in networks.iter() {
        let addresses: Vec<String> = data
            .ip_networks()
            .iter()
            .filter(|network| !network.addr.is_loopback())
            .map(|network| network.addr.to_string())
            .collect();
        if addresses.is_empty() {
            continue;
        }
        lines.push(format!(
            "{name}: {} · alınan {:.0} MB, gönderilen {:.0} MB",
            addresses.join(", "),
            data.total_received() as f64 / 1_048_576.0,
            data.total_transmitted() as f64 / 1_048_576.0
        ));
    }
    if lines.is_empty() {
        return Err(tool_failure("NETWORK_UNAVAILABLE", "Ağ bilgisi okunamadı."));
    }
    Ok(Done::new(
        "Ağ bağlantıları.",
        Some(report("network", &lines.join("\n"))),
    ))
}

/// A computer without a battery (a desktop PC, a virtual machine) is an answer, not a failure: the person learns there is none.
fn battery() -> Result<Done> {
    match battery_details() {
        Err(error) if error.code == "BATTERY_UNAVAILABLE" => Ok(Done::new(
            "Bu bilgisayarda pil yok (masaüstü bilgisayar ya da pil bilgisi verilmiyor).",
            None,
        )),
        other => other,
    }
}

fn battery_details() -> Result<Done> {
    let unavailable = || {
        tool_failure(
            "BATTERY_UNAVAILABLE",
            "Pil bilgisi okunamadı (masaüstü bilgisayar olabilir).",
        )
    };
    let mut lines: Vec<String> = Vec::new();
    if cfg!(target_os = "macos") {
        let summary = run("/usr/bin/pmset", &["-g", "batt"], 10)
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .unwrap_or_default();
        if !summary.contains('%') {
            return Err(unavailable());
        }
        lines.push(summary);
        let details = run("/usr/sbin/ioreg", &["-rn", "AppleSmartBattery"], 10)
            .map(|out| String::from_utf8_lossy(&out.stdout).to_string())
            .unwrap_or_default();
        let value = |key: &str| -> Option<u64> {
            let start = details.find(&format!("\"{key}\" = "))? + key.len() + 5;
            details[start..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .ok()
        };
        if let Some(cycles) = value("CycleCount") {
            lines.push(format!("Döngü sayısı: {cycles}"));
        }
        if let (Some(maximum), Some(design)) = (
            value("AppleRawMaxCapacity").or_else(|| value("NominalChargeCapacity")),
            value("DesignCapacity"),
        ) {
            if let Some(health) = (maximum * 100).checked_div(design) {
                lines.push(format!("Pil sağlığı: %{health}"));
            }
        }
    } else if cfg!(target_os = "linux") {
        let root = std::path::Path::new("/sys/class/power_supply");
        let battery = std::fs::read_dir(root)
            .ok()
            .and_then(|items| {
                items
                    .filter_map(|item| item.ok())
                    .find(|item| item.file_name().to_string_lossy().starts_with("BAT"))
            })
            .ok_or_else(unavailable)?
            .path();
        let read = |name: &str| {
            std::fs::read_to_string(battery.join(name))
                .map(|text| text.trim().to_string())
                .ok()
        };
        lines.push(format!(
            "Şarj: %{} ({})",
            read("capacity").unwrap_or_else(|| "?".into()),
            read("status").unwrap_or_default()
        ));
        if let Some(cycles) = read("cycle_count") {
            lines.push(format!("Döngü sayısı: {cycles}"));
        }
        let full = read("energy_full")
            .or_else(|| read("charge_full"))
            .and_then(|v| v.parse::<u64>().ok());
        let design = read("energy_full_design")
            .or_else(|| read("charge_full_design"))
            .and_then(|v| v.parse::<u64>().ok());
        if let (Some(full), Some(design)) = (full, design.filter(|design| *design > 0)) {
            lines.push(format!("Pil sağlığı: %{}", full * 100 / design));
        }
    } else {
        let out = run(
            "powershell",
            &["-NoProfile", "-Command", "Get-CimInstance Win32_Battery | ForEach-Object { \"Şarj: %$($_.EstimatedChargeRemaining) (durum $($_.BatteryStatus))\" }"],
            15,
        )
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default();
        if out.is_empty() {
            return Err(unavailable());
        }
        lines.push(out);
    }
    Ok(Done::new(
        "Pil durumu okundu.",
        Some(report("battery", &lines.join("\n"))),
    ))
}

fn arrange_windows(mode: Option<&str>) -> Result<Done> {
    if !cfg!(target_os = "macos") {
        return unsupported("Pencere dizme");
    }
    let script = r#"
tell application "System Events"
    set front to name of first application process whose frontmost is true
    set others to name of every application process whose visible is true and background only is false
end tell
set AppleScript's text item delimiters to linefeed
return front & linefeed & (others as text)
"#;
    let names =
        apple_script(script).map_err(|message| permission("ACCESSIBILITY_PERMISSION", message))?;
    let mut apps: Vec<String> = Vec::new();
    for name in names
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.starts_with("Wiqo"))
    {
        if !apps.iter().any(|app| app == name) {
            apps.push(name.to_string());
        }
    }
    let bounds = apple_script("tell application \"Finder\" to get bounds of window of desktop")
        .map_err(|message| permission("AUTOMATION_PERMISSION", message))?;
    let numbers: Vec<i64> = bounds
        .split(',')
        .filter_map(|part| part.trim().parse().ok())
        .collect();
    let [_, _, width, height] = numbers[..] else {
        return Err(tool_failure("NO_SCREEN", "Ekran bulunamadı."));
    };
    // The menu bar takes the top 25 points.
    let (top, height) = (25, height - 25);
    let place = |app: &str, x: i64, w: i64| {
        format!(
            "tell application \"System Events\" to tell process \"{}\" to tell front window to set {{position, size}} to {{{{{x}, {top}}}, {{{w}, {height}}}}}",
            app.replace('"', "")
        )
    };
    let Some(first) = apps.first().cloned() else {
        return Err(tool_failure("NO_WINDOW", "Düzenlenecek açık pencere yok."));
    };
    if mode == Some("maximize") {
        apple_script(&place(&first, 0, width))
            .map_err(|message| permission("ACCESSIBILITY_PERMISSION", message))?;
        return Ok(Done::message(format!("{first} penceresi ekranı kapladı.")));
    }
    let Some(second) = apps.get(1).cloned() else {
        return Err(tool_failure(
            "ONE_WINDOW",
            "Yan yana koymak için iki uygulama penceresi açık olmalı.",
        ));
    };
    apple_script(&place(&first, 0, width / 2))
        .map_err(|message| permission("ACCESSIBILITY_PERMISSION", message))?;
    apple_script(&place(&second, width / 2, width / 2))
        .map_err(|message| permission("ACCESSIBILITY_PERMISSION", message))?;
    Ok(Done::message(format!(
        "{first} ve {second} yan yana dizildi."
    )))
}

fn speak(text: &str) -> Result<Done> {
    if text.trim().is_empty() {
        return Err(error(
            "INVALID_PAYLOAD",
            "Seslendirilecek metin yok.",
            "validation",
        ));
    }
    let desktop = crate::fsx::desktop_dir().unwrap_or_else(crate::fsx::home);
    let stamp = chrono::Local::now().format("%Y-%m-%d %H.%M");
    let input = std::env::temp_dir().join(format!("wiqo-say-{}.txt", uuid::Uuid::new_v4()));
    std::fs::write(&input, text)
        .map_err(|_| tool_failure("SPEAK_FAILED", "Metin hazırlanamadı."))?;
    let (output, out) = if cfg!(target_os = "macos") {
        let output = crate::utility::free_path(&desktop.join(format!("Wiqo ses {stamp}.m4a")));
        let out = run(
            "/usr/bin/say",
            &[
                "-f",
                &input.to_string_lossy(),
                "-o",
                &output.to_string_lossy(),
                "--file-format=m4af",
            ],
            180,
        );
        (output, out)
    } else if cfg!(windows) {
        let output = crate::utility::free_path(&desktop.join(format!("Wiqo ses {stamp}.wav")));
        let script = format!(
            "Add-Type -AssemblyName System.Speech; $s=New-Object System.Speech.Synthesis.SpeechSynthesizer; $s.SetOutputToWaveFile('{}'); $s.Speak([IO.File]::ReadAllText('{}')); $s.Dispose()",
            output.to_string_lossy().replace('\'', "''"),
            input.to_string_lossy().replace('\'', "''")
        );
        let out = run(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", &script],
            180,
        );
        (output, out)
    } else {
        let output = crate::utility::free_path(&desktop.join(format!("Wiqo ses {stamp}.wav")));
        let out = run(
            "espeak-ng",
            &[
                "-v",
                "tr",
                "-f",
                &input.to_string_lossy(),
                "-w",
                &output.to_string_lossy(),
            ],
            180,
        );
        (output, out)
    };
    let _ = std::fs::remove_file(&input);
    if !out.is_some_and(|out| out.status.success()) || !output.exists() {
        return Err(tool_failure("SPEAK_FAILED", "Ses dosyası oluşturulamadı."));
    }
    let name = output
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(Done::new(
        format!("Ses dosyası masaüstüne kaydedildi: {name}."),
        Some(data(
            "files",
            crate::fsx::entry(&output).into_iter().collect::<Vec<_>>(),
        )),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_reports_the_computer_in_one_call() {
        let text = |action: &str| {
            let request = json!({ "kind": "systemUtility", "value": { "action": action } });
            let done = system_utility(&Context::for_tests(), &request).unwrap();
            done.data.unwrap()["value"]["text"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let overview = text("overview");
        for section in ["== Sistem ==", "== Diskler ==", "== Pil ==", "PID  %CPU"] {
            assert!(overview.contains(section), "{section} missing: {overview}");
        }
        assert!(text("systemInfo").contains("İşlemci:"));
        assert!(text("disks").contains("GB toplam"));
    }

    #[test]
    fn top_processes_lists_this_one() {
        let request =
            json!({ "kind": "systemUtility", "value": { "action": "topProcesses", "limit": 500 } });
        let done = system_utility(&Context::for_tests(), &request).unwrap();
        assert_eq!(done.message, "En çok kaynak kullanan 50 süreç.");
        let unknown = json!({ "kind": "systemUtility", "value": { "action": "reboot" } });
        assert_eq!(
            system_utility(&Context::for_tests(), &unknown)
                .unwrap_err()
                .code,
            "INVALID_PAYLOAD"
        );
    }
}
