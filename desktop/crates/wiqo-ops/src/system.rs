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
        // `macOSVersion` is the wire name from the first (Mac) companion; it carries the operating system version on every platform.
        let mut status = json!({
            "hostname": System::host_name().unwrap_or_else(|| "Bilgisayar".into()),
            "macOSVersion": System::long_os_version().unwrap_or_else(|| System::name().unwrap_or_default()),
            "uptimeSeconds": System::uptime() as f64,
            "runtimeStatus": "running",
        });
        if let Some((percentage, charging)) = battery() {
            status["batteryPercentage"] = json!(percentage);
            status["isCharging"] = json!(charging);
        }
        if let Some(volume) = crate::device::current_volume() {
            status["volumePercentage"] = json!(volume);
        }
        Outcome::ok(
            "Sistem durumu alındı.",
            json!({ "kind": "systemStatus", "value": status }),
        )
    }
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
        None
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
