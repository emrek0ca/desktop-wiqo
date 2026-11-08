//! The person's own Mac apps: Messages, Mail, Notes, Calendar and Reminders, driven by AppleScript (`crate::applescript`), so macOS asks once for
//! Automation permission per app.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use wiqo_core::engine::{Context, Lane};

use crate::applescript::{date_lines, from_local, literal, optional, records, script};
use crate::op::{data, error, payload, tool_failure, Blocking, Done, Result};
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
    ]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applescript::{FIELD, RECORD};
    use chrono::Local;

    #[test]
    fn a_calendar_record_becomes_an_event() {
        let moment: DateTime<Utc> = "2026-10-21T07:30:00Z".parse().unwrap();
        let local = moment
            .with_timezone(&Local)
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string();
        let text = format!(
            "id1{FIELD}Toplantı{FIELD}{local}{FIELD}{local}{FIELD}İş{FIELD}missing value{RECORD}"
        );
        let event = event_info(&records(&text, 6)[0]).unwrap();
        assert_eq!(event["title"], "Toplantı");
        assert!(event["location"].is_null());
    }
}
