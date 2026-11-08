//! AppleScript, shared by everything that drives a Mac app through it: quoting text into a script, running one with a deadline, reading the
//! records it prints, and the one test for "macOS refused the Automation permission" every caller uses.

use std::time::Duration;

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use wiqo_core::wire::WireError;

use crate::op::{permission, tool_failure, Result};
use crate::process::run_with_timeout;

/// macOS refused to let Wiqo drive the app (Automation permission): error -1743, or its wording.
pub(crate) fn refused(stderr: &str) -> bool {
    stderr.contains("-1743") || stderr.contains("Not authorized") || stderr.contains("not allowed")
}

pub(crate) const FIELD: char = '\u{1f}';
pub(crate) const RECORD: char = '\u{1e}';

pub(crate) fn literal(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace("\r\n", "\\n")
        .replace(['\r', '\n'], "\\n");
    format!("\"{escaped}\"")
}

/// Runs AppleScript. A refused Automation permission becomes `<prefix>_PERMISSION_DENIED` naming the app; anything else `<prefix>_<failed>`.
pub(crate) fn script(
    source: &str,
    seconds: u64,
    prefix: &str,
    app: &str,
    failed: (&str, &str),
) -> Result<String> {
    let mut command = crate::process::command("/usr/bin/osascript");
    command.args(["-e", source]);
    let Some(out) = run_with_timeout(command, Duration::from_secs(seconds)) else {
        return Err(WireError::new(
            &format!("{prefix}_TIMEOUT"),
            format!("{app} zamanında yanıt vermedi."),
            "timeout",
        ));
    };
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_string());
    }
    if refused(&String::from_utf8_lossy(&out.stderr)) {
        return Err(permission(
            &format!("{prefix}_PERMISSION_DENIED"),
            format!("Mac'te Wiqo'nun {app} izni yok; bilgisayardaki Wiqo penceresinde İzinler > Uygulamaları kontrol'e tek tıkla izin ver."),
        ));
    }
    Err(tool_failure(&format!("{prefix}_{}", failed.0), failed.1))
}

pub(crate) fn records(text: &str, fields: usize) -> Vec<Vec<String>> {
    text.split(RECORD)
        .filter(|record| !record.trim().is_empty())
        .map(|record| {
            record
                .split(FIELD)
                .map(|field| field.trim().to_string())
                .collect::<Vec<_>>()
        })
        .filter(|record| record.len() >= fields)
        .collect()
}

/// An AppleScript date as `«class isot»` text ("2026-10-21T10:00:00", local time) → the contract's UTC ISO 8601.
pub(crate) fn from_local(text: &str) -> Option<String> {
    let naive = NaiveDateTime::parse_from_str(text.trim(), "%Y-%m-%dT%H:%M:%S").ok()?;
    let local = Local.from_local_datetime(&naive).earliest()?;
    Some(
        local
            .with_timezone(&Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

/// AppleScript lines that set `variable` to the given moment, in local time (the language has no date literal that is safe across locales).
pub(crate) fn date_lines(variable: &str, moment: DateTime<Utc>) -> String {
    let local = moment.with_timezone(&Local);
    use chrono::{Datelike, Timelike};
    format!(
        "set {variable} to current date\nset day of {variable} to 1\nset year of {variable} to {}\nset month of {variable} to {}\nset day of {variable} to {}\nset hours of {variable} to {}\nset minutes of {variable} to {}\nset seconds of {variable} to 0\n",
        local.year(),
        local.month(),
        local.day(),
        local.hour(),
        local.minute()
    )
}

pub(crate) fn optional(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text != "missing value").then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_dates_round_trip() {
        assert_eq!(literal("a \"b\"\nc\\"), "\"a \\\"b\\\"\\nc\\\\\"");
        let moment: DateTime<Utc> = "2026-10-21T07:30:00Z".parse().unwrap();
        let local = moment
            .with_timezone(&Local)
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string();
        assert_eq!(from_local(&local).as_deref(), Some("2026-10-21T07:30:00Z"));
        assert!(date_lines("d", moment).contains("set minutes of d to 30"));
        let text = format!("a{FIELD}b{RECORD}c{RECORD}");
        assert_eq!(
            records(&text, 2),
            vec![vec!["a".to_string(), "b".to_string()]]
        );
        assert!(refused(
            "execution error: Not authorized to send Apple events (-1743)"
        ));
        assert!(!refused("syntax error"));
    }
}
