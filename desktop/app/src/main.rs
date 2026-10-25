//! Wiqo for the desktop: one window (pairing, status, phones, permissions, activity) and a tray icon, around the same runtime the `wiqo-agent` command uses.
//! Closing the window keeps Wiqo running in the tray, so the phone can still reach this computer; "Çık" in the tray menu stops it.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use wiqo_agent::{Desktop, Event, PairingOffer, Snapshot};

mod login_item;
mod symbols;

/// The login item starts Wiqo with this: straight to the tray, no window.
const HIDDEN_ARG: &str = "--hidden";

struct App {
    desktop: Arc<Desktop>,
    /// The one pairing code that is waiting for a phone, and the task that waits. Asking for a code again returns this one while it is valid:
    /// two live codes would mean one of them is never watched, and a phone that scans it waits for nothing.
    pairing: Mutex<Option<(PairingOffer, tokio::task::JoinHandle<()>)>>,
    /// Stops a Google sign-in that is waiting for the browser.
    google_cancel: Arc<tokio::sync::Notify>,
}

type Reply<T> = Result<T, String>;

fn text<E: std::fmt::Display>(error: E) -> String {
    error.to_string()
}

#[tauri::command]
fn snapshot(app: State<'_, App>) -> Snapshot {
    app.desktop.snapshot()
}

/// Shows a new pairing code and waits for a phone in the background; the window hears `paired` or `pairing-failed`.
#[tauri::command]
async fn start_pairing(handle: AppHandle, app: State<'_, App>) -> Reply<PairingOffer> {
    if let Some((offer, task)) = app.pairing.lock().map_err(text)?.as_ref() {
        if !task.is_finished() && offer.seconds_left() > 60 {
            return Ok(offer.clone());
        }
    }
    let offer = app.desktop.start_pairing().await.map_err(text)?;
    let (desktop, waiting) = (app.desktop.clone(), offer.clone());
    let task = tokio::spawn(async move {
        if let Err(error) = desktop.finish_pairing(&waiting).await {
            tracing::warn!("pairing: {error}");
            let _ = handle.emit("pairing-failed", error.to_string());
        }
    });
    if let Some((_, previous)) = app
        .pairing
        .lock()
        .map_err(text)?
        .replace((offer.clone(), task))
    {
        previous.abort();
    }
    Ok(offer)
}

#[tauri::command]
fn cancel_pairing(app: State<'_, App>) -> Reply<()> {
    if let Some((_, task)) = app.pairing.lock().map_err(text)?.take() {
        task.abort();
    }
    Ok(())
}

#[tauri::command]
fn set_full_access(app: State<'_, App>, on: bool) -> Reply<Snapshot> {
    app.desktop.set_full_access(on).map_err(text)?;
    Ok(app.desktop.snapshot())
}

#[tauri::command]
fn set_capability(app: State<'_, App>, id: String, enabled: bool) -> Reply<Snapshot> {
    app.desktop.set_capability(&id, enabled).map_err(text)?;
    Ok(app.desktop.snapshot())
}

#[tauri::command]
async fn remove_phone(app: State<'_, App>, pairing_id: String) -> Reply<Snapshot> {
    app.desktop.remove_phone(&pairing_id).await.map_err(text)?;
    Ok(app.desktop.snapshot())
}

#[tauri::command]
fn set_connected(app: State<'_, App>, on: bool) -> Snapshot {
    if on {
        app.desktop.connect();
    } else {
        app.desktop.disconnect();
    }
    app.desktop.snapshot()
}

#[tauri::command]
fn permissions(app: State<'_, App>) -> Vec<wiqo_agent::permissions::Permission> {
    app.desktop.permissions()
}

/// Shows the system dialog (or opens System Settings) for one permission; the dialog blocks, so it runs off the UI thread.
#[tauri::command]
async fn request_permission(
    app: State<'_, App>,
    id: String,
) -> Reply<Vec<wiqo_agent::permissions::Permission>> {
    let desktop = app.desktop.clone();
    tokio::task::spawn_blocking(move || desktop.request_permission(&id))
        .await
        .map_err(text)
}

/// Screen Recording takes effect only in a new process.
#[tauri::command]
fn restart_app(handle: AppHandle) {
    handle.restart();
}

/// Starts with the computer (hidden, in the tray), so the phone never finds the desktop offline after a restart.
#[tauri::command]
fn autostart(handle: AppHandle) -> bool {
    login_item::is_enabled(&handle)
}

#[tauri::command]
fn set_autostart(handle: AppHandle, on: bool) -> Reply<bool> {
    login_item::set(&handle, on)?;
    Ok(login_item::is_enabled(&handle))
}

/// "Sorun bildir": sends the report (with the diagnostics when allowed) or saves it on the Desktop; a saved file is shown in the file manager.
#[tauri::command]
async fn report_problem(
    app: State<'_, App>,
    report: wiqo_agent::ProblemReport,
) -> Reply<wiqo_agent::ReportOutcome> {
    let outcome = app.desktop.report_problem(report).await.map_err(text)?;
    if let Some(path) = &outcome.saved_to {
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(path)
            .status();
        #[cfg(windows)]
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{path}"))
            .status();
        // Linux file managers have no common "select": the folder opens.
        #[cfg(target_os = "linux")]
        let _ = std::process::Command::new("xdg-open")
            .arg(
                std::path::Path::new(path)
                    .parent()
                    .unwrap_or(std::path::Path::new(path)),
            )
            .status();
    }
    Ok(outcome)
}

/// How many crash reports were written since the window last asked (each is reported once).
#[tauri::command]
fn unseen_crashes() -> usize {
    wiqo_agent::diagnostics::unseen_crashes()
}

/// "Geri al" for one command (`key`) or a whole task (`task`).
#[tauri::command]
async fn undo(app: State<'_, App>, key: Option<String>, task: Option<String>) -> Reply<String> {
    let desktop = app.desktop.clone();
    tokio::task::spawn_blocking(move || desktop.undo(key.as_deref(), task.as_deref()))
        .await
        .map_err(text)?
        .map_err(text)
}

// ----- chat -----

#[tauri::command]
fn chat_account(app: State<'_, App>) -> Option<String> {
    app.desktop.chat.email()
}

/// Signs in and makes this computer the account's device (online at once).
#[tauri::command]
async fn sign_in(app: State<'_, App>, email: String, password: String) -> Reply<Snapshot> {
    app.desktop.sign_in(&email, &password).await.map_err(text)?;
    Ok(app.desktop.snapshot())
}

#[tauri::command]
async fn register(
    app: State<'_, App>,
    email: String,
    password: String,
    invite: String,
) -> Reply<Snapshot> {
    app.desktop
        .register(&email, &password, &invite)
        .await
        .map_err(text)?;
    Ok(app.desktop.snapshot())
}

/// Opens Google's page in the browser; the answer comes back as a `wiqo://oauth-callback` link (see `main`).
#[tauri::command]
async fn google_sign_in(app: State<'_, App>) -> Reply<Option<Snapshot>> {
    let cancel = app.google_cancel.clone();
    match app.desktop.google_sign_in(open_browser, &cancel).await {
        Ok(_) => Ok(Some(app.desktop.snapshot())),
        // An older server: the answer comes back as a wiqo:// link instead (`signed-in` / `sign-in-failed` events).
        Err(error) if error.to_string() == "WAITING_FOR_LINK" => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

#[tauri::command]
fn cancel_google_sign_in(app: State<'_, App>) {
    app.google_cancel.notify_waiters();
}

#[tauri::command]
async fn forgot_password(app: State<'_, App>, email: String) -> Reply<String> {
    app.desktop.chat.forgot_password(&email).await.map_err(text)
}

#[tauri::command]
async fn reset_password(
    app: State<'_, App>,
    email: String,
    code: String,
    password: String,
) -> Reply<Snapshot> {
    app.desktop
        .reset_password(&email, &code, &password)
        .await
        .map_err(text)?;
    Ok(app.desktop.snapshot())
}

#[tauri::command]
async fn profile(app: State<'_, App>) -> Reply<serde_json::Value> {
    app.desktop.chat.profile().await.map_err(text)
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("/usr/bin/open")
        .arg(url)
        .status();
    #[cfg(windows)]
    let _ = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .status();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).status();
}

/// A `wiqo://oauth-callback?...` link (Google sign-in) finishes the sign-in; the window hears `signed-in` or `sign-in-failed`.
fn handle_link(handle: &AppHandle, link: &str) {
    if !link.starts_with("wiqo://oauth-callback") {
        return;
    }
    show_window(handle);
    let (handle, link) = (handle.clone(), link.to_string());
    tauri::async_runtime::spawn(async move {
        let desktop = handle.state::<App>().desktop.clone();
        match desktop.google_finish(&link).await {
            Ok(email) => {
                let _ = handle.emit("signed-in", email);
            }
            Err(error) => {
                let _ = handle.emit("sign-in-failed", error.to_string());
            }
        }
    });
}

#[tauri::command]
fn sign_out(app: State<'_, App>) -> Reply<Snapshot> {
    app.desktop.sign_out().map_err(text)?;
    Ok(app.desktop.snapshot())
}

/// Starts a message; its answer arrives as `chat` events with this `turn`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn chat_send(
    app: State<'_, App>,
    turn: String,
    conversation: String,
    history: serde_json::Value,
    message: String,
    task_id: Option<String>,
    question_id: Option<String>,
    attachments: Option<serde_json::Value>,
) {
    let desktop = app.desktop.clone();
    let answer_to = task_id.zip(question_id);
    let attachments = attachments.unwrap_or(serde_json::Value::Null);
    tauri::async_runtime::spawn(async move {
        desktop
            .chat_send(
                &turn,
                &conversation,
                history,
                &message,
                answer_to,
                attachments,
            )
            .await
    });
}

/// A file from a reply, for showing it in the conversation: its type and base64 bytes (pictures and PDFs up to 12 MB).
#[tauri::command]
async fn artifact_preview(app: State<'_, App>, id: String) -> Reply<serde_json::Value> {
    use base64::Engine as _;
    let (mime, bytes) = app.desktop.chat.artifact(&id).await.map_err(text)?;
    if bytes.len() > 12 * 1024 * 1024 {
        return Err("Önizleme için çok büyük.".into());
    }
    Ok(
        serde_json::json!({ "mimeType": mime, "base64": base64::engine::general_purpose::STANDARD.encode(bytes) }),
    )
}

/// Saves a reply's file into Downloads (a taken name gets " 2") and opens it or shows it.
#[tauri::command]
async fn artifact_save(
    app: State<'_, App>,
    id: String,
    filename: String,
    open: bool,
) -> Reply<String> {
    let (_, bytes) = app.desktop.chat.artifact(&id).await.map_err(text)?;
    let folder = dirs_downloads().ok_or("İndirilenler klasörü bulunamadı.")?;
    let clean: String = filename
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '\0'))
        .collect();
    let name = if clean.trim().is_empty() {
        "Wiqo dosyası".to_string()
    } else {
        clean
    };
    let mut path = folder.join(&name);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) => (stem.to_string(), format!(".{ext}")),
        None => (name.clone(), String::new()),
    };
    let mut index = 2;
    while path.exists() {
        path = folder.join(format!("{stem} {index}{ext}"));
        index += 1;
    }
    std::fs::write(&path, bytes).map_err(text)?;
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("/usr/bin/open")
        .args(if open { vec![] } else { vec!["-R"] })
        .arg(&path)
        .status();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer")
        .arg(if open {
            path.display().to_string()
        } else {
            format!("/select,{}", path.display())
        })
        .status();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open")
        .arg(if open {
            path.as_path()
        } else {
            path.parent().unwrap_or(path.as_path())
        })
        .status();
    Ok(path.to_string_lossy().to_string())
}

fn dirs_downloads() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(|home| std::path::PathBuf::from(home).join("Downloads"))
        .filter(|path| path.is_dir())
}

/// Opens a web address from a card (a source, a map, a link) in the browser; only http(s).
#[tauri::command]
fn open_link(url: String) -> Reply<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Bu bağlantı açılamaz.".into());
    }
    open_browser(&url);
    Ok(())
}

/// An SF Symbol as a PNG data address (macOS); `None` elsewhere, where the page draws its own SVG.
#[tauri::command]
fn symbol(name: String, size: f64, weight: Option<f64>) -> Option<String> {
    use base64::Engine as _;
    let bytes = symbols::png(&name, size.clamp(8.0, 64.0), weight.unwrap_or(0.0))?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// "Geri al" on a chat card.
#[tauri::command]
fn undo_record(app: State<'_, App>, kind: String, id: String) -> Reply<String> {
    app.desktop.undo_record(&kind, &id).map_err(text)
}

#[tauri::command]
async fn chat_cancel(app: State<'_, App>, task: String, conversation: String) -> Reply<()> {
    app.desktop.stop_work();
    app.desktop.chat.cancel(&task, &conversation).await;
    Ok(())
}

#[tauri::command]
async fn chat_conversations(app: State<'_, App>) -> Reply<serde_json::Value> {
    app.desktop.chat.conversations().await.map_err(text)
}

/// The Ajanlar list: reminders, routines, watches, jobs, to-dos, goals and helpers.
#[tauri::command]
async fn agents_overview(app: State<'_, App>) -> Reply<serde_json::Value> {
    app.desktop.chat.agents().await.map_err(text)
}

#[tauri::command]
async fn agent_action(
    app: State<'_, App>,
    reference: String,
    action: String,
) -> Reply<serde_json::Value> {
    app.desktop
        .chat
        .agent_action(&reference, &action)
        .await
        .map_err(text)
}

#[tauri::command]
async fn chat_messages(app: State<'_, App>, conversation: String) -> Reply<serde_json::Value> {
    app.desktop.chat.messages(&conversation).await.map_err(text)
}

/// This computer's city for a reply that asks where you are (Location permission asked the first time). `None`: refused, off, or not found.
#[tauri::command]
async fn current_city() -> Option<String> {
    tauri::async_runtime::spawn_blocking(|| {
        wiqo_agent::location::current_city(std::time::Duration::from_secs(25))
    })
    .await
    .ok()
    .flatten()
}

#[tauri::command]
async fn continue_location(
    app: State<'_, App>,
    request_id: String,
    city: Option<String>,
) -> Reply<serde_json::Value> {
    app.desktop
        .chat
        .continue_location(&request_id, city.as_deref())
        .await
        .map_err(text)
}

#[tauri::command]
async fn chat_confirm(
    app: State<'_, App>,
    confirmation: serde_json::Value,
    approved: bool,
) -> Reply<serde_json::Value> {
    app.desktop
        .chat_confirm(&confirmation, approved)
        .await
        .map_err(text)
}

#[tauri::command]
fn answer_approval(app: State<'_, App>, id: String, allow: bool) {
    app.desktop.answer_approval(&id, allow);
}

#[tauri::command]
fn set_approve_on_pc(app: State<'_, App>, on: bool) -> Reply<Snapshot> {
    app.desktop.set_approve_on_pc(on).map_err(text)?;
    Ok(app.desktop.snapshot())
}

#[tauri::command]
fn set_reads_without_phone(app: State<'_, App>, on: bool) -> Reply<Snapshot> {
    app.desktop.set_reads_without_phone(on).map_err(text)?;
    Ok(app.desktop.snapshot())
}

#[tauri::command]
fn working(app: State<'_, App>) -> Vec<wiqo_agent::WorkItem> {
    app.desktop.working()
}

#[tauri::command]
fn show_main_window(handle: AppHandle) {
    show_window(&handle);
}

#[tauri::command]
fn pending_approvals(app: State<'_, App>) -> Vec<wiqo_agent::PendingApproval> {
    app.desktop.pending_approvals()
}

#[tauri::command]
fn hide_panel(handle: AppHandle) {
    if let Some(panel) = handle.get_webview_window(PANEL) {
        let _ = panel.hide();
    }
}

const PANEL: &str = "panel";

/// The corner mascot: a small transparent always-on-top window shown while Wiqo works or waits for a "yes". The permission card itself is in the main window.
fn show_panel(handle: &AppHandle, focus: bool) {
    let panel = match handle.get_webview_window(PANEL) {
        Some(panel) => panel,
        None => {
            let (width, height) = (80.0, 80.0);
            let built = tauri::WebviewWindowBuilder::new(
                handle,
                PANEL,
                tauri::WebviewUrl::App("panel.html".into()),
            )
            .title("Wiqo")
            .inner_size(width, height)
            .resizable(false)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible(false)
            .focused(false)
            .build();
            let Ok(panel) = built else { return };
            if let Ok(Some(monitor)) = panel.primary_monitor() {
                let scale = monitor.scale_factor();
                let size = monitor.size().to_logical::<f64>(scale);
                let _ = panel.set_position(tauri::LogicalPosition::new(
                    size.width - width - 20.0,
                    size.height - height - 90.0,
                ));
            }
            panel
        }
    };
    let _ = panel.show();
    if focus {
        let _ = panel.set_focus();
    }
}

#[tauri::command]
fn stop_work(app: State<'_, App>) {
    app.desktop.stop_work();
}

fn show_window(handle: &AppHandle) {
    if let Some(window) = handle.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn tray(handle: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(handle, "open", "Wiqo'yu aç", true, None::<&str>)?;
    let stop = MenuItem::with_id(handle, "stop", "Çalışan işi durdur", true, None::<&str>)?;
    let pause = MenuItem::with_id(handle, "pause", "Bağlantıyı durdur", true, None::<&str>)?;
    let quit = MenuItem::with_id(handle, "quit", "Wiqo'dan çık", true, None::<&str>)?;
    let menu = Menu::with_items(
        handle,
        &[
            &open,
            &stop,
            &pause,
            &PredefinedMenuItem::separator(handle)?,
            &quit,
        ],
    )?;
    let pause_item = pause.clone();
    TrayIconBuilder::with_id("wiqo")
        .icon(tauri::include_image!("icons/32x32.png"))
        .tooltip("Wiqo")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => show_window(app),
            "stop" => app.state::<App>().desktop.stop_work(),
            "pause" => {
                let desktop = app.state::<App>().desktop.clone();
                if desktop.is_running() {
                    desktop.disconnect();
                    let _ = pause_item.set_text("Bağlantıyı başlat");
                } else {
                    desktop.connect();
                    let _ = pause_item.set_text("Bağlantıyı durdur");
                }
                let _ = app.emit("wiqo", Event::Phones);
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        })
        .build(handle)?;
    Ok(())
}

/// Looks for a new version at start and every six hours, downloads and verifies it (the updater checks its signature against the key in
/// tauri.conf.json) and installs it; the window offers the restart, so a running task is never cut off.
fn keep_updated(handle: AppHandle) {
    use tauri_plugin_updater::UpdaterExt;
    tauri::async_runtime::spawn(async move {
        loop {
            match handle.updater() {
                Ok(updater) => match updater.check().await {
                    Ok(Some(update)) => {
                        let version = update.version.clone();
                        match update.download_and_install(|_, _| {}, || {}).await {
                            Ok(()) => {
                                tracing::info!("update {version} installed; waiting for a restart");
                                let _ = handle.emit("update-ready", version);
                                return;
                            }
                            Err(error) => tracing::warn!("update {version}: {error}"),
                        }
                    }
                    Ok(None) => {}
                    Err(error) => tracing::info!("update check: {error}"),
                },
                Err(error) => tracing::warn!("updater: {error}"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
        }
    });
}

/// Logs go to `desktop.log` in the app's data folder (a bundled app has no terminal).
fn start_logging() {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    match wiqo_agent::log_file() {
        Some(file) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init(),
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}

fn main() {
    start_logging();
    wiqo_agent::diagnostics::install_crash_reporter();
    let desktop = match Desktop::open(None) {
        Ok(desktop) => desktop,
        Err(error) => {
            eprintln!("Wiqo açılamadı: {error}");
            std::process::exit(1);
        }
    };
    let hidden = std::env::args().any(|arg| arg == HIDDEN_ARG);
    tauri::Builder::default()
        // A second launch (the Dock, Spotlight, a login item) brings up the running Wiqo instead of a rival connection under the same device id.
        .plugin(tauri_plugin_single_instance::init(|handle, args, _cwd| {
            show_window(handle);
            // Windows and Linux hand a `wiqo://` link to the running Wiqo as an argument of a second launch.
            for arg in &args {
                handle_link(handle, arg);
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![HIDDEN_ARG]),
        ))
        .manage(App {
            desktop: desktop.clone(),
            pairing: Mutex::new(None),
            google_cancel: Arc::new(tokio::sync::Notify::new()),
        })
        .invoke_handler(tauri::generate_handler![
            snapshot,
            start_pairing,
            cancel_pairing,
            set_full_access,
            set_capability,
            remove_phone,
            set_connected,
            stop_work,
            autostart,
            set_autostart,
            answer_approval,
            undo,
            chat_account,
            sign_in,
            register,
            google_sign_in,
            cancel_google_sign_in,
            forgot_password,
            reset_password,
            profile,
            sign_out,
            chat_send,
            chat_cancel,
            chat_conversations,
            agents_overview,
            agent_action,
            chat_messages,
            chat_confirm,
            current_city,
            continue_location,
            artifact_preview,
            artifact_save,
            open_link,
            undo_record,
            symbol,
            set_approve_on_pc,
            set_reads_without_phone,
            working,
            hide_panel,
            pending_approvals,
            show_main_window,
            report_problem,
            unseen_crashes,
            permissions,
            request_permission,
            restart_app
        ])
        .setup(move |app| {
            tray(app.handle())?;
            login_item::first_launch(app.handle());
            desktop.watch_permissions();
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let links = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        handle_link(&links, url.as_str());
                    }
                });
            }
            keep_updated(app.handle().clone());
            if hidden {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            // Everything the runtime reports goes to the window as one `wiqo` event stream.
            let handle = app.handle().clone();
            let mut events = desktop.events();
            let connected = desktop.clone();
            tauri::async_runtime::spawn(async move {
                connected.connect();
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            // A task failed for a missing permission: the window comes up on the permissions screen.
                            match &event {
                                Event::PermissionNeeded { .. } => show_window(&handle),
                                Event::Working { items } if !items.is_empty() => {
                                    show_panel(&handle, false)
                                }
                                Event::Approval { .. } => {
                                    show_panel(&handle, false);
                                    show_window(&handle);
                                }
                                _ => {}
                            }
                            let _ = handle.emit("wiqo", event);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window hides it: Wiqo keeps answering the phone from the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("Wiqo could not start");
}
