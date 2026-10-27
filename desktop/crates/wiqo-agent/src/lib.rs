//! One Wiqo desktop, ready to use from a window or a terminal: it opens its state, pairs with a phone, keeps the server connection, and tells what happens.
//! The app (Tauri) and the `wiqo-agent` command are two faces of this same object; neither has logic of its own.

pub mod chat;
pub mod diagnostics;
pub mod location;
pub mod permissions;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use base64::Engine as _;
use serde::Serialize;
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinHandle;
use wiqo_contract::{Manifest, Platform};
use wiqo_core::api::{Api, PollResult};
use wiqo_core::engine::{Engine, Services};
use wiqo_core::ledger::Ledger;
use wiqo_core::relay::{platform_name, ConnectionState, Observer, Relay, Runtime};
use wiqo_core::storage::{
    CredentialStore, Credentials, DataDir, Identity, PairedPhone, PairedPhones, Settings,
};
use wiqo_core::verify::NonceCache;
use wiqo_core::wire::{CommandResult, CommandStatus};

/// "2026-10-20T17:00:00.123Z" -> "2026-10-20T17:00:00Z": the form phones decode.
fn whole_seconds(stamp: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .map(|date| {
            date.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
        .unwrap_or_else(|_| stamp.to_string())
}

/// A file in the desktop's data folder (app-level markers that are not part of the runtime's state).
pub fn data_file(name: &str) -> Option<std::path::PathBuf> {
    DataDir::default_location().ok().map(|dir| dir.file(name))
}

/// The name the person gave this computer ("Emre'nin MacBook'u"), not the network host name.
fn computer_name() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(output) = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
    {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if output.status.success() && !name.is_empty() {
            return name;
        }
    }
    #[cfg(target_os = "windows")]
    if let Ok(name) = std::env::var("COMPUTERNAME") {
        return name;
    }
    sysinfo::System::host_name()
        .map(|name| name.trim_end_matches(".local").to_string())
        .unwrap_or_else(|| "Wiqo".into())
}

/// `desktop.log` in the data folder, opened for appending (moved to `desktop.old.log` past 2 MB).
pub fn log_file() -> Option<std::fs::File> {
    let dir = DataDir::default_location().ok()?;
    let path = dir.file("desktop.log");
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 2_000_000) {
        let _ = std::fs::rename(&path, dir.file("desktop.old.log"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

pub const DEFAULT_SERVER: &str = "https://api.wiqoai.com";
const ACTIVITY_KEPT: usize = 100;

/// What the window shows and the tray summarises.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Event {
    Connection {
        state: ConnectionState,
    },
    Activity {
        item: Activity,
    },
    Phones,
    Paired {
        phone: String,
    },
    /// What runs right now (the progress panel).
    Working {
        items: Vec<WorkItem>,
    },
    /// A step of the server's agent waits for a click here ("PC'de onayla").
    Approval {
        id: String,
        kind: String,
        detail: String,
        seconds: u64,
    },
    ApprovalClosed {
        id: String,
    },
    /// One event of a chat answer (`turn` is the message it belongs to): the server's own SSE events, plus `task`, `step_update` and `turnEnd`.
    Chat {
        turn: String,
        event: serde_json::Value,
    },
    /// One event of an agent thread's answer (`turn` is the message it belongs to): the server's `agent_*` events, then `turnEnd`.
    Thread {
        turn: String,
        thread: String,
        event: serde_json::Value,
    },
    /// A command failed for a missing system permission: the window opens on it.
    PermissionNeeded {
        id: String,
    },
}

/// One finished command, as a line in the activity list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub key: String,
    pub kind: String,
    pub ok: bool,
    pub message: String,
    pub at: String,
    /// The task it belongs to (the server's run id, or a terminal session's grant): the window groups by it.
    pub task: Option<String>,
    /// What can be taken back: the change's undo records.
    pub undo: Vec<UndoRef>,
    pub undone: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoRef {
    pub id: String,
    /// "file" (a file operation's record) or "terminal" (a terminal session's edit).
    pub kind: &'static str,
}

/// Every `undoId` a result carries.
fn undo_refs(result: &CommandResult) -> Vec<UndoRef> {
    fn walk(value: &serde_json::Value, found: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    match (key.as_str(), child.as_str()) {
                        ("undoId", Some(id)) => found.push(id.to_string()),
                        _ => walk(child, found),
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|item| walk(item, found)),
            _ => {}
        }
    }
    let Some(data) = &result.data else {
        return Vec::new();
    };
    let kind = if data["kind"] == "terminalResult" {
        "terminal"
    } else {
        "file"
    };
    let mut found = Vec::new();
    walk(data, &mut found);
    found.into_iter().map(|id| UndoRef { id, kind }).collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub id: String,
    pub enabled: bool,
    /// The operations behind this capability need the phone's confirmation each time.
    pub asks: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhoneInfo {
    pub pairing_id: String,
    pub name: String,
    pub platform: String,
}

/// Everything the window needs at once.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub device_name: String,
    pub device_id: String,
    pub platform: String,
    pub version: String,
    pub server: String,
    pub paired: bool,
    pub connection: ConnectionState,
    pub phones: Vec<PhoneInfo>,
    pub full_access: bool,
    pub approve_on_pc: bool,
    pub reads_without_phone: bool,
    /// The signed-in Wiqo account, if any.
    pub account: Option<String>,
    pub capabilities: Vec<Capability>,
    pub activity: Vec<Activity>,
}

/// The code a phone scans, and how it looks.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingOffer {
    pub code: String,
    pub qr_svg: String,
    pub expires_at: String,
    #[serde(skip)]
    session_id: String,
    #[serde(skip)]
    secret: String,
}

impl PairingOffer {
    /// Seconds until the code expires (0 when it already has).
    pub fn seconds_left(&self) -> i64 {
        chrono::DateTime::parse_from_rfc3339(&self.expires_at)
            .map(|date| (date.timestamp() - chrono::Utc::now().timestamp()).max(0))
            .unwrap_or(0)
    }
}

/// What the person chose in "Sorun bildir".
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProblemReport {
    pub category: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub include_logs: bool,
    #[serde(default)]
    pub contact_ok: bool,
    /// Only save the zip on the Desktop.
    #[serde(default)]
    pub save: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportOutcome {
    pub sent: bool,
    pub saved_to: Option<String>,
}

/// One command that is running.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItem {
    pub key: String,
    pub kind: String,
    pub detail: String,
    pub started_at: String,
    pub task: Option<String>,
}

const APPROVAL_SECONDS: u64 = 90;

/// A "PC'de onayla" question that is still open: what the corner card fetches when it opens, because the card's window may not exist
/// yet when the question is raised (the first question of a session used to be lost, and the step was refused for no answer).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingApproval {
    pub id: String,
    pub kind: String,
    pub detail: String,
    pub seconds: u64,
}

struct Shared {
    events: broadcast::Sender<Event>,
    connection: Mutex<ConnectionState>,
    activity: Mutex<VecDeque<Activity>>,
    working: Mutex<Vec<WorkItem>>,
    approvals: Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
    open_questions: Mutex<Vec<(PendingApproval, std::time::Instant)>>,
    /// Kinds the person just allowed: the same kind is not asked again for two minutes, so a task of thirty moves is one question, not thirty.
    allowed_kinds: Mutex<std::collections::HashMap<String, std::time::Instant>>,
    permissions: Arc<permissions::Permissions>,
    /// When each permission was last asked for after a failed step: one ask per two minutes, never a dialog storm.
    asked_at: Mutex<std::collections::HashMap<&'static str, std::time::Instant>>,
}

impl Shared {
    fn announce_work(&self) {
        let items = self.working.lock().expect("working lock").clone();
        let _ = self.events.send(Event::Working { items });
    }
}

/// "PC'de onayla": the step waits for the person's click in the panel; no answer within 90 seconds refuses it.
struct Desk(Arc<Shared>);

#[async_trait::async_trait]
impl wiqo_core::engine::Approver for Desk {
    async fn approve(&self, kind: &str, detail: &str) -> bool {
        // Changes that can be taken back (a move, a copy, a write) share one answer for a short while; a delete or a send is asked every time.
        let batched = matches!(
            kind,
            "moveFile"
                | "copyFile"
                | "writeFile"
                | "saveTextFile"
                | "renameFile"
                | "moveDesktopFile"
                | "renameDesktopFile"
        );
        if batched {
            let mut allowed = self.0.allowed_kinds.lock().expect("allowed lock");
            allowed.retain(|_, at| at.elapsed() < Duration::from_secs(120));
            if allowed.contains_key(kind) {
                return true;
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (answer, answered) = tokio::sync::oneshot::channel();
        self.0
            .approvals
            .lock()
            .expect("approvals lock")
            .insert(id.clone(), answer);
        self.0.open_questions.lock().expect("questions lock").push((
            PendingApproval {
                id: id.clone(),
                kind: kind.to_string(),
                detail: detail.to_string(),
                seconds: APPROVAL_SECONDS,
            },
            std::time::Instant::now(),
        ));
        let _ = self.0.events.send(Event::Approval {
            id: id.clone(),
            kind: kind.to_string(),
            detail: detail.to_string(),
            seconds: APPROVAL_SECONDS,
        });
        let allowed = matches!(
            tokio::time::timeout(Duration::from_secs(APPROVAL_SECONDS), answered).await,
            Ok(Ok(true))
        );
        if allowed && batched {
            self.0
                .allowed_kinds
                .lock()
                .expect("allowed lock")
                .insert(kind.to_string(), std::time::Instant::now());
        }
        self.0.approvals.lock().expect("approvals lock").remove(&id);
        self.0
            .open_questions
            .lock()
            .expect("questions lock")
            .retain(|(question, _)| question.id != id);
        let _ = self.0.events.send(Event::ApprovalClosed { id });
        allowed
    }
}

impl Observer for Shared {
    fn state(&self, state: &ConnectionState) {
        *self.connection.lock().expect("connection lock") = state.clone();
        let _ = self.events.send(Event::Connection {
            state: state.clone(),
        });
    }

    fn command(&self, key: &str, kind: &str, result: &CommandResult) {
        let task = self
            .working
            .lock()
            .expect("working lock")
            .iter()
            .find(|item| item.key == key)
            .and_then(|item| item.task.clone());
        let item = Activity {
            key: key.to_string(),
            kind: kind.to_string(),
            ok: result.status == CommandStatus::Completed,
            message: result.message.clone(),
            at: result.finished_at.clone(),
            task,
            undo: undo_refs(result),
            undone: false,
        };
        let mut activity = self.activity.lock().expect("activity lock");
        activity.push_front(item.clone());
        activity.truncate(ACTIVITY_KEPT);
        let _ = self.events.send(Event::Activity { item });
        if let Some((code, id)) = result.error.as_ref().and_then(|error| {
            permissions::for_error(&error.code).map(|id| (error.code.clone(), id))
        }) {
            let _ = self
                .events
                .send(Event::PermissionNeeded { id: id.to_string() });
            // Asked the moment it is needed: the system dialog (or its Settings page) comes up by itself.
            let due = {
                let mut asked = self.asked_at.lock().expect("asked lock");
                let due = asked
                    .get(id)
                    .is_none_or(|at| at.elapsed() > Duration::from_secs(120));
                if due {
                    asked.insert(id, std::time::Instant::now());
                }
                due
            };
            if due {
                let permissions = self.permissions.clone();
                std::thread::spawn(move || {
                    permissions.request_for_error(&code);
                });
            }
        }
    }

    fn phones_changed(&self) {
        let _ = self.events.send(Event::Phones);
    }

    fn started(&self, key: &str, kind: &str, detail: &str, task: Option<&str>) {
        self.working.lock().expect("working lock").push(WorkItem {
            key: key.to_string(),
            kind: kind.to_string(),
            detail: detail.to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            task: task.map(str::to_string),
        });
        self.announce_work();
    }

    fn finished(&self, key: &str) {
        self.working
            .lock()
            .expect("working lock")
            .retain(|item| item.key != key);
        self.announce_work();
    }
}

pub struct Desktop {
    pub runtime: Arc<Runtime>,
    server: String,
    shared: Arc<Shared>,
    relay: Mutex<Option<(JoinHandle<()>, Arc<Notify>)>>,
    permissions: Arc<permissions::Permissions>,
    pub chat: chat::Chat,
}

impl Desktop {
    /// Opens the desktop's state in the platform's application data folder (or `WIQO_DATA_DIR`). `server` overrides the paired server.
    pub fn open(server: Option<String>) -> Result<Arc<Desktop>> {
        let dir = DataDir::default_location().context("data folder")?;
        let platform = Platform::current();
        let hostname = computer_name();
        let identity = Identity::load_or_create(&dir, &hostname)?;
        let settings = Arc::new(Settings::open(&dir));
        // The keychain holds the tokens, except for a data folder of its own (tests, a second profile).
        let vault = Arc::new(wiqo_core::vault::Vault::new(
            std::env::var_os("WIQO_DATA_DIR").is_none(),
        ));
        let credentials = Arc::new(CredentialStore::open_with(&dir, vault.clone()));
        let services = Arc::new(Services {
            data_dir: dir.clone(),
            credentials: credentials.clone(),
            access: Arc::new(wiqo_core::grants::Access::default()),
        });
        let (events, _) = broadcast::channel(256);
        let permissions = Arc::new(permissions::Permissions::new(dir.file("permissions.json")));
        let shared = Arc::new(Shared {
            events,
            connection: Mutex::new(ConnectionState::Stopped),
            activity: Mutex::new(VecDeque::new()),
            working: Mutex::new(Vec::new()),
            approvals: Mutex::new(std::collections::HashMap::new()),
            open_questions: Mutex::new(Vec::new()),
            allowed_kinds: Mutex::new(std::collections::HashMap::new()),
            permissions: permissions.clone(),
            asked_at: Mutex::new(std::collections::HashMap::new()),
        });
        let mut engine = Engine::new(platform, settings, Arc::new(Ledger::open(&dir)), services);
        wiqo_ops::register_all(&mut engine).map_err(anyhow::Error::msg)?;
        engine.set_approver(Arc::new(Desk(shared.clone())));
        let probe = permissions.clone();
        engine.set_blockers(Arc::new(move || probe.blockers()));
        let runtime = Arc::new(Runtime {
            identity,
            credentials,
            phones: PairedPhones::open_with(&dir, vault.clone()),
            engine,
            nonces: NonceCache::default(),
        });
        let server = server
            .or_else(|| std::env::var("WIQO_SERVER").ok())
            .or_else(|| {
                runtime
                    .credentials
                    .get()
                    .map(|credentials| credentials.api_base)
            })
            .unwrap_or_else(|| DEFAULT_SERVER.to_string());
        let initial = if runtime.credentials.get().is_some() {
            ConnectionState::Stopped
        } else {
            ConnectionState::NotPaired
        };
        *shared.connection.lock().expect("connection lock") = initial;
        let chat = chat::Chat::new(&server, vault, &dir);
        Ok(Arc::new(Desktop {
            chat,
            permissions,
            runtime,
            server,
            shared,
            relay: Mutex::new(None),
        }))
    }

    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.shared.events.subscribe()
    }

    pub fn is_paired(&self) -> bool {
        self.runtime.credentials.get().is_some()
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    /// Starts the server connection in the background (once; a second call does nothing while it runs).
    pub fn connect(self: &Arc<Self>) {
        let mut relay = self.relay.lock().expect("relay lock");
        if relay.as_ref().is_some_and(|(task, _)| !task.is_finished()) || !self.is_paired() {
            return;
        }
        let connection =
            Relay::new(self.runtime.clone(), &self.server).with_observer(self.shared.clone());
        let refresher = connection.refresher();
        let task = tokio::spawn(async move { connection.run().await });
        *relay = Some((task, refresher));
    }

    /// Stops the connection: no command reaches this computer until `connect` again.
    pub fn disconnect(&self) {
        if let Some((task, _)) = self.relay.lock().expect("relay lock").take() {
            task.abort();
        }
        self.shared.state(&if self.is_paired() {
            ConnectionState::Stopped
        } else {
            ConnectionState::NotPaired
        });
    }

    pub fn is_running(&self) -> bool {
        self.relay
            .lock()
            .expect("relay lock")
            .as_ref()
            .is_some_and(|(task, _)| !task.is_finished())
    }

    /// "Durdur": what this computer is doing for the server's agent stops — background terminal jobs at once, and the task grant is withdrawn.
    pub fn stop_work(&self) {
        if let Some(terminal) = self.runtime.engine.terminal() {
            terminal.cancel_all();
        }
        let access = &self.runtime.engine.services().access;
        if access.active().is_some() {
            access.grant(0, Vec::new(), String::new());
        }
    }

    /// "Sorun bildir": sends the person's choice and note, with the diagnostics zip when they allow it, to the Wiqo team. When it cannot be
    /// sent (offline, signed out), or `save` is asked for, the zip goes to the Desktop instead.
    pub async fn report_problem(&self, report: ProblemReport) -> Result<ReportOutcome> {
        let summary = serde_json::json!({ "state": self.snapshot(), "permissions": self.permissions(), "category": report.category });
        let note = report.note.clone();
        let bytes =
            tokio::task::spawn_blocking(move || diagnostics::build(summary, &note)).await??;
        if report.save {
            let path = diagnostics::save(&bytes)?;
            return Ok(ReportOutcome {
                sent: false,
                saved_to: Some(path.to_string_lossy().to_string()),
            });
        }
        let token = self.chat.token_for_device().or_else(|| {
            self.runtime
                .credentials
                .get()
                .map(|credentials| credentials.access_token)
        });
        let sent = match token {
            Some(token) => {
                use base64::Engine as _;
                let body = serde_json::json!({
                    "category": report.category, "note": report.note, "contactOk": report.contact_ok,
                    "appVersion": env!("CARGO_PKG_VERSION"), "platform": platform_name(self.runtime.engine.platform()),
                    "diagnostics": report.include_logs.then(|| base64::engine::general_purpose::STANDARD.encode(&bytes)),
                });
                reqwest::Client::new()
                    .post(format!(
                        "{}/desktop-feedback",
                        self.server.trim_end_matches('/')
                    ))
                    .bearer_auth(token)
                    .timeout(Duration::from_secs(60))
                    .json(&body)
                    .send()
                    .await
                    .is_ok_and(|response| response.status().is_success())
            }
            None => false,
        };
        if sent {
            return Ok(ReportOutcome {
                sent: true,
                saved_to: None,
            });
        }
        let path = diagnostics::save(&bytes)?;
        Ok(ReportOutcome {
            sent: false,
            saved_to: Some(path.to_string_lossy().to_string()),
        })
    }

    /// Sends one chat message; the answer arrives as `Event::Chat` with this `turn`, ending with `turnEnd` (and an error text when it failed).
    pub async fn chat_send(
        &self,
        turn: &str,
        conversation: &str,
        history: serde_json::Value,
        text: &str,
        answer_to: Option<(String, String)>,
        attachments: serde_json::Value,
    ) {
        let device = self.runtime.identity.device_id.clone();
        let outcome = self
            .chat
            .send(
                &self.runtime.engine,
                &device,
                &self.shared.events,
                turn,
                conversation,
                history,
                text,
                answer_to,
                attachments,
            )
            .await;
        let error = outcome.err().map(|error| error.to_string());
        let _ = self.shared.events.send(Event::Chat {
            turn: turn.to_string(),
            event: serde_json::json!({ "type": "turnEnd", "error": error }),
        });
    }

    /// Sends one message to an agent thread; the answer arrives as `Event::Thread` with this `turn`, ending with `turnEnd` (and an error text when it failed).
    pub async fn thread_send(&self, turn: &str, thread: &str, text: &str) {
        let events = self.shared.events.clone();
        let (turn_id, thread_id) = (turn.to_string(), thread.to_string());
        let emit = move |event: serde_json::Value| {
            let _ = events.send(Event::Thread {
                turn: turn_id.clone(),
                thread: thread_id.clone(),
                event,
            });
        };
        let request_id = uuid::Uuid::new_v4().to_string();
        let outcome = self
            .chat
            .thread_stream(thread, &request_id, text, &emit)
            .await;
        let error = outcome.err().map(|error| error.to_string());
        emit(serde_json::json!({ "type": "turnEnd", "error": error }));
    }

    /// Signs in to the person's Wiqo account and makes this computer one of the account's devices (online at once, no phone needed).
    pub async fn sign_in(self: &Arc<Self>, email: &str, password: &str) -> Result<String> {
        let email = self.chat.sign_in(email, password).await?;
        self.link_device().await?;
        Ok(email)
    }

    pub async fn register(
        self: &Arc<Self>,
        email: &str,
        password: &str,
        invite: &str,
    ) -> Result<String> {
        let email = self.chat.register(email, password, invite).await?;
        self.link_device().await?;
        Ok(email)
    }

    /// Google sign-in through the browser and a loopback address; then this computer joins the account. `cancel` stops the wait.
    pub async fn google_sign_in(
        self: &Arc<Self>,
        open: impl Fn(&str),
        cancel: &Notify,
    ) -> Result<String> {
        let email = self.chat.google_sign_in(open, cancel).await?;
        self.link_device().await?;
        Ok(email)
    }

    pub async fn google_finish(self: &Arc<Self>, callback: &str) -> Result<String> {
        let email = self.chat.google_finish(callback).await?;
        self.link_device().await?;
        Ok(email)
    }

    pub async fn reset_password(
        self: &Arc<Self>,
        email: &str,
        code: &str,
        password: &str,
    ) -> Result<String> {
        let email = self.chat.reset_password(email, code, password).await?;
        self.link_device().await?;
        Ok(email)
    }

    /// The account's session becomes this computer's device session (`/device-sessions/bootstrap`), then the connection starts.
    async fn link_device(self: &Arc<Self>) -> Result<()> {
        let token = self
            .chat
            .token_for_device()
            .ok_or_else(|| anyhow::anyhow!("Önce giriş yap."))?;
        let tokens = Api::new(&self.server)
            .bootstrap(&token, &self.runtime.identity.device_id)
            .await
            .map_err(|error| anyhow::anyhow!("Bu bilgisayar hesabına bağlanamadı: {error}"))?;
        self.runtime.credentials.set(Credentials {
            api_base: self.server.clone(),
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            access_expires_at: chrono::Utc::now().timestamp() + tokens.access_token_expires_in,
        })?;
        self.disconnect();
        self.connect();
        let _ = self.shared.events.send(Event::Phones);
        Ok(())
    }

    /// Signs out: the connection stops and the device session is forgotten; paired phones stay listed for the next sign-in.
    pub fn sign_out(&self) -> Result<()> {
        self.chat.sign_out()?;
        self.disconnect();
        self.runtime.credentials.clear()?;
        self.shared.state(&ConnectionState::NotPaired);
        Ok(())
    }

    pub async fn chat_confirm(
        &self,
        confirmation: &serde_json::Value,
        approved: bool,
    ) -> Result<serde_json::Value> {
        let phones = self.runtime.phones.list();
        self.chat
            .confirm(
                &phones,
                &self.runtime.identity.device_id,
                confirmation,
                approved,
            )
            .await
    }

    /// "Geri al": takes back one command's changes, or (with `task`) every change of that task, newest first. Each record restores only
    /// when the file still holds exactly what the change left, so newer work is never overwritten. Returns what happened, in one sentence.
    pub fn undo(&self, key: Option<&str>, task: Option<&str>) -> Result<String> {
        let targets: Vec<(String, Vec<UndoRef>)> = self
            .shared
            .activity
            .lock()
            .expect("activity lock")
            .iter()
            .filter(|item| !item.undone && !item.undo.is_empty())
            .filter(|item| {
                key.is_some_and(|key| item.key == key)
                    || task.is_some_and(|task| item.task.as_deref() == Some(task))
            })
            .map(|item| (item.key.clone(), item.undo.clone()))
            .collect();
        if targets.is_empty() {
            bail!("Geri alınacak bir değişiklik yok.");
        }
        let services = self.runtime.engine.services().clone();
        let context = wiqo_core::engine::Context {
            command_id: uuid::Uuid::new_v4(),
            services: services.clone(),
        };
        let (mut done, mut failed) = (0, Vec::new());
        // The activity list is newest first: undoing in that order unwinds the task backwards.
        for (item_key, refs) in &targets {
            let mut all = true;
            for undo in refs.iter().rev() {
                let outcome = match undo.kind {
                    "terminal" => wiqo_ops::terminal::undo_recorded(&services, &undo.id),
                    _ => wiqo_ops::undo_file(&context, &undo.id),
                };
                match outcome {
                    Ok(_) => done += 1,
                    Err(error) => {
                        all = false;
                        failed.push(error.message);
                    }
                }
            }
            if all {
                if let Some(item) = self
                    .shared
                    .activity
                    .lock()
                    .expect("activity lock")
                    .iter_mut()
                    .find(|item| item.key == *item_key)
                {
                    item.undone = true;
                }
            }
        }
        let _ = self.shared.events.send(Event::Phones);
        match failed.first() {
            None => Ok(format!("{done} değişiklik geri alındı.")),
            Some(reason) if done > 0 => Ok(format!(
                "{done} değişiklik geri alındı; {} tanesi alınamadı: {reason}",
                failed.len()
            )),
            Some(reason) => bail!("{reason}"),
        }
    }

    /// The "Geri al" on a chat card: a file change or a terminal session's edit by its record id.
    pub fn undo_record(&self, kind: &str, id: &str) -> Result<String> {
        let services = self.runtime.engine.services().clone();
        let outcome = match kind {
            "terminal" => wiqo_ops::terminal::undo_recorded(&services, id),
            _ => {
                let context = wiqo_core::engine::Context {
                    command_id: uuid::Uuid::new_v4(),
                    services,
                };
                wiqo_ops::undo_file(&context, id)
            }
        };
        outcome.map_err(|error| anyhow::anyhow!(error.message))
    }

    /// The answer to a "PC'de onayla" card.
    pub fn answer_approval(&self, id: &str, allow: bool) {
        if let Some(answer) = self
            .shared
            .approvals
            .lock()
            .expect("approvals lock")
            .remove(id)
        {
            let _ = answer.send(allow);
        }
    }

    pub fn set_approve_on_pc(&self, on: bool) -> Result<()> {
        self.runtime
            .engine
            .settings()
            .update(|data| data.approve_on_pc = on)?;
        Ok(())
    }

    /// "Okuma adımlarını telefonsuz çalıştır": the server hears it at once (the registration's `agent.readOnly`).
    pub fn set_reads_without_phone(&self, on: bool) -> Result<()> {
        self.runtime
            .engine
            .settings()
            .update(|data| data.reads_without_phone = on)?;
        self.reannounce();
        Ok(())
    }

    /// Where the on-device speech model lives (next to the other data of this computer).
    pub fn voice_dir(&self) -> std::path::PathBuf {
        DataDir::default_location()
            .map(|dir| dir.file("voice"))
            .unwrap_or_else(|_| std::env::temp_dir().join("wiqo-voice"))
    }

    /// The questions waiting for a click right now, with the seconds each has left.
    pub fn pending_approvals(&self) -> Vec<PendingApproval> {
        self.shared
            .open_questions
            .lock()
            .expect("questions lock")
            .iter()
            .map(|(question, raised)| PendingApproval {
                seconds: APPROVAL_SECONDS.saturating_sub(raised.elapsed().as_secs()),
                ..question.clone()
            })
            .collect()
    }

    /// What runs right now.
    pub fn working(&self) -> Vec<WorkItem> {
        self.shared.working.lock().expect("working lock").clone()
    }

    /// The system permissions and where they stand (never shows a dialog).
    pub fn permissions(&self) -> Vec<permissions::Permission> {
        self.permissions.list()
    }

    /// Asks for one permission (a system dialog or the right Settings page). Blocks while a dialog is open.
    pub fn request_permission(&self, id: &str) -> Vec<permissions::Permission> {
        self.permissions.request(id);
        // What the server may plan follows the permissions: register again with the new state.
        self.reannounce();
        self.permissions.list()
    }

    /// Permissions also change in System Settings, outside Wiqo: checked every minute, and the server hears when the blocked set moves.
    pub fn watch_permissions(self: &Arc<Self>) {
        let desktop = Arc::downgrade(self);
        std::thread::spawn(move || {
            let mut last = None;
            loop {
                let Some(desktop) = desktop.upgrade() else {
                    return;
                };
                let mut now: Vec<String> = desktop.permissions.blockers().into_keys().collect();
                now.sort();
                if last.as_ref().is_some_and(|last| *last != now) {
                    desktop.reannounce();
                }
                last = Some(now);
                drop(desktop);
                std::thread::sleep(Duration::from_secs(60));
            }
        });
    }

    fn reannounce(&self) {
        self.runtime.engine.services().access.announce();
    }

    pub fn snapshot(&self) -> Snapshot {
        let engine = &self.runtime.engine;
        let settings = engine.settings();
        let manifest = Manifest::get();
        let mut capabilities: Vec<Capability> = engine
            .capabilities()
            .into_iter()
            .map(|id| {
                let asks = manifest.operations().iter().any(|operation| {
                    operation.capability == id && operation.requires_user_presence
                });
                Capability {
                    enabled: settings.capability_enabled(&id),
                    asks,
                    id,
                }
            })
            .collect();
        capabilities.sort_by(|a, b| a.id.cmp(&b.id));
        Snapshot {
            device_name: self.runtime.identity.display_name.clone(),
            device_id: self.runtime.identity.device_id.clone(),
            platform: platform_name(engine.platform()).to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            server: self.server.clone(),
            paired: self.is_paired(),
            connection: self
                .shared
                .connection
                .lock()
                .expect("connection lock")
                .clone(),
            phones: self
                .runtime
                .phones
                .list()
                .into_iter()
                .map(|phone| PhoneInfo {
                    pairing_id: phone.pairing_id,
                    name: phone.name,
                    platform: phone.platform,
                })
                .collect(),
            full_access: settings.full_access(),
            approve_on_pc: settings.approve_on_pc(),
            reads_without_phone: settings.reads_without_phone(),
            account: self.chat.email(),
            capabilities,
            activity: self
                .shared
                .activity
                .lock()
                .expect("activity lock")
                .iter()
                .cloned()
                .collect(),
        }
    }

    pub fn set_full_access(&self, on: bool) -> Result<()> {
        self.runtime
            .engine
            .settings()
            .update(|data| data.full_access = on)?;
        self.reannounce();
        Ok(())
    }

    pub fn set_capability(&self, id: &str, enabled: bool) -> Result<()> {
        self.runtime.engine.settings().update(|data| {
            if enabled {
                data.disabled_capabilities.remove(id);
            } else {
                data.disabled_capabilities.insert(id.to_string());
            }
        })?;
        self.reannounce();
        Ok(())
    }

    /// Opens a pairing session on the server and returns the code to show (as text and as a QR picture).
    pub async fn start_pairing(&self) -> Result<PairingOffer> {
        let api = Api::new(&self.server);
        let identity = &self.runtime.identity;
        let session = api
            .create_pairing_session(
                &identity.device_id,
                &identity.display_name,
                platform_name(self.runtime.engine.platform()),
            )
            .await?;
        // Phones decode the date with ISO-8601 without fractional seconds (Swift's `.iso8601`); the server writes milliseconds, so the code carries whole seconds.
        let expires = whole_seconds(&session.expires_at);
        let payload = serde_json::json!({ "version": 1, "sessionID": session.session_id, "secret": session.secret, "expiresAt": expires });
        let code = format!(
            "rova-relay:v1:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string())
        );
        let qr_svg = qrcode::QrCode::new(code.as_bytes())?
            .render::<qrcode::render::svg::Color>()
            .min_dimensions(220, 220)
            .quiet_zone(true)
            .build();
        Ok(PairingOffer {
            code,
            qr_svg,
            expires_at: session.expires_at,
            session_id: session.session_id,
            secret: session.secret,
        })
    }

    /// Waits until a phone claims the offer (or it expires), stores the phone's key and the server tokens, and connects.
    pub async fn finish_pairing(self: &Arc<Self>, offer: &PairingOffer) -> Result<PhoneInfo> {
        let api = Api::new(&self.server);
        let deadline = std::time::Instant::now() + Duration::from_secs(600);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(1_000)).await;
            match api.poll_pairing(&offer.session_id, &offer.secret).await? {
                PollResult::Waiting => continue,
                PollResult::Claimed(claim) => {
                    tracing::info!(
                        "pairing claimed by {} ({})",
                        claim.phone.name,
                        claim.phone.platform
                    );
                    let phone = PairedPhone {
                        pairing_id: claim.pairing_id.clone(),
                        name: claim.phone.name.clone(),
                        platform: claim.phone.platform.clone(),
                        token: claim.pairing_token.clone(),
                    };
                    self.runtime.phones.add(phone)?;
                    self.runtime.credentials.set(Credentials {
                        api_base: self.server.clone(),
                        access_token: claim.device_access_token.clone(),
                        refresh_token: claim.device_refresh_token.clone(),
                        access_expires_at: chrono::Utc::now().timestamp() + 14 * 60,
                    })?;
                    let _ = self.shared.events.send(Event::Paired {
                        phone: claim.phone.name.clone(),
                    });
                    let _ = self.shared.events.send(Event::Phones);
                    // A new pairing issues new tokens: a connection that was open with the old ones starts again.
                    self.disconnect();
                    self.connect();
                    return Ok(PhoneInfo {
                        pairing_id: claim.pairing_id,
                        name: claim.phone.name,
                        platform: claim.phone.platform,
                    });
                }
            }
        }
        bail!("Eşleştirme kodunun süresi doldu; yeni bir kod oluşturun.")
    }

    /// Unpairs a phone: the server forgets the pairing and this desktop drops the phone's key at once.
    pub async fn remove_phone(&self, pairing_id: &str) -> Result<()> {
        if let Some(credentials) = self.runtime.credentials.get() {
            if let Err(error) = Api::new(&self.server)
                .revoke_phone(&credentials.access_token, pairing_id)
                .await
            {
                tracing::warn!("server did not confirm the unpairing: {error}");
            }
        }
        self.runtime.phones.remove(pairing_id)?;
        let _ = self.shared.events.send(Event::Phones);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_pairing_code_carries_a_date_phones_can_decode() {
        assert_eq!(
            super::whole_seconds("2026-10-20T17:00:00.123Z"),
            "2026-10-20T17:00:00Z"
        );
        assert_eq!(
            super::whole_seconds("2026-10-20T17:00:00Z"),
            "2026-10-20T17:00:00Z"
        );
    }
}
