//! One Wiqo desktop, ready to use from a window or a terminal: it opens its state, pairs with a phone, keeps the server connection, and tells what happens.
//! The app (Tauri) and the `wiqo-agent` command are two faces of this same object; neither has logic of its own.

pub use wiqo_ops::{set_cursor_sink, CursorEvent};

pub mod chat;
pub mod diagnostics;
pub mod location;
pub mod permissions;
mod remote_screen;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use base64::Engine as _;
use serde::Serialize;
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinHandle;
use wiqo_contract::Platform;
use wiqo_core::api::{Api, PollResult};
use wiqo_core::engine::{Engine, Services};
use wiqo_core::ledger::Ledger;
use wiqo_core::relay::{platform_name, ConnectionState, Observer, Relay, Runtime};
use wiqo_core::storage::{
    CredentialStore, Credentials, DataDir, Identity, PairedPhone, PairedPhones, Settings,
};
use wiqo_core::verify::NonceCache;
use wiqo_core::wire::{CommandResult, CommandStatus, Request};

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
    RunSync {
        account: String,
        event: serde_json::Value,
    },
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
    /// The signed-in Wiqo account, if any.
    pub account: Option<String>,
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

/// A "PC'de onayla" question that is still open: what the corner card fetches when it opens, because the card's window may not exist
/// yet when the question is raised (the first question of a session used to be lost, and the step was refused for no answer).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingApproval {
    pub id: String,
    pub kind: String,
    pub detail: String,
    pub seconds: u64,
    /// The phone's message this step belongs to: the question is on that phone too.
    #[serde(skip)]
    request_id: Option<String>,
}

struct Shared {
    events: broadcast::Sender<Event>,
    connection: Mutex<ConnectionState>,
    activity: Mutex<VecDeque<Activity>>,
    working: Mutex<Vec<WorkItem>>,
    approvals: Mutex<
        std::collections::HashMap<String, tokio::sync::oneshot::Sender<(bool, &'static str)>>,
    >,
    open_questions: Mutex<Vec<(PendingApproval, std::time::Instant)>>,
    /// Kinds the person just allowed: the same kind is not asked again for two minutes, so a task of thirty moves is one question, not thirty.
    allowed_kinds: Mutex<std::collections::HashMap<String, std::time::Instant>>,
    permissions: Arc<permissions::Permissions>,
    /// When each permission was last asked for after a failed step: one ask per two minutes, never a dialog storm.
    asked_at: Mutex<std::collections::HashMap<&'static str, std::time::Instant>>,
    /// What the relay hands to this computer's own work, for `Desktop::serve_relay`.
    inbox: tokio::sync::mpsc::UnboundedSender<Inbox>,
}

impl Shared {
    fn announce_work(&self) {
        let items = self.working.lock().expect("working lock").clone();
        let _ = self.events.send(Event::Working { items });
    }
}

impl Observer for Shared {
    fn remote(&self, request: Request) {
        let _ = self.inbox.send(Inbox::Phone(request));
    }

    fn remote_screen(&self, session: wiqo_core::relay::RemoteSession) {
        let _ = self.inbox.send(Inbox::RemoteScreen(session));
    }

    fn automation(&self, steps: Vec<(i64, serde_json::Value)>) {
        let _ = self.inbox.send(Inbox::Automation(steps));
    }

    fn watches(&self, watches: Vec<serde_json::Value>) {
        let _ = self.inbox.send(Inbox::Watches(watches));
    }

    fn agent_steps(&self, units: Vec<serde_json::Value>) {
        let _ = self.inbox.send(Inbox::AgentSteps(units));
    }

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

/// An open question as the phone reads it (frames.json `approvalRequest`). `push` asks for the server's generic approval push, for a phone whose app is
/// closed: when the question is raised, not when a phone asks what is still open.
fn approval_request(question: &PendingApproval, seconds_left: u64, push: bool) -> serde_json::Value {
    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(seconds_left as i64);
    wiqo_core::relay::to_phone(
        serde_json::json!({ "kind": wiqo_contract::frames::APPROVAL_REQUEST, "approvalId": question.id, "requestId": question.request_id,
            "operation": question.kind, "detail": question.detail, "expiresAt": expires_at.to_rfc3339() }),
        push.then_some(question.id.as_str()),
    )
}

/// A task's progress as its Live Activity shows it: the step it is on, how many steps it has had, and whether it waits for a yes or ended.
struct ActivityLine {
    title: String,
    detail: String,
    steps: std::collections::BTreeMap<String, bool>,
    failed: bool,
    last_sent: Option<std::time::Instant>,
}

impl ActivityLine {
    fn new(title: String) -> ActivityLine {
        ActivityLine { title, detail: String::new(), steps: Default::default(), failed: false, last_sent: None }
    }

    fn state(&self, status: &str) -> serde_json::Value {
        let done = self.steps.values().filter(|done| **done).count();
        serde_json::json!({ "title": self.title, "detail": self.detail, "step": done, "total": self.steps.len(), "status": status })
    }

    /// The state to send for this event, when it changes what the activity shows and the last one is not too recent.
    fn read(&mut self, event: &serde_json::Value) -> Option<serde_json::Value> {
        let urgent = match event["type"].as_str() {
            Some("step_update") => {
                let id = event["step"]["id"].as_str().unwrap_or("").to_string();
                let status = event["step"]["status"].as_str().unwrap_or("running");
                if let Some(summary) = event["step"]["summary"].as_str().filter(|summary| !summary.is_empty()) {
                    self.detail = summary.chars().take(120).collect();
                }
                if matches!(status, "failed" | "denied") {
                    self.failed = true;
                }
                self.steps.insert(id, status == "completed");
                false
            }
            Some("approval") | Some("confirmation_needed") => {
                self.detail = "Onayını bekliyor".into();
                return self.send("waiting", true);
            }
            Some("error") => {
                self.failed = true;
                false
            }
            _ => return None,
        };
        self.send("running", urgent)
    }

    fn send(&mut self, status: &str, urgent: bool) -> Option<serde_json::Value> {
        if !urgent && self.last_sent.is_some_and(|at| at.elapsed() < Duration::from_secs(2)) {
            return None;
        }
        self.last_sent = Some(std::time::Instant::now());
        Some(self.state(status))
    }

    fn end(&self) -> serde_json::Value {
        let mut state = self.state(if self.failed { "failed" } else { "done" });
        if !self.failed {
            state["detail"] = serde_json::json!("Bitti");
        }
        state
    }
}

/// What the relay hands to this computer's own work.
enum Inbox {
    /// A paired phone's checked frame.
    Phone(Request),
    /// Due automation steps: the server's step id and the step.
    Automation(Vec<(i64, serde_json::Value)>),
    /// Signed-in pages due for a check (the server's watchers).
    Watches(Vec<serde_json::Value>),
    /// Background tasks' steps to run on this computer (rova-backend agent/pcUnits.ts).
    AgentSteps(Vec<serde_json::Value>),
    /// A paired phone opened the live remote screen (frames.json remoteScreen, signature checked by the relay).
    RemoteScreen(wiqo_core::relay::RemoteSession),
}

/// A phone's `remoteMessage` (frames.json): the request it would have sent to `/copilot/chat` itself.
struct RemoteMessage {
    request_id: String,
    conversation: String,
    history: serde_json::Value,
    text: String,
    task: Option<String>,
    question: Option<String>,
    attachments: serde_json::Value,
    /// The whole `/copilot/chat` body the phone would have sent.
    request: serde_json::Value,
}

impl RemoteMessage {
    fn read(value: &serde_json::Value) -> Option<RemoteMessage> {
        let plain = |text: &str, max: usize| {
            !text.is_empty()
                && text.len() <= max
                && text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
        };
        let request_id = value["requestId"]
            .as_str()
            .filter(|id| id.len() >= 8 && plain(id, 128))?
            .to_string();
        let conversation = value["conversationId"].as_str().unwrap_or("default");
        let conversation = plain(conversation, 128).then(|| conversation.to_string())?;
        let mut messages = value["messages"].as_array()?.clone();
        let last = messages.pop()?;
        if last["role"] != "user" {
            return None;
        }
        let text = last["content"].as_str()?.to_string();
        let id = |key: &str| {
            value[key]
                .as_str()
                .filter(|id| plain(id, 200))
                .map(str::to_string)
        };
        let (task, question) = (id("taskId"), id("questionId"));
        Some(RemoteMessage {
            request_id,
            conversation,
            history: serde_json::Value::Array(messages),
            text,
            task,
            question,
            attachments: value["attachments"].clone(),
            request: value.clone(),
        })
    }
}

pub struct Desktop {
    pub runtime: Arc<Runtime>,
    /// Taken once by `serve_relay`.
    inbox: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Inbox>>>,
    server: String,
    shared: Arc<Shared>,
    relay: Mutex<Option<(JoinHandle<()>, Arc<Notify>)>>,
    /// Taken once by `connect`: the job events the terminal emitted, forwarded to the server as `pcJobEvent`.
    job_events: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>>,
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
        wiqo_ops::set_full_access(settings.snapshot().full_access);
        // The keychain holds the tokens, except for a data folder of its own (tests, a second profile).
        let vault = Arc::new(wiqo_core::vault::Vault::new(
            std::env::var_os("WIQO_DATA_DIR").is_none(),
        ));
        let credentials = Arc::new(CredentialStore::open_with(&dir, vault.clone()));
        let services = Arc::new(Services {
            data_dir: dir.clone(),
            credentials: credentials.clone(),
            announce: Default::default(),
            job_events: Default::default(),
        });
        // Long jobs say when they started and ended; the events wait here until the relay is connected (`connect`).
        let (job_sender, job_reader) = tokio::sync::mpsc::unbounded_channel();
        services.job_events.attach(job_sender);
        let (events, _) = broadcast::channel(256);
        let permissions = Arc::new(permissions::Permissions::new(dir.file("permissions.json")));
        let (inbox, inbox_reader) = tokio::sync::mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            inbox,
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
        let mut engine = Engine::new(platform, settings, Arc::new(Ledger::open(&dir)), services.clone());
        wiqo_ops::register_all(&mut engine).map_err(anyhow::Error::msg)?;
        // Jobs the last run left open are over: they are told as "not known" (the process did not survive the restart).
        if let Some(terminal) = engine.terminal() {
            terminal.recover(&services);
        }
        let probe = permissions.clone();
        engine.set_blockers(Arc::new(move || probe.blockers()));
        let runtime = Arc::new(Runtime {
            identity,
            outbox: Default::default(),
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
            inbox: Mutex::new(Some(inbox_reader)),
            chat,
            permissions,
            runtime,
            server,
            shared,
            relay: Mutex::new(None),
            job_events: Mutex::new(Some(job_reader)),
        }))
    }

    pub async fn watch_runs(&self) {
        self.chat.watch_runs(&self.shared.events).await;
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
    /// Reuses device credentials, or repairs only a first, interrupted account link.
    pub async fn restore_connection(self: &Arc<Self>) {
        if self.runtime.engine.settings().snapshot().connection_stopped {
            return;
        }
        if self.is_paired() {
            self.connect();
            return;
        }
        if self.chat.token_for_device().is_none() {
            return;
        }
        self.shared.state(&ConnectionState::Connecting);
        let owner = self.chat.token_for_device();
        if self.link_device_mode(true).await.is_err()
            && self.chat.token_for_device() == owner
            && !self.is_paired()
        {
            self.shared.state(&ConnectionState::NotPaired);
        }
    }

    pub fn connect(self: &Arc<Self>) {
        let _ = self
            .runtime
            .engine
            .settings()
            .update(|data| data.connection_stopped = false);
        let mut relay = self.relay.lock().expect("relay lock");
        if relay.as_ref().is_some_and(|(task, _)| !task.is_finished()) || !self.is_paired() {
            return;
        }
        let connection =
            Relay::new(self.runtime.clone(), &self.server).with_observer(self.shared.clone());
        let refresher = connection.refresher();
        let task = tokio::spawn(async move { connection.run().await });
        *relay = Some((task, refresher));
        if let Some(mut reader) = self.job_events.lock().expect("job events lock").take() {
            let runtime = self.runtime.clone();
            tokio::spawn(async move {
                while let Some(event) = reader.recv().await {
                    let mut frame = event;
                    if let Some(object) = frame.as_object_mut() {
                        object.insert("type".into(), "pcJobEvent".into());
                    }
                    // The phone's own copy of a finished job (its app may be open): a companion frame, the server records the job from the first one.
                    runtime.outbox.deliver(&frame).await;
                    if frame["phase"] == "finished" {
                        runtime
                            .outbox
                            .deliver(&wiqo_core::relay::to_phone(
                                serde_json::json!({ "kind": wiqo_contract::frames::JOB_FINISHED, "job": frame }),
                                None,
                            ))
                            .await;
                    }
                }
            });
        }
    }

    /// Stops the connection: no command reaches this computer until `connect` again.
    pub fn disconnect(&self) {
        let _ = self
            .runtime
            .engine
            .settings()
            .update(|data| data.connection_stopped = true);
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

    /// "Durdur": what this computer is doing stops — screen control and background terminal jobs at once.
    pub fn stop_work(&self) {
        wiqo_ops::stop_desktop_control();
        if let Some(terminal) = self.runtime.engine.terminal() {
            terminal.cancel_all();
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
        self: &Arc<Self>,
        turn: &str,
        conversation: &str,
        history: serde_json::Value,
        text: &str,
        answer_to: Option<(String, String)>,
        attachments: serde_json::Value,
        mission: Option<String>,
    ) {
        let device = self.runtime.identity.device_id.clone();
        let outcome = self
            .chat
            .send(
                &self.runtime.engine,
                &device,
                &self.shared.events,
                turn,
                uuid::Uuid::new_v4().to_string(),
                conversation,
                history,
                text,
                answer_to.as_ref().map(|(task, _)| task.clone()),
                answer_to.map(|(_, question)| question),
                attachments,
                &chat::Asker::Window { mission },
                &self.approver(None),
            )
            .await;
        let error = outcome.err().map(|error| error.to_string());
        let _ = self.shared.events.send(Event::Chat {
            turn: turn.to_string(),
            event: serde_json::json!({ "type": "turnEnd", "error": error }),
        });
    }

    /// The yes a turn's steps wait for (`ask`), for the window's turn or a phone's message (`request_id`).
    fn approver(self: &Arc<Self>, request_id: Option<String>) -> Box<chat::Approve> {
        let desktop = self.clone();
        Box::new(move |kind: String, detail: String| {
            let (desktop, request_id) = (desktop.clone(), request_id.clone());
            Box::pin(async move {
                desktop
                    .ask_decision(&kind, &detail, request_id.as_deref())
                    .await
            })
        })
    }

    /// What the relay hands to this computer, for as long as the app runs: a paired phone's frame (contracts/companion/frames.json: a message is
    /// acknowledged at once and answered here, every event passed to the phone; an approval answer), or due automation steps it asked for.
    pub async fn serve_relay(self: &Arc<Self>) {
        let Some(mut inbox) = self.inbox.lock().expect("inbox lock").take() else {
            return;
        };
        while let Some(item) = inbox.recv().await {
            let request = match item {
                Inbox::Phone(request) => request,
                Inbox::Automation(steps) => {
                    let desktop = self.clone();
                    tokio::spawn(async move { desktop.run_automation(steps).await });
                    continue;
                }
                Inbox::AgentSteps(units) => {
                    let desktop = self.clone();
                    tokio::spawn(async move { desktop.run_agent_steps(units).await });
                    continue;
                }
                Inbox::Watches(watches) => {
                    let desktop = self.clone();
                    tokio::spawn(async move { desktop.read_watches(watches).await });
                    continue;
                }
                // The phone's live remote screen: shown in this window's work list while it runs, so the person here always sees it.
                Inbox::RemoteScreen(session) => {
                    let shared = self.shared.clone();
                    tokio::spawn(remote_screen::serve(session, move |on| {
                        let key = "remote-screen";
                        if on {
                            wiqo_core::relay::Observer::started(shared.as_ref(), key, "remoteScreen", "Telefondan uzaktan kontrol", None);
                        } else {
                            wiqo_core::relay::Observer::finished(shared.as_ref(), key);
                        }
                    }));
                    continue;
                }
            };
            let value = &request.payload["value"];
            match request.kind.as_str() {
                wiqo_contract::frames::REMOTE_MESSAGE => {
                    let desktop = self.clone();
                    tokio::spawn(async move { desktop.remote_turn(request).await });
                }
                wiqo_contract::frames::APPROVAL_DECISION => {
                    if let (Some(id), Some(approved)) =
                        (value["approvalId"].as_str(), value["approved"].as_bool())
                    {
                        self.answer(id, approved, "phone");
                    }
                }
                // "Durdur" on the phone: that message's turn stops here, with the screen work and terminal jobs this computer runs (as "Durdur" in
                // this window does). The phone tells the server to stop the task itself.
                wiqo_contract::frames::CANCEL_TURN => {
                    if let Some(id) = value["requestId"].as_str() {
                        if self.chat.stop_turn(id) {
                            self.stop_work();
                        } else {
                            tracing::info!("cancelTurn {id}: no such running turn");
                        }
                    }
                }
                // A phone that just opened asks what still waits for its yes: each open question of a phone's message or a background task is said
                // again, with the time it has left. A question of this window's own turn stays on this computer.
                wiqo_contract::frames::APPROVALS_OPEN => self.say_open_approvals().await,
                wiqo_contract::frames::CANCEL_JOB => {
                    if let (Some(id), Some(terminal)) = (value["jobId"].as_str(), self.runtime.engine.terminal()) {
                        if !terminal.cancel_job(id) {
                            tracing::info!("cancelJob {id}: no such running job");
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Every question that still waits for a phone's yes, said again to the phones with the time it has left and without a push.
    async fn say_open_approvals(&self) {
        for question in self.pending_approvals() {
            if question.request_id.is_some() {
                self.runtime
                    .outbox
                    .deliver(&approval_request(&question, question.seconds, false))
                    .await;
            }
        }
    }

    /// Background tasks' steps that need this computer: each runs as a chat turn held here, in the task's own conversation, with the same approval rules as
    /// a message typed here (the question is in this window and goes to the person's phone by push). When the turn ends the server is told on the socket;
    /// what the step achieved is read there from the turn's own record, not from this computer's word.
    async fn run_agent_steps(self: &Arc<Self>, units: Vec<serde_json::Value>) {
        for unit in units {
            let (Some(unit_id), Some(request_id), Some(conversation), Some(prompt)) = (unit["unitId"].as_i64(), unit["requestId"].as_str(), unit["conversationId"].as_str(), unit["prompt"].as_str()) else {
                continue;
            };
            let device = self.runtime.identity.device_id.clone();
            let turn = format!("agent-step-{unit_id}");
            let outcome = self
                .chat
                .send(
                    &self.runtime.engine,
                    &device,
                    &self.shared.events,
                    &turn,
                    request_id.to_string(),
                    conversation,
                    serde_json::json!([]),
                    prompt,
                    None,
                    None,
                    serde_json::json!([]),
                    &chat::Asker::Window { mission: None },
                    &self.approver(Some(request_id.to_string())),
                )
                .await;
            if let Err(error) = &outcome {
                tracing::warn!("background task step {unit_id} ended with: {error}");
            }
            let _ = self.shared.events.send(Event::Chat {
                turn: turn.clone(),
                event: serde_json::json!({ "type": "turnEnd", "error": outcome.err().map(|error| error.to_string()) }),
            });
            self.runtime
                .outbox
                .deliver(&serde_json::json!({ "type": "agentStepDone", "unitId": unit_id }))
                .await;
        }
    }

    /// Due automation steps: each runs here as the person's own automation and its receipt goes back on the socket. Nobody is there to say yes, so a
    /// step that changes, deletes or sends is refused (the server sends none; this is the computer's own rule).
    async fn run_automation(self: &Arc<Self>, steps: Vec<(i64, serde_json::Value)>) {
        for (step_id, step) in steps {
            let result = if chat::needs_yes(&step) {
                chat::refused_step(
                    &step,
                    "Otomasyonda onay verecek kimse yok; bu adım çalıştırılmadı.",
                )
            } else {
                chat::run_step(&self.runtime.engine, &step).await
            };
            self.runtime
                .outbox
                .deliver(&serde_json::json!({ "type": "automationStepDone", "stepId": step_id, "commandId": step["id"], "result": result }))
                .await;
        }
    }

    /// Signed-in pages the server's watchers asked to check: each is read in Wiqo's own browser profile on this computer (where the person signed in)
    /// and what it says goes back as `watchObserved`; the server compares it with the last reading. Nothing on the page is pressed or typed.
    async fn read_watches(self: &Arc<Self>, watches: Vec<serde_json::Value>) {
        for watch in watches {
            let Some(trigger) = watch["triggerId"].as_i64() else { continue };
            let url = watch["url"].as_str().unwrap_or("").to_string();
            let selector = watch["selector"].as_str().map(str::to_string);
            let read = tokio::task::spawn_blocking(move || wiqo_ops::read_watched_page(&url, selector.as_deref()))
                .await
                .unwrap_or_else(|_| Err("BROWSER_FAILED: okunamadı".into()));
            let answer = match read {
                Ok(page) => serde_json::json!({ "type": "watchObserved", "triggerId": trigger, "page": page }),
                Err(error) => serde_json::json!({ "type": "watchObserved", "triggerId": trigger, "error": error }),
            };
            self.runtime.outbox.deliver(&answer).await;
        }
    }

    /// One message from a phone: "aldım" first (the phone waits `ackSeconds` and then writes to the server itself), then the same turn as one typed
    /// here, with the phone's id, features and language, its steps run as that phone's, and the whole stream passed to the phone.
    async fn remote_turn(self: &Arc<Self>, request: Request) {
        let Some(message) = RemoteMessage::read(&request.payload["value"]) else {
            tracing::warn!("remote message {} unreadable, dropped", request.id);
            return;
        };
        let outbox = self.runtime.outbox.clone();
        let frame = |kind: &str, body: serde_json::Value| {
            let mut frame = serde_json::json!({ "kind": kind, "requestId": message.request_id });
            if let (Some(frame), Some(body)) = (frame.as_object_mut(), body.as_object()) {
                frame.extend(body.clone());
            }
            wiqo_core::relay::to_phone(frame, None)
        };
        outbox
            .deliver(&frame(wiqo_contract::frames::ACK, serde_json::json!({})))
            .await;
        // The stream leaves in order through one queue: a busy socket slows the phone's copy, it never drops an event of it.
        let (forward, mut queue) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
        let request_id = message.request_id.clone();
        let sender = outbox.clone();
        let title: String = message.text.chars().take(60).collect();
        let drain = tokio::spawn(async move {
            // The phone's Live Activity for this task moves on while its app is closed: how far the task got goes to the server, which passes it
            // to Apple as it is (rova-backend notifications/liveActivity.ts). At most every two seconds; the end at once.
            let mut activity = ActivityLine::new(title);
            while let Some(event) = queue.recv().await {
                if let Some(state) = activity.read(&event) {
                    sender.deliver(&serde_json::json!({ "type": "activityUpdate", "requestId": request_id, "state": state })).await;
                }
                sender
                    .deliver(&wiqo_core::relay::to_phone(
                        serde_json::json!({ "kind": wiqo_contract::frames::STREAM_EVENT, "requestId": request_id, "event": event }),
                        None,
                    ))
                    .await;
            }
            sender.deliver(&serde_json::json!({ "type": "activityUpdate", "requestId": request_id, "state": activity.end() })).await;
        });
        let asker = chat::Asker::Phone {
            request: message.request,
            forward: Box::new(move |event| {
                let _ = forward.send(event);
            }),
        };
        let device = self.runtime.identity.device_id.clone();
        let outcome = self
            .chat
            .send(
                &self.runtime.engine,
                &device,
                &self.shared.events,
                &message.request_id,
                message.request_id.clone(),
                &message.conversation,
                message.history,
                &message.text,
                message.task,
                message.question,
                message.attachments,
                &asker,
                &self.approver(Some(message.request_id.clone())),
            )
            .await;
        if let (Err(error), chat::Asker::Phone { forward, .. }) = (&outcome, &asker) {
            forward(serde_json::json!({ "type": "error", "message": error.to_string() }));
        }
        drop(asker);
        let _ = drain.await;
        outbox
            .deliver(&frame(
                wiqo_contract::frames::TURN_END,
                serde_json::json!({}),
            ))
            .await;
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
        self.link_device_mode(false).await
    }

    async fn link_device_mode(self: &Arc<Self>, restore_only: bool) -> Result<()> {
        let token = self
            .chat
            .token_for_device()
            .ok_or_else(|| anyhow::anyhow!("Önce giriş yap."))?;
        let tokens = Api::new(&self.server)
            .bootstrap(&token, &self.runtime.identity.device_id, restore_only)
            .await
            .map_err(|error| anyhow::anyhow!("Bu bilgisayar hesabına bağlanamadı: {error}"))?;
        if self.chat.token_for_device().as_deref() != Some(token.as_str()) {
            bail!("Hesap değişti; eski cihaz bağlantısı uygulanmadı.");
        }
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

    /// Signs out: the connection stops and the device session ends on the server too (best effort), so another account can sign in on this
    /// computer; paired phones stay listed for the next sign-in.
    pub async fn sign_out(&self) -> Result<()> {
        if let Some(credentials) = self.runtime.credentials.get() {
            let api = Api::new(&self.server);
            // The access token lives 15 minutes; an expired one is renewed once so the revoke is accepted.
            let token = if credentials.access_expires_at <= chrono::Utc::now().timestamp() + 30 {
                api.refresh(&self.runtime.identity.device_id, &credentials.refresh_token)
                    .await
                    .map(|tokens| tokens.access_token)
                    .unwrap_or(credentials.access_token)
            } else {
                credentials.access_token
            };
            if let Err(error) = api.revoke_session(&token).await {
                tracing::warn!("server did not confirm the sign-out: {error}");
            }
        }
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
        self.chat.confirm(confirmation, approved).await
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

    /// The "Geri al" on a chat card (file_undo): a file change by its record id. A terminal edit is undone from the activity list (`undo`).
    pub fn undo_record(&self, id: &str) -> Result<String> {
        let context = wiqo_core::engine::Context {
            command_id: uuid::Uuid::new_v4(),
            services: self.runtime.engine.services().clone(),
        };
        wiqo_ops::undo_file(&context, id).map_err(|error| anyhow::anyhow!(error.message))
    }

    /// Full access: whether the person turned it on for this computer.
    pub fn full_access(&self) -> bool {
        self.runtime.engine.settings().snapshot().full_access
    }

    /// Turned on or off here, the only place it can be turned on. It takes effect for the next step.
    pub fn set_full_access(&self, on: bool) -> Result<bool> {
        self.runtime.engine.settings().update(|data| data.full_access = on)?;
        wiqo_ops::set_full_access(on);
        Ok(on)
    }

    /// This computer's screen memory (wiqo_ops::screen_memory): its state, for the settings.
    pub fn screen_memory(&self) -> serde_json::Value {
        serde_json::to_value(wiqo_ops::screen_memory::status(self.data_path())).unwrap_or_default()
    }

    /// Turned on or off here, the only place it can be turned on. Off forgets everything read.
    pub fn set_screen_memory(&self, on: bool) -> Result<serde_json::Value> {
        wiqo_ops::screen_memory::set_enabled(self.data_path(), on)?;
        Ok(self.screen_memory())
    }

    /// `action`: "pause" (for `hours`, or until resumed), "resume", "forget"; `excluded`: the applications never read.
    pub fn screen_memory_action(&self, action: &str, hours: Option<i64>, excluded: Option<Vec<String>>) -> Result<serde_json::Value> {
        let data = self.data_path();
        match action {
            "pause" => drop(wiqo_ops::screen_memory::pause(data, hours)?),
            "resume" => drop(wiqo_ops::screen_memory::resume(data)?),
            "forget" => wiqo_ops::screen_memory::forget(data)?,
            "exclude" => drop(wiqo_ops::screen_memory::set_excluded(data, excluded.unwrap_or_default())?),
            _ => anyhow::bail!("Bu ekran hafızası işlemi tanınmıyor."),
        }
        Ok(self.screen_memory())
    }

    /// Which application the person used last before Wiqo, every second: a request typed in Wiqo's window is about that one (wiqo_ops::note_frontmost).
    pub async fn keep_front_app(self: &Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let _ = tokio::task::spawn_blocking(wiqo_ops::note_frontmost).await;
        }
    }

    /// The screen memory's reading every few seconds, off the async threads; it reads nothing while off or paused (wiqo_ops::screen_memory::tick).
    pub async fn keep_screen_memory(self: &Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            let desktop = self.clone();
            let _ = tokio::task::spawn_blocking(move || wiqo_ops::screen_memory::tick(desktop.data_path())).await;
        }
    }

    fn data_path(&self) -> &std::path::Path {
        self.runtime.engine.services().data_dir.path()
    }

    /// "Bir kez göster, Wiqo öğrensin": the recording starts (a second start keeps the first). Returns when it started.
    pub fn record_start(&self) -> String {
        wiqo_ops::recorder::start().to_rfc3339()
    }

    /// The recording stops; its steps come back for the person to read before anything leaves the computer.
    pub fn record_stop(&self) -> serde_json::Value {
        serde_json::to_value(wiqo_ops::recorder::stop()).unwrap_or_default()
    }

    pub fn record_cancel(&self) {
        wiqo_ops::recorder::cancel();
    }

    pub fn recording_since(&self) -> Option<String> {
        wiqo_ops::recorder::recording_since().map(|at| at.to_rfc3339())
    }

    /// The answer to a "PC'de onayla" card, clicked in this computer's panel.
    pub fn answer_approval(&self, id: &str, allow: bool) {
        self.answer(id, allow, "desktop");
    }

    /// The first answer to a question wins, from the panel or a phone; a later one finds nothing and is ignored, so a step is never run twice.
    fn answer(&self, id: &str, allow: bool, by: &'static str) {
        if let Some(answer) = self
            .shared
            .approvals
            .lock()
            .expect("approvals lock")
            .remove(id)
        {
            let _ = answer.send((allow, by));
        }
    }

    /// "PC'de onayla": a step that changes, deletes or sends waits for a yes. The question is in this computer's panel and, for a phone's message, on
    /// that phone at the same moment (with the generic approval push for a phone whose app is closed); whichever answers first decides, and both are
    /// told it is settled. No answer within `chatApprovalSeconds` is its own end (`Decision::TimedOut`), not a no: the person was away, and the task
    /// says so. Changes that can be taken back (a move, a copy, a write) share one yes for a short while; a delete or a send is asked every time.
    pub async fn ask(&self, kind: &str, detail: &str, request_id: Option<&str>) -> bool {
        self.ask_decision(kind, detail, request_id).await == chat::Decision::Yes
    }

    pub async fn ask_decision(
        &self,
        kind: &str,
        detail: &str,
        request_id: Option<&str>,
    ) -> chat::Decision {
        let batched = matches!(kind, "moveFile" | "copyFile" | "writeFile");
        if batched {
            let mut allowed = self.shared.allowed_kinds.lock().expect("allowed lock");
            allowed.retain(|_, at| at.elapsed() < Duration::from_secs(120));
            // One yes covers the same question again for a short while, never a different path, recipient or content.
            if allowed.contains_key(&format!("{kind}\u{1}{detail}")) {
                return chat::Decision::Yes;
            }
        }
        let seconds = wiqo_contract::frames::Frames::get().chat_approval_seconds;
        let id = uuid::Uuid::new_v4().to_string();
        let (answer, answered) = tokio::sync::oneshot::channel();
        self.shared
            .approvals
            .lock()
            .expect("approvals lock")
            .insert(id.clone(), answer);
        let question = PendingApproval {
            id: id.clone(),
            kind: kind.to_string(),
            detail: detail.to_string(),
            seconds,
            request_id: request_id.map(str::to_string),
        };
        self.shared
            .open_questions
            .lock()
            .expect("questions lock")
            .push((question.clone(), std::time::Instant::now()));
        let _ = self.shared.events.send(Event::Approval {
            id: id.clone(),
            kind: kind.to_string(),
            detail: detail.to_string(),
            seconds,
        });
        if let Some(request) = request_id {
            self.runtime
                .outbox
                .deliver(&approval_request(&question, seconds, true))
                .await;
            // The phone's Live Activity says it waits for a yes, with the lock screen's alert (rova-backend notifications/liveActivity.ts).
            self.runtime
                .outbox
                .deliver(&serde_json::json!({ "type": "activityUpdate", "requestId": request,
                    "state": { "detail": detail.lines().next().unwrap_or("").chars().take(120).collect::<String>(), "status": "waiting" } }))
                .await;
        }
        let (allowed, by) = match tokio::time::timeout(Duration::from_secs(seconds), answered).await
        {
            Ok(Ok(answer)) => answer,
            _ => (false, "timeout"),
        };
        self.shared
            .approvals
            .lock()
            .expect("approvals lock")
            .remove(&id);
        self.shared
            .open_questions
            .lock()
            .expect("questions lock")
            .retain(|(question, _)| question.id != id);
        if allowed && batched {
            self.shared
                .allowed_kinds
                .lock()
                .expect("allowed lock")
                .insert(format!("{kind}\u{1}{detail}"), std::time::Instant::now());
        }
        let _ = self
            .shared
            .events
            .send(Event::ApprovalClosed { id: id.clone() });
        if let Some(request_id) = request_id {
            self.runtime
                .outbox
                .deliver(&wiqo_core::relay::to_phone(
                    serde_json::json!({ "kind": wiqo_contract::frames::APPROVAL_RESOLVED, "approvalId": id, "requestId": request_id, "approved": allowed, "by": by }),
                    None,
                ))
                .await;
        }
        if allowed {
            chat::Decision::Yes
        } else if by == "timeout" {
            chat::Decision::TimedOut
        } else {
            chat::Decision::No
        }
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
                seconds: wiqo_contract::frames::Frames::get()
                    .chat_approval_seconds
                    .saturating_sub(raised.elapsed().as_secs()),
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
        self.runtime.engine.services().announce.notify_one();
    }

    pub fn snapshot(&self) -> Snapshot {
        let engine = &self.runtime.engine;
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
            account: self.chat.email(),
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
    use super::*;

    /// One desktop on a data folder of its own, with what it sends to the server captured.
    fn desktop() -> (Arc<Desktop>, tokio::sync::mpsc::Receiver<String>) {
        static FOLDER: std::sync::Once = std::sync::Once::new();
        FOLDER.call_once(|| {
            let folder =
                std::env::temp_dir().join(format!("wiqo-approvals-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&folder).unwrap();
            std::env::set_var("WIQO_DATA_DIR", folder);
        });
        // One at a time: the first opening writes the folder's files, the others read them.
        static OPENING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let desktop = {
            let _one = OPENING.lock().unwrap();
            Desktop::open(Some("http://127.0.0.1:1".into())).unwrap()
        };
        let (sender, sent) = tokio::sync::mpsc::channel(32);
        desktop.runtime.outbox.attach(Some(sender));
        (desktop, sent)
    }

    async fn open_question(desktop: &Arc<Desktop>) -> String {
        for _ in 0..200 {
            if let Some(question) = desktop.pending_approvals().first() {
                return question.id.clone();
            }
            tokio::task::yield_now().await;
        }
        panic!("no question was raised");
    }

    fn frames(sent: &mut tokio::sync::mpsc::Receiver<String>) -> Vec<serde_json::Value> {
        let mut frames = Vec::new();
        while let Ok(text) = sent.try_recv() {
            frames.push(serde_json::from_str::<serde_json::Value>(&text).unwrap());
        }
        frames
    }

    #[tokio::test]
    async fn the_first_answer_wins_and_a_second_one_changes_nothing() {
        let (desktop, mut sent) = desktop();
        let asking = desktop.clone();
        let decision = tokio::spawn(async move {
            asking
                .ask("deleteFile", "a.txt çöpe atılacak", Some("request-1"))
                .await
        });
        let id = open_question(&desktop).await;
        desktop.answer(&id, true, "phone");
        desktop.answer_approval(&id, false);
        assert!(decision.await.unwrap(), "the phone answered first");
        let frames = frames(&mut sent);
        let asked = frames
            .iter()
            .find(|frame| frame["frame"]["kind"] == "approvalRequest")
            .unwrap();
        assert_eq!(
            asked["notify"],
            id.as_str(),
            "a closed phone gets the generic push"
        );
        assert_eq!(asked["frame"]["operation"], "deleteFile");
        let settled = frames
            .iter()
            .find(|frame| frame["frame"]["kind"] == "approvalResolved")
            .unwrap();
        assert_eq!(settled["frame"]["approved"], true);
        assert_eq!(settled["frame"]["by"], "phone");
        assert!(desktop.pending_approvals().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn no_answer_in_time_refuses_the_step() {
        let (desktop, mut sent) = desktop();
        let asking = desktop.clone();
        let decision = tokio::spawn(async move {
            asking
                .ask("sendEmail", "E-posta gönderilecek", Some("request-2"))
                .await
        });
        let id = open_question(&desktop).await;
        tokio::time::advance(Duration::from_secs(
            wiqo_contract::frames::Frames::get().chat_approval_seconds + 1,
        ))
        .await;
        assert!(!decision.await.unwrap());
        desktop.answer_approval(&id, true);
        let settled = frames(&mut sent)
            .into_iter()
            .find(|frame| frame["frame"]["kind"] == "approvalResolved")
            .unwrap();
        assert_eq!(settled["frame"]["approved"], false);
        assert_eq!(settled["frame"]["by"], "timeout");
    }

    #[tokio::test]
    async fn a_phone_that_opens_late_is_told_the_question_that_still_waits_without_a_second_push() {
        let (desktop, mut sent) = desktop();
        let asking = desktop.clone();
        let decision = tokio::spawn(async move { asking.ask("deleteFile", "a.txt", Some("phone-request")).await });
        let id = open_question(&desktop).await;
        let first = frames(&mut sent);
        assert!(first.iter().any(|frame| frame["frame"]["kind"] == "approvalRequest" && frame["notify"] == id.as_str()));
        desktop.say_open_approvals().await;
        let again = frames(&mut sent);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0]["frame"]["kind"], "approvalRequest");
        assert_eq!(again[0]["frame"]["approvalId"], id.as_str());
        assert_eq!(again[0]["frame"]["requestId"], "phone-request");
        assert!(again[0].get("notify").is_none(), "asking again sends no second push");
        desktop.answer_approval(&id, true);
        assert!(decision.await.unwrap());
        desktop.say_open_approvals().await;
        assert!(frames(&mut sent).iter().all(|frame| frame["frame"]["kind"] != "approvalRequest"), "a settled question is not said again");
    }

    #[tokio::test]
    async fn a_question_of_this_window_stays_on_this_computer() {
        let (desktop, mut sent) = desktop();
        let asking = desktop.clone();
        let decision = tokio::spawn(async move { asking.ask("deleteFile", "b.txt", None).await });
        let id = open_question(&desktop).await;
        desktop.answer_approval(&id, false);
        assert!(!decision.await.unwrap());
        assert!(
            frames(&mut sent).is_empty(),
            "nothing goes to a phone for a message typed here"
        );
    }

    #[tokio::test]
    async fn an_automation_step_runs_here_and_its_receipt_goes_back_but_a_change_is_never_made_unattended(
    ) {
        let (desktop, mut sent) = desktop();
        let read = serde_json::json!({ "id": uuid::Uuid::new_v4().to_string(), "type": "getSystemStatus", "payload": { "kind": "none" }, "summary": "Durumu oku", "isDestructive": false });
        let change = serde_json::json!({ "id": uuid::Uuid::new_v4().to_string(), "type": "deleteFile", "payload": { "kind": "fileName", "value": "~/Desktop/a.txt" }, "summary": "Sil", "isDestructive": true });
        desktop
            .run_automation(vec![(7, read.clone()), (8, change.clone())])
            .await;
        let answers = frames(&mut sent);
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0]["type"], "automationStepDone");
        assert_eq!(answers[0]["stepId"], 7);
        assert_eq!(answers[0]["commandId"], read["id"]);
        assert!(answers[0]["result"]["status"].is_string());
        assert_eq!(answers[1]["stepId"], 8);
        assert_eq!(answers[1]["result"]["status"], "denied");
        assert_eq!(answers[1]["result"]["errorCode"], "DENIED_ON_PC");
    }

    #[test]
    fn the_pairing_code_carries_a_date_phones_can_decode() {
        assert_eq!(
            whole_seconds("2026-10-20T17:00:00.123Z"),
            "2026-10-20T17:00:00Z"
        );
        assert_eq!(
            whole_seconds("2026-10-20T17:00:00Z"),
            "2026-10-20T17:00:00Z"
        );
    }
}
