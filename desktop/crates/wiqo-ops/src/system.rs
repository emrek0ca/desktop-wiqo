//! `ping` and `getSystemStatus`: is the desktop there, and what is it.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};
use sysinfo::System;
use wiqo_core::engine::{Context, Lane, Operation, Outcome};

use crate::Op;

pub(crate) fn operations() -> Vec<Op> {
    vec![Arc::new(Ping), Arc::new(GetSystemStatus)]
}

struct Ping;

#[async_trait]
impl Operation for Ping {
    fn name(&self) -> &'static str {
        "ping"
    }

    fn lane(&self) -> Lane {
        Lane::ReadOnly
    }

    async fn run(&self, _ctx: &Context, _payload: &Value) -> Outcome {
        Outcome::ok(
            "Pong.",
            json!({ "kind": "ping", "value": { "serverTime": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true) } }),
        )
    }
}

struct GetSystemStatus;

#[async_trait]
impl Operation for GetSystemStatus {
    fn name(&self) -> &'static str {
        "getSystemStatus"
    }

    fn lane(&self) -> Lane {
        Lane::ReadOnly
    }

    async fn run(&self, _ctx: &Context, _payload: &Value) -> Outcome {
        let status = tokio::task::spawn_blocking(system_status)
            .await
            .unwrap_or_else(|_| json!({ "runtimeStatus": "running" }));
        let mut message = format!(
            "{} · {}",
            status["computerName"].as_str().unwrap_or("Bilgisayar"),
            status["osName"].as_str().unwrap_or("")
        );
        if let Some(battery) = status["batteryPercentage"].as_u64() {
            message.push_str(&format!(" · pil %{battery}"));
        }
        if let (Some(free), Some(total)) = (
            status["disk"]["freeBytes"].as_u64(),
            status["disk"]["totalBytes"].as_u64(),
        ) {
            message.push_str(&format!(
                " · disk {} boş / {}",
                crate::utility::human(free),
                crate::utility::human(total)
            ));
        }
        Outcome::ok(message, json!({ "kind": "systemStatus", "value": status }))
    }
}

/// The name the person gave this computer ("Emre'nin MacBook Air", "DESKTOP-8S2D0E3"), not a network address.
fn computer_name() -> String {
    #[cfg(target_os = "macos")]
    if let Some(output) = crate::process::run_with_timeout(
        {
            let mut command = crate::process::command("/usr/sbin/scutil");
            command.args(["--get", "ComputerName"]);
            command
        },
        std::time::Duration::from_secs(3),
    ) {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    #[cfg(windows)]
    if let Ok(name) = std::env::var("COMPUTERNAME") {
        if !name.trim().is_empty() {
            return name.trim().to_string();
        }
    }
    System::host_name().unwrap_or_else(|| "Bilgisayar".into())
}

/// What the computer is and how it is doing: system, processor, memory, the system disk, battery, volume.
fn system_status() -> Value {
    let mut system = System::new();
    system.refresh_memory();
    system.refresh_cpu_usage();
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    system.refresh_cpu_usage();
    let cpus = system.cpus();
    let os_name = if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    };
    let os_version =
        System::long_os_version().unwrap_or_else(|| System::name().unwrap_or_default());
    // `macOSVersion` is the wire name from the first (Mac) companion; it carries the operating system version on every platform.
    let mut status = json!({
        "hostname": computer_name(),
        "computerName": computer_name(),
        "osName": os_name,
        "osVersion": os_version,
        "macOSVersion": os_version,
        "uptimeSeconds": System::uptime() as f64,
        "runtimeStatus": "running",
        "cpu": {
            "brand": cpus.first().map(|cpu| cpu.brand().trim().to_string()).unwrap_or_default(),
            "cores": cpus.len(),
            "usagePercent": (f64::from(system.global_cpu_usage()) * 10.0).round() / 10.0,
        },
        "memory": {
            "totalBytes": system.total_memory(),
            "usedBytes": system.used_memory(),
        },
    });
    // The disk the system runs from (the largest when none says so).
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let system_disk = disks
        .list()
        .iter()
        .find(|disk| {
            let mount = disk.mount_point().to_string_lossy().to_lowercase();
            mount == "/" || mount.starts_with("c:")
        })
        .or_else(|| disks.list().iter().max_by_key(|disk| disk.total_space()));
    if let Some(disk) = system_disk {
        status["disk"] = json!({
            "name": disk.name().to_string_lossy(),
            "mount": disk.mount_point().to_string_lossy(),
            "totalBytes": disk.total_space(),
            "freeBytes": disk.available_space(),
        });
    }
    if let Some((percentage, charging)) = battery() {
        status["batteryPercentage"] = json!(percentage);
        status["isCharging"] = json!(charging);
    }
    if let Some(volume) = crate::device::current_volume() {
        status["volumePercentage"] = json!(volume);
    }
    status
}

/// Battery level and whether it is charging; `None` on a computer without a battery or when the platform does not say.
fn battery() -> Option<(u8, bool)> {
    #[cfg(target_os = "macos")]
    {
        let output = crate::process::command("/usr/bin/pmset")
            .args(["-g", "batt"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        let percent = text
            .split('%')
            .next()?
            .rsplit(|c: char| !c.is_ascii_digit())
            .next()?
            .parse::<u8>()
            .ok()?;
        let discharging = text.contains("discharging");
        Some((percent, !discharging))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let entry = std::fs::read_dir("/sys/class/power_supply")
            .ok()?
            .flatten()
            .find(|item| item.file_name().to_string_lossy().starts_with("BAT"))?;
        let percent = std::fs::read_to_string(entry.path().join("capacity"))
            .ok()?
            .trim()
            .parse::<u8>()
            .ok()?;
        let charging = std::fs::read_to_string(entry.path().join("status"))
            .map(|status| status.trim() != "Discharging")
            .unwrap_or(false);
        Some((percent, charging))
    }
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut power: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        if unsafe { GetSystemPowerStatus(&mut power) } == 0 {
            return None;
        }
        // 128: no battery; 255: unknown.
        if power.BatteryFlag & 128 != 0 || power.BatteryLifePercent > 100 {
            return None;
        }
        Some((power.BatteryLifePercent, power.ACLineStatus == 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ping_and_status_answer_in_the_contract_shape() {
        let pong = Ping.run(&Context::for_tests(), &Value::Null).await;
        assert!(pong.error.is_none());
        assert_eq!(pong.data.unwrap()["kind"], "ping");
        let status = GetSystemStatus
            .run(&Context::for_tests(), &Value::Null)
            .await
            .data
            .unwrap();
        assert_eq!(status["kind"], "systemStatus");
        assert!(status["value"]["hostname"].is_string());
        assert!(status["value"]["uptimeSeconds"].as_f64().unwrap() > 0.0);
    }
}
