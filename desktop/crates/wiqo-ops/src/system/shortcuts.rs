//! Wiqo's own Shortcuts ("Wiqo Wi-Fi Aç" and the like): only those run, and the state they change is read back to say whether it worked.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use wiqo_core::engine::{Context, Lane};
use wiqo_core::wire::WireError;

use crate::op::{data, error, payload, permission, tool_failure, Blocking, Done, Result};
use crate::process::run_with_timeout;
use crate::Op;

pub(crate) fn operations() -> Vec<Op> {
    vec![Arc::new(Blocking {
        name: "runShortcut",
        lane: Lane::SideEffect,
        run: run_shortcut,
    })]
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Shortcut {
    shortcut: String,
    #[serde(default = "default_shortcut_seconds")]
    timeout_seconds: u64,
}

fn default_shortcut_seconds() -> u64 {
    15
}

fn run(program: &str, args: &[&str], seconds: u64) -> String {
    let mut command = crate::process::command(program);
    command.args(args);
    run_with_timeout(command, Duration::from_secs(seconds))
        .map(|out| String::from_utf8_lossy(&out.stdout).to_lowercase())
        .unwrap_or_default()
}

fn wifi_state() -> Option<&'static str> {
    let ports = run("/usr/sbin/networksetup", &["-listallhardwareports"], 3);
    let lines: Vec<&str> = ports.lines().map(str::trim).collect();
    let index = lines
        .iter()
        .position(|line| *line == "hardware port: wi-fi")?;
    let device = lines[index + 1..]
        .iter()
        .find(|line| line.starts_with("device:"))?
        .trim_start_matches("device:")
        .trim()
        .to_string();
    let power = run("/usr/sbin/networksetup", &["-getairportpower", &device], 3);
    if power.contains("on") {
        Some("on")
    } else if power.contains("off") {
        Some("off")
    } else {
        None
    }
}

fn bluetooth_state() -> Option<&'static str> {
    let output = run(
        "/usr/sbin/system_profiler",
        &["SPBluetoothDataType", "-json"],
        5,
    )
    .replace(' ', "");
    if output.contains("\"controller_power_state\":1")
        || output.contains("\"controller_state\":\"attrib_on\"")
    {
        Some("on")
    } else if output.contains("\"controller_power_state\":0")
        || output.contains("\"controller_state\":\"attrib_off\"")
    {
        Some("off")
    } else {
        None
    }
}

/// What the shortcut's output says about the Focus it changed (the same markers the Mac app reads).
fn focus_state(name: &str, output: &str) -> Option<&'static str> {
    let turning_off = name.ends_with("Kapat");
    if [
        "off",
        "kapali",
        "none",
        "no focus",
        "no active focus",
        "inactive",
        "devre disi",
    ]
    .iter()
    .any(|marker| output.contains(marker))
    {
        return Some("off");
    }
    let targets: &[&str] = if name.contains("Rahatsız") {
        &["rahatsiz etme", "do not disturb", "default"]
    } else if name.contains("Kişisel") {
        &["kisisel", "personal", "personal-time"]
    } else {
        &["is", "work"]
    };
    if turning_off && targets.iter().any(|marker| output.contains(marker)) {
        return Some("on");
    }
    let all = [
        "rahatsiz etme",
        "do not disturb",
        "default",
        "kisisel",
        "personal",
        "personal-time",
        "is",
        "work",
    ];
    if turning_off && all.iter().any(|marker| output.contains(marker)) {
        return Some("off");
    }
    if targets.iter().any(|marker| output.contains(marker))
        || output.contains("on")
        || output.contains("acik")
        || output.contains("enabled")
    {
        return Some("on");
    }
    None
}

fn run_shortcut(_ctx: &Context, value: &Value) -> Result<Done> {
    let request: Shortcut = payload(value, "shortcut", "kestirme")?;
    let name = request.shortcut.clone();
    if !name.starts_with("Wiqo ") {
        return Err(error(
            "INVALID_PAYLOAD",
            "Yalnızca Wiqo'nun kurduğu kestirmeler çalıştırılabilir.",
            "validation",
        ));
    }
    let mut command = crate::process::command("/usr/bin/shortcuts");
    command.args(["run", &name, "--output-type", "public.plain-text"]);
    let Some(out) = run_with_timeout(
        command,
        Duration::from_secs(request.timeout_seconds.clamp(1, 55)),
    ) else {
        return Err(WireError::new(
            "SHORTCUT_TIMEOUT",
            "Shortcut çalıştırma süresi doldu.",
            "timeout",
        ));
    };
    let output = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    if !out.status.success() {
        if ["not authorized", "not permitted", "permission"]
            .iter()
            .any(|marker| stderr.contains(marker))
        {
            return Err(permission(
                "SHORTCUT_PERMISSION_REQUIRED",
                format!("{name} için Wiqo'nun Shortcuts/Otomasyon iznini açın."),
            ));
        }
        let code = if stderr.contains("not found") || stderr.contains("bulunamadı") {
            "SHORTCUT_NOT_INSTALLED"
        } else {
            "SHORTCUT_FAILED"
        };
        return Err(tool_failure(
            code,
            format!(
                "{name} çalıştırılamadı. Kestirmenin Wiqo tarafından kurulduğunu kontrol edin."
            ),
        ));
    }
    let expected = if name.ends_with("Aç") { "on" } else { "off" };
    let folded = wiqo_contract::names::fold(&output);
    let observed = if name.contains("Wi-Fi") {
        wifi_state()
    } else if name.contains("Bluetooth") {
        bluetooth_state()
    } else {
        focus_state(&name, &folded)
    };
    let verified = observed == Some(expected);
    let content = json!({ "shortcut": name, "output": output.chars().take(4_000).collect::<String>(), "expectedState": expected, "observedState": observed, "verified": verified });
    if !verified {
        return Ok(Done::partial(
            tool_failure(
                "SHORTCUT_VERIFICATION_FAILED",
                format!("{name} çalıştı ancak istenen durum doğrulanamadı."),
            ),
            data("shortcutResult", content),
        ));
    }
    Ok(Done::new(
        format!("{name} çalıştı."),
        Some(data("shortcutResult", content)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_wiqo_shortcuts_run() {
        let request = json!({ "kind": "shortcut", "value": { "shortcut": "Delete Everything" } });
        assert_eq!(
            run_shortcut(&Context::for_tests(), &request)
                .unwrap_err()
                .code,
            "INVALID_PAYLOAD"
        );
    }
}
