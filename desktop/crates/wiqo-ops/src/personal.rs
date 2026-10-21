//! The person's own Mac apps: Messages, Mail, Notes, Calendar, Reminders and Shortcuts (driven by AppleScript, so macOS asks once for Automation
//! permission per app), plus display brightness, the clipboard history and Word/Excel/PDF documents written to the Desktop.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use wiqo_contract::is_valid_file_name;
use wiqo_core::engine::{Context, Lane};
use wiqo_core::wire::WireError;

use crate::op::{data, error, payload, permission, tool_failure, Blocking, Done, Result};
use crate::process::run_with_timeout;
use crate::Op;

pub(crate) fn operations() -> Vec<Op> {
    let (read, side) = (Lane::ReadOnly, Lane::SideEffect);
    let op = |name: &'static str, lane: Lane, run: fn(&Context, &Value) -> Result<Done>| -> Op {
        Arc::new(Blocking { name, lane, run })
    };
    vec![
        op("sendMessage", side, send_message),
        op("sendEmail", side, send_email),
        op("createNote", side, create_note),
        op("listCalendarEvents", read, |_, _| list_calendar_events()),
        op("createCalendarEvent", side, create_calendar_event),
        op("listReminders", read, |_, _| list_reminders()),
        op("createReminder", side, create_reminder),
        op("completeReminder", side, complete_reminder),
        op("runShortcut", side, run_shortcut),
        op("setBrightness", side, set_brightness),
        op("getClipboardHistory", read, |_, _| clipboard_history()),
        op("editDocument", side, edit_document),
    ]
}

// ----- AppleScript -----

const FIELD: char = '\u{1f}';
const RECORD: char = '\u{1e}';

fn literal(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace("\r\n", "\\n")
        .replace(['\r', '\n'], "\\n");
    format!("\"{escaped}\"")
}

/// Runs AppleScript. A refused Automation permission becomes `<prefix>_PERMISSION_DENIED` naming the app; anything else `<prefix>_<failed>`.
fn script(
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
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("-1743")
        || stderr.contains("Not authorized")
        || stderr.contains("not allowed")
    {
        return Err(permission(
            &format!("{prefix}_PERMISSION_DENIED"),
            format!("Mac'te Wiqo'nun {app} izni yok; bilgisayardaki Wiqo penceresinde İzinler > Uygulamaları kontrol'e tek tıkla izin ver."),
        ));
    }
    Err(tool_failure(&format!("{prefix}_{}", failed.0), failed.1))
}

fn records(text: &str, fields: usize) -> Vec<Vec<String>> {
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
fn from_local(text: &str) -> Option<String> {
    let naive = NaiveDateTime::parse_from_str(text.trim(), "%Y-%m-%dT%H:%M:%S").ok()?;
    let local = Local.from_local_datetime(&naive).earliest()?;
    Some(
        local
            .with_timezone(&Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

/// AppleScript lines that set `variable` to the given moment, in local time (the language has no date literal that is safe across locales).
fn date_lines(variable: &str, moment: DateTime<Utc>) -> String {
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

fn optional(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text != "missing value").then(|| text.to_string())
}

// ----- messages, mail, notes -----

#[derive(Deserialize)]
struct Message {
    recipient: String,
    body: String,
}

fn send_message(_ctx: &Context, value: &Value) -> Result<Done> {
    let message: Message = payload(value, "message", "mesaj")?;
    if message.recipient.trim().is_empty() || message.body.trim().is_empty() {
        return Err(error(
            "INVALID_PAYLOAD",
            "Alıcı veya mesaj boş.",
            "validation",
        ));
    }
    let source = format!(
        "tell application \"Messages\"\nset targetService to 1st service whose service type = iMessage\nset targetBuddy to buddy {} of targetService\nsend {} to targetBuddy\nend tell",
        literal(&message.recipient),
        literal(&message.body)
    );
    script(
        &source,
        20,
        "MESSAGE",
        "Mesajlar",
        ("SEND_FAILED", "Mesaj gönderilemedi."),
    )?;
    Ok(Done::message(format!(
        "Mesaj {} kişisine gönderildi.",
        message.recipient
    )))
}

#[derive(Deserialize)]
struct Email {
    to: String,
    subject: String,
    body: String,
}

fn send_email(_ctx: &Context, value: &Value) -> Result<Done> {
    let email: Email = payload(value, "email", "e-posta")?;
    if !email.to.contains('@') {
        return Err(error(
            "INVALID_PAYLOAD",
            "Alıcı e-posta adresi geçersiz.",
            "validation",
        ));
    }
    let source = format!(
        "tell application \"Mail\"\nset newMessage to make new outgoing message with properties {{subject:{}, content:{}, visible:false}}\ntell newMessage\nmake new to recipient at end of to recipients with properties {{address:{}}}\nsend\nend tell\nend tell",
        literal(&email.subject),
        literal(&email.body),
        literal(&email.to)
    );
    script(
        &source,
        30,
        "EMAIL",
        "Mail",
        ("SEND_FAILED", "E-posta gönderilemedi."),
    )?;
    Ok(Done::message(format!(
        "E-posta {} adresine gönderildi.",
        email.to
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Note {
    title: String,
    body: String,
    folder_name: Option<String>,
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn create_note(_ctx: &Context, value: &Value) -> Result<Done> {
    let note: Note = payload(value, "note", "not")?;
    // Notes bodies are HTML: one <div> per line keeps the line breaks, and the title leads the body the way the Notes app writes it.
    let body: String = std::iter::once(note.title.as_str())
        .chain(note.body.split('\n'))
        .map(|line| format!("<div>{}</div>", html_escape(line)))
        .collect();
    let properties = format!("{{name:{}, body:{}}}", literal(&note.title), literal(&body));
    let source = match note.folder_name.as_deref().filter(|name| !name.is_empty()) {
        Some(folder) => format!(
            "tell application \"Notes\" to make new note at folder {} with properties {properties}",
            literal(folder)
        ),
        None => format!("tell application \"Notes\" to make new note with properties {properties}"),
    };
    let failed = if note.folder_name.is_some() {
        "Not kaydedilemedi; klasör adını kontrol edin."
    } else {
        "Not kaydedilemedi."
    };
    script(&source, 20, "NOTE", "Notlar", ("SAVE_FAILED", failed))?;
    Ok(Done::message(format!(
        "\"{}\" notu oluşturuldu.",
        note.title
    )))
}

// ----- calendar -----

fn event_info(fields: &[String]) -> Option<Value> {
    Some(json!({
        "identifier": fields[0], "title": optional(&fields[1]).unwrap_or_else(|| "(Başlıksız)".into()),
        "startAt": from_local(&fields[2])?, "endAt": from_local(&fields[3])?, "calendarName": fields[4], "location": optional(&fields[5]),
    }))
}

const EVENT_FIELDS: &str = "(uid of e) & f & (summary of e) & f & ((start date of e) as «class isot» as string) & f & ((end date of e) as «class isot» as string) & f & (name of c) & f & loc & r";

fn list_calendar_events() -> Result<Done> {
    let source = format!(
        "set f to character id 31\nset r to character id 30\nset out to \"\"\nset now to current date\nset later to now + 14 * days\n\
         tell application \"Calendar\"\nrepeat with c in calendars\ntry\nset evs to (every event of c whose start date ≥ now and start date ≤ later)\n\
         repeat with e in evs\nset loc to \"\"\ntry\nset loc to (location of e) as string\nend try\nset out to out & {EVENT_FIELDS}\nend repeat\nend try\nend repeat\nend tell\nreturn out"
    );
    let text = script(
        &source,
        50,
        "CALENDAR",
        "Takvim",
        ("READ_FAILED", "Takvim okunamadı."),
    )?;
    let mut events: Vec<Value> = records(&text, 6)
        .iter()
        .filter_map(|fields| event_info(fields))
        .collect();
    events.sort_by(|a, b| a["startAt"].as_str().cmp(&b["startAt"].as_str()));
    events.truncate(100);
    let message = if events.is_empty() {
        "Önümüzdeki 14 günde etkinlik yok.".to_string()
    } else {
        format!("Önümüzdeki 14 günde {} etkinlik.", events.len())
    };
    Ok(Done::new(message, Some(data("calendarEvents", events))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    title: String,
    start_at: DateTime<Utc>,
    end_at: DateTime<Utc>,
    calendar_name: Option<String>,
    notes: Option<String>,
    location: Option<String>,
}

fn create_calendar_event(_ctx: &Context, value: &Value) -> Result<Done> {
    let event: Event = payload(value, "calendarEvent", "takvim etkinliği")?;
    if event.end_at < event.start_at {
        return Err(error(
            "INVALID_PAYLOAD",
            "Etkinliğin bitişi başlangıcından önce olamaz.",
            "validation",
        ));
    }
    let target = match event
        .calendar_name
        .as_deref()
        .filter(|name| !name.is_empty())
    {
        Some(name) => format!("set c to first calendar whose name is {}", literal(name)),
        // Calendar has no "default calendar" in AppleScript: the first one that can be written to.
        None => "set c to first calendar whose writable is true".into(),
    };
    let mut properties = format!(
        "{{summary:{}, start date:startAt, end date:endAt",
        literal(&event.title)
    );
    if let Some(notes) = event.notes.as_deref().filter(|text| !text.is_empty()) {
        properties.push_str(&format!(", description:{}", literal(notes)));
    }
    if let Some(location) = event.location.as_deref().filter(|text| !text.is_empty()) {
        properties.push_str(&format!(", location:{}", literal(location)));
    }
    properties.push('}');
    let source = format!(
        "set f to character id 31\nset r to character id 30\n{}{}tell application \"Calendar\"\ntry\n{target}\non error\nerror \"NO_CALENDAR\"\nend try\n\
         set e to make new event at end of events of c with properties {properties}\nset loc to \"\"\ntry\nset loc to (location of e) as string\nend try\nreturn {EVENT_FIELDS}\nend tell",
        date_lines("startAt", event.start_at),
        date_lines("endAt", event.end_at)
    );
    let text = script(
        &source,
        30,
        "CALENDAR",
        "Takvim",
        (
            "SAVE_FAILED",
            "Takvim etkinliği kaydedilemedi; takvim adını kontrol edin.",
        ),
    )?;
    let info = records(&text, 6)
        .first()
        .and_then(|fields| event_info(fields))
        .ok_or_else(|| tool_failure("CALENDAR_SAVE_FAILED", "Etkinlik kaydı doğrulanamadı."))?;
    Ok(Done::new(
        format!("\"{}\" takvime eklendi.", event.title),
        Some(data("calendarEvents", vec![info])),
    ))
}

// ----- reminders -----

const REMINDER_FIELDS: &str = "(id of m) & f & (name of m) & f & due & f & body & f & (name of l) & f & ((completed of m) as string) & r";
const REMINDER_PARTS: &str = "set due to \"\"\ntry\nset due to (due date of m) as «class isot» as string\nend try\nset body to \"\"\ntry\nset body to (body of m) as string\nend try\n";

fn reminder_info(fields: &[String]) -> Value {
    json!({
        "identifier": fields[0], "title": optional(&fields[1]).unwrap_or_else(|| "(Başlıksız)".into()), "dueAt": from_local(&fields[2]),
        "notes": optional(&fields[3]), "listName": fields[4], "isCompleted": fields[5] == "true",
    })
}

fn list_reminders() -> Result<Done> {
    let source = format!(
        "set f to character id 31\nset r to character id 30\nset out to \"\"\nset cutoff to (current date) + 30 * days\ntell application \"Reminders\"\n\
         repeat with l in lists\nrepeat with m in (every reminder of l whose completed is false)\n{REMINDER_PARTS}\
         set keep to true\ntry\nif (due date of m) > cutoff then set keep to false\nend try\nif keep then set out to out & {REMINDER_FIELDS}\nend repeat\nend repeat\nend tell\nreturn out"
    );
    let text = script(
        &source,
        50,
        "REMINDERS",
        "Anımsatıcılar",
        ("READ_FAILED", "Hatırlatıcılar okunamadı."),
    )?;
    let mut reminders: Vec<Value> = records(&text, 6)
        .iter()
        .map(|fields| reminder_info(fields))
        .collect();
    // Due ones first by date, then the ones without a date.
    reminders.sort_by(|a, b| match (a["dueAt"].as_str(), b["dueAt"].as_str()) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    reminders.truncate(100);
    Ok(Done::new(
        format!("{} açık hatırlatıcı.", reminders.len()),
        Some(data("reminders", reminders)),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reminder {
    title: String,
    due_at: Option<DateTime<Utc>>,
    notes: Option<String>,
    list_name: Option<String>,
}

fn create_reminder(_ctx: &Context, value: &Value) -> Result<Done> {
    let reminder: Reminder = payload(value, "reminder", "hatırlatıcı")?;
    let list = match reminder
        .list_name
        .as_deref()
        .filter(|name| !name.is_empty())
    {
        Some(name) => format!("set l to list {}", literal(name)),
        None => "set l to default list".into(),
    };
    let mut properties = format!("{{name:{}", literal(&reminder.title));
    if let Some(notes) = reminder.notes.as_deref().filter(|text| !text.is_empty()) {
        properties.push_str(&format!(", body:{}", literal(notes)));
    }
    if reminder.due_at.is_some() {
        properties.push_str(", due date:dueAt");
    }
    properties.push('}');
    let source = format!(
        "set f to character id 31\nset r to character id 30\n{}tell application \"Reminders\"\n{list}\nset m to make new reminder at end of reminders of l with properties {properties}\n{REMINDER_PARTS}return {REMINDER_FIELDS}\nend tell",
        reminder.due_at.map(|due| date_lines("dueAt", due)).unwrap_or_default()
    );
    let text = script(
        &source,
        30,
        "REMINDERS",
        "Anımsatıcılar",
        (
            "SAVE_FAILED",
            "Hatırlatıcı kaydedilemedi; liste adını kontrol edin.",
        ),
    )?;
    let info = records(&text, 6)
        .first()
        .map(|fields| reminder_info(fields))
        .ok_or_else(|| tool_failure("REMINDER_SAVE_FAILED", "Hatırlatıcı kaydı doğrulanamadı."))?;
    Ok(Done::new(
        format!("\"{}\" hatırlatıcısı eklendi.", reminder.title),
        Some(data("reminders", vec![info])),
    ))
}

#[derive(Deserialize)]
struct Complete {
    identifier: String,
}

fn complete_reminder(_ctx: &Context, value: &Value) -> Result<Done> {
    let request: Complete = payload(value, "completeReminder", "hatırlatıcı")?;
    let source = format!(
        "set f to character id 31\nset r to character id 30\ntell application \"Reminders\"\ntry\nset m to first reminder whose id is {}\non error\nerror \"NOT_FOUND\"\nend try\n\
         set completed of m to true\nset l to container of m\n{REMINDER_PARTS}return {REMINDER_FIELDS}\nend tell",
        literal(&request.identifier)
    );
    let text = script(
        &source,
        30,
        "REMINDER",
        "Anımsatıcılar",
        (
            "NOT_FOUND",
            "Hatırlatıcı bulunamadı; listeyi yeniden alıp tekrar dene.",
        ),
    )?;
    let info = records(&text, 6)
        .first()
        .map(|fields| reminder_info(fields))
        .ok_or_else(|| tool_failure("REMINDER_SAVE_FAILED", "Hatırlatıcı güncellenemedi."))?;
    Ok(Done::new(
        "Hatırlatıcı tamamlandı.",
        Some(data("reminders", vec![info])),
    ))
}

// ----- shortcuts -----

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

// ----- brightness -----

fn set_brightness(_ctx: &Context, value: &Value) -> Result<Done> {
    let level: i64 = payload(value, "brightness", "parlaklık")?;
    let level = level.clamp(0, 100);
    let unavailable = || {
        tool_failure(
            "BRIGHTNESS_UNAVAILABLE",
            "Ekran parlaklığı bu bilgisayarda desteklenmiyor.",
        )
    };
    #[cfg(target_os = "macos")]
    {
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGMainDisplayID() -> u32;
        }
        type SetBrightness = unsafe extern "C" fn(u32, f32) -> i32;
        let library = unsafe {
            libloading::Library::new(
                "/System/Library/PrivateFrameworks/DisplayServices.framework/DisplayServices",
            )
        }
        .map_err(|_| unavailable())?;
        let set: libloading::Symbol<SetBrightness> =
            unsafe { library.get(b"DisplayServicesSetBrightness") }.map_err(|_| unavailable())?;
        if unsafe { set(CGMainDisplayID(), level as f32 / 100.0) } != 0 {
            return Err(tool_failure("BRIGHTNESS_FAILED", "Parlaklık ayarlanamadı. Harici ekranlarda veya bazı Mac modellerinde desteklenmiyor olabilir."));
        }
        Ok(Done::message(format!("Parlaklık %{level} yapıldı.")))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = level;
        Err(unavailable())
    }
}

// ----- clipboard history -----

struct History {
    entries: Mutex<Vec<Value>>,
}

/// The last 20 texts copied while Wiqo runs, newest first (kept in memory only). Started by the first request, so nothing is watched unless asked.
fn history() -> &'static History {
    static HISTORY: OnceLock<History> = OnceLock::new();
    HISTORY.get_or_init(|| {
        std::thread::spawn(|| {
            let mut last: Option<String> = None;
            loop {
                if let Ok(text) = arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
                    if !text.is_empty() && last.as_deref() != Some(text.as_str()) {
                        last = Some(text.clone());
                        let mut entries = history().entries.lock().expect("history lock");
                        if entries.first().and_then(|entry| entry["text"].as_str()) != Some(text.as_str()) {
                            let text: String = text.chars().take(4_000).collect();
                            entries.insert(0, json!({ "text": text, "copiedAt": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true) }));
                            entries.truncate(20);
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(1500));
            }
        });
        History { entries: Mutex::new(Vec::new()) }
    })
}

/// Starts watching the clipboard when the app starts (the history is about what the person copies, not what was on the clipboard at the request).
pub fn start_clipboard_history() {
    let _ = history();
}

fn clipboard_history() -> Result<Done> {
    let entries = history().entries.lock().expect("history lock").clone();
    let message = if entries.is_empty() {
        "Pano geçmişi boş.".to_string()
    } else {
        format!("Panoda son {} kayıt.", entries.len())
    };
    Ok(Done::new(message, Some(data("clipboardHistory", entries))))
}

// ----- documents on the Desktop -----

#[derive(Deserialize)]
struct Document {
    filename: String,
    format: String,
    content: String,
    #[serde(default)]
    overwrite: bool,
}

fn edit_document(ctx: &Context, value: &Value) -> Result<Done> {
    let document: Document = payload(value, "document", "belge")?;
    if !is_valid_file_name(&document.filename) {
        return Err(tool_failure(
            "INVALID_FILENAME",
            "Belge dosya adı geçersiz.",
        ));
    }
    if document.content.is_empty() || document.content.chars().count() > 200_000 {
        return Err(tool_failure(
            "INVALID_DOCUMENT_CONTENT",
            "Belge içeriği boş veya desteklenen sınırın dışında.",
        ));
    }
    let format = document.format.to_lowercase();
    if !["doc", "docx", "xlsx", "pdf", "md", "txt", "csv", "html"].contains(&format.as_str()) {
        return Err(error(
            "INVALID_PAYLOAD",
            "Belge biçimi desteklenmiyor.",
            "validation",
        ));
    }
    let desktop = crate::fsx::desktop_dir()
        .filter(|path| path.is_dir())
        .ok_or_else(|| tool_failure("DESKTOP_UNAVAILABLE", "Masaüstü klasörüne erişilemedi."))?;
    let name = if document
        .filename
        .to_lowercase()
        .ends_with(&format!(".{format}"))
    {
        document.filename.clone()
    } else {
        format!("{}.{format}", document.filename)
    };
    let destination = desktop.join(&name);
    if destination.parent() != Some(desktop.as_path()) || crate::fsx::is_symlink(&destination) {
        return Err(tool_failure(
            "INVALID_FILENAME",
            "Belge dosya adı geçersiz.",
        ));
    }
    if destination.exists() && !document.overwrite {
        return Err(tool_failure(
            "FILE_EXISTS",
            "Aynı adlı dosya zaten var; üzerine yazılmadı.",
        ));
    }
    let scratch = std::env::temp_dir().join(format!("wiqo-document-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&scratch)
        .map_err(|_| tool_failure("DOCUMENT_WRITE_FAILED", "Belge yazılamadı."))?;
    let produced = render(&format, &document.content, &scratch);
    let bytes = produced.and_then(|path| {
        std::fs::read(path)
            .map_err(|_| tool_failure("DOCUMENT_WRITE_FAILED", "Belge çıktısı oluşturulamadı."))
    });
    let _ = std::fs::remove_dir_all(&scratch);
    let bytes = bytes?;
    if bytes.is_empty() {
        return Err(tool_failure("DOCUMENT_WRITE_FAILED", "Belge çıktısı boş."));
    }
    let before = if destination.exists() {
        crate::files::back_up(ctx, &destination)?;
        std::fs::read(&destination).ok()
    } else {
        None
    };
    crate::files::write_atomic(&destination, &bytes)
        .map_err(|_| tool_failure("DOCUMENT_WRITE_FAILED", "Belge yazılamadı."))?;
    if std::fs::read(&destination).ok().as_deref() != Some(bytes.as_slice()) {
        return Err(tool_failure(
            "FILE_VERIFICATION_FAILED",
            "Belge yazıldı ancak içerik doğrulanamadı; otomatik tekrar yapılmadı.",
        ));
    }
    let undo_id =
        crate::undo::register(ctx, &destination.to_string_lossy(), before.as_deref(), None);
    let mut item = crate::fsx::entry(&destination)
        .ok_or_else(|| tool_failure("FILE_VERIFICATION_FAILED", "Belge doğrulanamadı."))?;
    item.undo_id = undo_id;
    Ok(Done::new(
        format!("{name} masaüstüne kaydedildi."),
        Some(data("files", vec![item])),
    ))
}

fn render(format: &str, content: &str, scratch: &std::path::Path) -> Result<std::path::PathBuf> {
    let failed = |message: &str| tool_failure("DOCUMENT_CONVERSION_FAILED", message);
    let source = scratch.join("source.txt");
    std::fs::write(&source, content).map_err(|_| failed("Belge hazırlanamadı."))?;
    let ok = |program: &str, args: &[&str], seconds: u64| {
        let mut command = crate::process::command(program);
        command.args(args);
        run_with_timeout(command, Duration::from_secs(seconds)).filter(|out| out.status.success())
    };
    match format {
        "txt" | "md" | "csv" | "html" => Ok(source),
        "doc" | "docx" => {
            let output = scratch.join(format!("out.{format}"));
            ok(
                "/usr/bin/textutil",
                &[
                    "-convert",
                    format,
                    "-output",
                    &output.to_string_lossy(),
                    &source.to_string_lossy(),
                ],
                60,
            )
            .ok_or_else(|| failed("Word belgesi dönüştürülemedi."))?;
            Ok(output)
        }
        "pdf" => {
            let out = ok(
                "/usr/sbin/cupsfilter",
                &["-m", "application/pdf", &source.to_string_lossy()],
                60,
            )
            .filter(|out| !out.stdout.is_empty())
            .ok_or_else(|| failed("PDF oluşturulamadı."))?;
            let output = scratch.join("out.pdf");
            std::fs::write(&output, out.stdout).map_err(|_| failed("PDF yazılamadı."))?;
            Ok(output)
        }
        _ => {
            // A spreadsheet from rows: "|" or tab separated text becomes CSV, which LibreOffice turns into a workbook.
            let csv: String = content
                .lines()
                .map(|line| {
                    line.split(['|', '\t'])
                        .map(|cell| format!("\"{}\"", cell.trim().replace('"', "\"\"")))
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .collect::<Vec<_>>()
                .join("\n");
            let table = scratch.join("source.csv");
            std::fs::write(&table, csv).map_err(|_| failed("Tablo hazırlanamadı."))?;
            let office = [
                "/Applications/LibreOffice.app/Contents/MacOS/soffice",
                "/opt/homebrew/bin/soffice",
                "/usr/local/bin/soffice",
            ]
            .into_iter()
            .find(|path| std::path::Path::new(path).is_file())
            .ok_or_else(|| {
                tool_failure(
                    "DOCUMENT_ENGINE_UNAVAILABLE",
                    "Excel belgesi yazmak için LibreOffice kurulmalı.",
                )
            })?;
            ok(
                office,
                &[
                    "--headless",
                    "--convert-to",
                    "xlsx",
                    "--outdir",
                    &scratch.to_string_lossy(),
                    &table.to_string_lossy(),
                ],
                120,
            )
            .ok_or_else(|| failed("Excel belgesi dönüştürülemedi."))?;
            Ok(scratch.join("source.xlsx"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applescript_text_and_dates_round_trip() {
        assert_eq!(literal("a \"b\"\nc\\"), "\"a \\\"b\\\"\\nc\\\\\"");
        let moment: DateTime<Utc> = "2026-10-21T07:30:00Z".parse().unwrap();
        let local = moment
            .with_timezone(&Local)
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string();
        assert_eq!(from_local(&local).as_deref(), Some("2026-10-21T07:30:00Z"));
        assert!(date_lines("d", moment).contains("set minutes of d to 30"));
        let text = format!(
            "id1{FIELD}Toplantı{FIELD}{local}{FIELD}{local}{FIELD}İş{FIELD}missing value{RECORD}"
        );
        let event = event_info(&records(&text, 6)[0]).unwrap();
        assert_eq!(event["title"], "Toplantı");
        assert!(event["location"].is_null());
    }

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
