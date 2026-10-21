//! The computer itself: power, sound, media keys, the clipboard and web addresses. What the platforms can do differs; each function says what it needs and
//! answers with a contract error code when the platform or a permission does not allow it.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use wiqo_core::engine::{Context, Lane};

#[cfg(target_os = "macos")]
use crate::op::permission;
use crate::op::{data, error, payload, tool_failure, Blocking, Done, Result};
use crate::process::run_with_timeout;
use crate::Op;

pub(crate) fn operations() -> Vec<Op> {
    let side = Lane::SideEffect;
    vec![
        Arc::new(Blocking {
            name: "lockMac",
            lane: side,
            run: |_, _| power(Power::Lock),
        }),
        Arc::new(Blocking {
            name: "sleepMac",
            lane: side,
            run: |_, _| power(Power::Sleep),
        }),
        Arc::new(Blocking {
            name: "displaySleep",
            lane: side,
            run: |_, _| power(Power::Display),
        }),
        Arc::new(Blocking {
            name: "setVolume",
            lane: side,
            run: set_volume,
        }),
        Arc::new(Blocking {
            name: "toggleMute",
            lane: side,
            run: |_, _| toggle_mute(),
        }),
        Arc::new(Blocking {
            name: "mediaControl",
            lane: side,
            run: media_control,
        }),
        Arc::new(Blocking {
            name: "setClipboardText",
            lane: side,
            run: set_clipboard,
        }),
        Arc::new(Blocking {
            name: "getClipboardText",
            lane: Lane::ReadOnly,
            run: get_clipboard,
        }),
        Arc::new(Blocking {
            name: "openURL",
            lane: side,
            run: open_url,
        }),
    ]
}

/// Runs a program for at most `seconds`; `Ok(stdout)` when it exited with success.
fn run(program: &str, arguments: &[&str], seconds: u64) -> std::result::Result<String, String> {
    let mut command = crate::process::command(program);
    command.args(arguments);
    match run_with_timeout(command, Duration::from_secs(seconds)) {
        Some(output) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        Some(output) => Err(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        None => Err(format!("{program} zamanında yanıt vermedi.")),
    }
}

// ----- power -----

enum Power {
    Lock,
    Sleep,
    Display,
}

fn power(action: Power) -> Result<Done> {
    let (ok, failure) = match action {
        Power::Lock => (
            lock(),
            tool_failure(
                "LOCK_PERMISSION_DENIED",
                "Bilgisayar kilitlenemedi (izin verilmedi ya da bu sistemde desteklenmiyor).",
            ),
        ),
        Power::Sleep => (
            sleep(),
            tool_failure("SLEEP_FAILED", "Bilgisayar uyku moduna alınamadı."),
        ),
        Power::Display => (
            display_sleep(),
            tool_failure("DISPLAY_FAILED", "Ekran uyku moduna alınamadı."),
        ),
    };
    if !ok {
        return Err(failure);
    }
    Ok(Done::message(match action {
        Power::Lock => "Bilgisayar kilitlendi.",
        Power::Sleep => "Bilgisayar uyku moduna alındı.",
        Power::Display => "Ekran uyku moduna alındı.",
    }))
}

#[cfg(target_os = "macos")]
fn lock() -> bool {
    // The login window's own lock call (no synthetic keystroke, so no accessibility permission is needed).
    unsafe {
        if let Ok(library) = libloading::Library::new(
            "/System/Library/PrivateFrameworks/login.framework/Versions/Current/login",
        ) {
            if let Ok(lock) =
                library.get::<unsafe extern "C" fn() -> i32>(b"SACLockScreenImmediate\0")
            {
                if lock() == 0 {
                    return true;
                }
            }
        }
    }
    run("/usr/bin/osascript", &["-e", "tell application \"System Events\" to keystroke \"q\" using {control down, command down}"], 8).is_ok()
}

#[cfg(target_os = "macos")]
fn sleep() -> bool {
    run("/usr/bin/pmset", &["sleepnow"], 8).is_ok()
}

#[cfg(target_os = "macos")]
fn display_sleep() -> bool {
    run("/usr/bin/pmset", &["displaysleepnow"], 8).is_ok()
}

#[cfg(target_os = "windows")]
fn lock() -> bool {
    run("rundll32.exe", &["user32.dll,LockWorkStation"], 8).is_ok()
}

#[cfg(target_os = "windows")]
fn sleep() -> bool {
    run(
        "rundll32.exe",
        &["powrprof.dll,SetSuspendState", "0,1,0"],
        8,
    )
    .is_ok()
}

#[cfg(target_os = "windows")]
fn display_sleep() -> bool {
    let script = "(Add-Type '[DllImport(\"user32.dll\")] public static extern int SendMessage(int hWnd, int hMsg, int wParam, int lParam);' -Name a -Pas)::SendMessage(-1,0x0112,0xF170,2)";
    run("powershell", &["-NoProfile", "-Command", script], 8).is_ok()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn lock() -> bool {
    run("loginctl", &["lock-session"], 8).is_ok()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn sleep() -> bool {
    run("systemctl", &["suspend"], 8).is_ok()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn display_sleep() -> bool {
    run("xset", &["dpms", "force", "off"], 8).is_ok()
}

// ----- sound -----

fn set_volume(_ctx: &Context, value: &Value) -> Result<Done> {
    let level: i64 = payload(value, "volume", "ses seviyesi")?;
    let level = level.clamp(0, 100);
    set_output_volume(level as u8)
        .map_err(|_| tool_failure("VOLUME_FAILED", "Ses ayarlanamadı."))?;
    Ok(Done::message(format!("Ses %{level} yapıldı.")))
}

fn toggle_mute() -> Result<Done> {
    toggle_output_mute()
        .map_err(|_| tool_failure("MUTE_FAILED", "Sessiz modu değiştirilemedi."))?;
    Ok(Done::message("Ses durumu değiştirildi."))
}

/// The current output volume, when the platform can tell (it is part of the system status).
pub(crate) fn current_volume() -> Option<u8> {
    read_output_volume()
}

#[cfg(target_os = "macos")]
fn set_output_volume(level: u8) -> std::result::Result<(), String> {
    run(
        "/usr/bin/osascript",
        &["-e", &format!("set volume output volume {level}")],
        6,
    )
    .map(|_| ())
}

#[cfg(target_os = "macos")]
fn toggle_output_mute() -> std::result::Result<(), String> {
    run(
        "/usr/bin/osascript",
        &[
            "-e",
            "set volume output muted not (output muted of (get volume settings))",
        ],
        6,
    )
    .map(|_| ())
}

#[cfg(target_os = "macos")]
fn read_output_volume() -> Option<u8> {
    run(
        "/usr/bin/osascript",
        &["-e", "output volume of (get volume settings)"],
        4,
    )
    .ok()?
    .parse()
    .ok()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn set_output_volume(level: u8) -> std::result::Result<(), String> {
    run(
        "pactl",
        &["set-sink-volume", "@DEFAULT_SINK@", &format!("{level}%")],
        6,
    )
    .or_else(|_| {
        run(
            "amixer",
            &["-D", "pulse", "sset", "Master", &format!("{level}%")],
            6,
        )
    })
    .map(|_| ())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn toggle_output_mute() -> std::result::Result<(), String> {
    run("pactl", &["set-sink-mute", "@DEFAULT_SINK@", "toggle"], 6)
        .or_else(|_| run("amixer", &["-D", "pulse", "sset", "Master", "toggle"], 6))
        .map(|_| ())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn read_output_volume() -> Option<u8> {
    let text = run("pactl", &["get-sink-volume", "@DEFAULT_SINK@"], 4).ok()?;
    text.split('/')
        .nth(1)?
        .trim()
        .trim_end_matches('%')
        .trim()
        .parse()
        .ok()
}

/// Windows has no command-line volume control; the volume keys are pressed instead (each press moves the volume by two points).
#[cfg(target_os = "windows")]
fn keys(script: &str) -> std::result::Result<String, String> {
    run(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            &format!("$w = New-Object -ComObject WScript.Shell; {script}"),
        ],
        12,
    )
}

#[cfg(target_os = "windows")]
fn set_output_volume(level: u8) -> std::result::Result<(), String> {
    keys(&format!("1..50 | ForEach-Object {{ $w.SendKeys([char]174) }}; 1..{} | ForEach-Object {{ $w.SendKeys([char]175) }}", level / 2)).map(|_| ())
}

#[cfg(target_os = "windows")]
fn toggle_output_mute() -> std::result::Result<(), String> {
    keys("$w.SendKeys([char]173)").map(|_| ())
}

#[cfg(target_os = "windows")]
fn read_output_volume() -> Option<u8> {
    None
}

// ----- media -----

fn media_control(_ctx: &Context, value: &Value) -> Result<Done> {
    let action: String = payload(value, "media", "medya komutu")?;
    if !["playPause", "nextTrack", "previousTrack"].contains(&action.as_str()) {
        return Err(tool_failure("INVALID_PAYLOAD", "Medya komutu geçersiz."));
    }
    send_media(&action)?;
    Ok(Done::message("Medya komutu uygulandı."))
}

#[cfg(target_os = "macos")]
fn send_media(action: &str) -> Result<()> {
    // The system media keys' own path (Control Center's Now Playing uses it), so the command reaches whichever app owns Now Playing.
    let command: u32 = match action {
        "playPause" => 2,
        "nextTrack" => 4,
        _ => 5,
    };
    unsafe {
        if let Ok(library) = libloading::Library::new(
            "/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote",
        ) {
            if let Ok(send) = library
                .get::<unsafe extern "C" fn(u32, *const std::ffi::c_void) -> bool>(
                    b"MRMediaRemoteSendCommand\0",
                )
            {
                if send(command, std::ptr::null()) {
                    return Ok(());
                }
            }
        }
    }
    // Without MediaRemote: ask Spotify or Music directly.
    let verb = match action {
        "playPause" => "playpause",
        "nextTrack" => "next track",
        _ => "previous track",
    };
    for app in ["Spotify", "Music"] {
        if run("/usr/bin/pgrep", &["-x", app], 3).is_ok() {
            return run("/usr/bin/osascript", &["-e", &format!("tell application \"{app}\" to {verb}")], 8).map(|_| ()).map_err(|message| {
                if message.contains("-1743") {
                    permission("MEDIA_PERMISSION_DENIED", format!("Wiqo için Otomasyon ({app}) izni verilmedi. Sistem Ayarları > Gizlilik ve Güvenlik > Otomasyon altından izin verin."))
                } else {
                    tool_failure("MEDIA_FAILED", format!("Medya komutu çalıştırılamadı: {message}"))
                }
            });
        }
    }
    Err(tool_failure(
        "NO_MEDIA_PLAYER",
        "Medya oynatıcı (Müzik veya Spotify) bulunamadı.",
    ))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn send_media(action: &str) -> Result<()> {
    let verb = match action {
        "playPause" => "play-pause",
        "nextTrack" => "next",
        _ => "previous",
    };
    run("playerctl", &[verb], 6).map(|_| ()).map_err(|_| {
        tool_failure(
            "NO_MEDIA_PLAYER",
            "Medya oynatıcı bulunamadı ya da komut uygulanamadı.",
        )
    })
}

#[cfg(target_os = "windows")]
fn send_media(action: &str) -> Result<()> {
    let key = match action {
        "playPause" => 179,
        "nextTrack" => 176,
        _ => 177,
    };
    keys(&format!("$w.SendKeys([char]{key})"))
        .map(|_| ())
        .map_err(|_| tool_failure("MEDIA_FAILED", "Medya komutu çalıştırılamadı."))
}

// ----- clipboard -----

fn set_clipboard(_ctx: &Context, value: &Value) -> Result<Done> {
    let text: String = payload(value, "clipboardText", "pano metni")?;
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_text(text))
        .map_err(|_| tool_failure("CLIPBOARD_FAILED", "Panoya metin yazılamadı."))?;
    Ok(Done::message("Metin panoya kopyalandı."))
}

fn get_clipboard(_ctx: &Context, _value: &Value) -> Result<Done> {
    let text = arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.get_text())
        .ok()
        .filter(|text| !text.is_empty());
    let text = text.ok_or_else(|| tool_failure("CLIPBOARD_EMPTY", "Panoda metin yok."))?;
    Ok(Done::new(
        "Pano içeriği okundu.",
        Some(data(
            "clipboardText",
            text.chars().take(20_000).collect::<String>(),
        )),
    ))
}

// ----- web addresses -----

/// An http(s) address with a host, no credentials, at most 2,048 characters.
pub(crate) fn is_allowed_url(raw: &str) -> bool {
    if raw.chars().count() > 2048 || raw.chars().any(|c| c.is_whitespace() && c != ' ') {
        return false;
    }
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|host| !host.is_empty())
        && url.username().is_empty()
        && url.password().is_none()
}

fn open_url(_ctx: &Context, value: &Value) -> Result<Done> {
    let raw: String = payload(value, "url", "URL")?;
    if !is_allowed_url(&raw) {
        return Err(tool_failure(
            "URL_INVALID",
            "URL geçersiz veya izin verilmeyen bir şema kullanıyor.",
        ));
    }
    let browser = open_in_browser(&raw)?;
    Ok(Done::message(
        browser
            .map(|name| format!("URL {name} içinde açıldı."))
            .unwrap_or_else(|| "URL açıldı.".into()),
    ))
}

/// Opens the address in the browser the person is looking at when that is a browser, otherwise in the default one; returns the browser's name when it is known.
fn open_in_browser(raw: &str) -> Result<Option<String>> {
    #[cfg(target_os = "macos")]
    {
        if let Some(name) = crate::apps::open_url_in_frontmost_browser(raw) {
            return Ok(Some(name));
        }
    }
    ::open::that(raw).map_err(|_| {
        error(
            "URL_OPEN_FAILED",
            "Bu adresi açacak tarayıcı bulunamadı.",
            "tool_failure",
        )
    })?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_plain_web_addresses_are_allowed() {
        assert!(is_allowed_url("https://wiqo.app/a?b=1"));
        assert!(is_allowed_url("http://localhost:3000"));
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://user:pw@evil.test",
            "https://",
            "ftp://x.test",
            "https://a.test/\nb",
            "not a url",
        ] {
            assert!(!is_allowed_url(bad), "{bad}");
        }
    }

    #[test]
    fn the_clipboard_commands_have_their_contract_shapes() {
        assert!(set_clipboard(
            &Context::for_tests(),
            &json!({ "kind": "url", "value": "x" })
        )
        .is_err());
        assert!(
            get_clipboard(&Context::for_tests(), &json!({})).is_ok()
                || get_clipboard(&Context::for_tests(), &json!({}))
                    .unwrap_err()
                    .code
                    == "CLIPBOARD_EMPTY"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn volume_is_read_and_a_bad_media_command_is_refused() {
        assert!(current_volume().is_some_and(|volume| volume <= 100));
        assert_eq!(
            media_control(
                &Context::for_tests(),
                &json!({ "kind": "media", "value": "explode" })
            )
            .unwrap_err()
            .code,
            "INVALID_PAYLOAD"
        );
    }
}
