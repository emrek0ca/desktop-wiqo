//! The connection to the server: register this desktop, receive commands, send answers, and stay connected.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Notify};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;
use wiqo_contract::Platform;

use crate::api::Api;
use crate::codes;
use crate::engine::{Engine, Origin};
use crate::storage::{CredentialStore, Credentials, Identity, PairedPhones};
use crate::terminal::{self, Outbox, Session, CODE_CAPABILITY};
use crate::verify::{signer_of, NonceCache, Rejection};
use crate::wire::{CommandResult, CommandStatus, Request, WireError};

const PROTOCOL_VERSION: u32 = 2;
/// Features the server may rely on, announced at registration (only those this desktop really has).
const FEATURE_FULL_ACCESS: &str = "agent.fullAccess";
const FEATURE_TASK_GRANT: &str = "agent.taskGrant";
const FEATURE_TASK_ACTIVE: &str = "agent.taskActive";
const REFRESH_MARGIN_SECONDS: i64 = 120;

/// Where the connection to the server stands, for whoever shows it (the app window, the tray, the CLI log).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", content = "detail", rename_all = "camelCase")]
pub enum ConnectionState {
    NotPaired,
    Connecting,
    Online,
    /// Disconnected; retrying after a pause. The detail says why.
    Retrying(String),
    Stopped,
}

/// Told about what happens on the connection. Every method has a do-nothing default.
pub trait Observer: Send + Sync {
    fn state(&self, _state: &ConnectionState) {}
    /// One command finished: its relay id, its type and its answer.
    fn command(&self, _key: &str, _kind: &str, _result: &CommandResult) {}
    fn phones_changed(&self) {}
    /// A command started (`key` is its relay id; `detail` a short line about what it touches) and, later, ended.
    /// `task` groups the commands of one task: the run id the server sends, or a terminal session's grant id.
    fn started(&self, _key: &str, _kind: &str, _detail: &str, _task: Option<&str>) {}
    fn finished(&self, _key: &str) {}
}

struct Silent;
impl Observer for Silent {}

/// Everything one running desktop needs.
pub struct Runtime {
    pub identity: Identity,
    pub credentials: Arc<CredentialStore>,
    pub phones: PairedPhones,
    pub engine: Engine,
    pub nonces: NonceCache,
}

pub fn platform_name(platform: Platform) -> &'static str {
    match platform {
        Platform::MacOs => "macOS",
        Platform::Windows => "windows",
        Platform::Linux => "linux",
    }
}

fn registration(runtime: &Runtime) -> Value {
    let engine = &runtime.engine;
    // Only what this desktop really does: saving into a sub-folder of the Desktop, taking task grants, and full access or a running grant.
    let mut features: Vec<&str> = vec!["artifact-directory-v1"];
    let names = engine.operation_names();
    if names.contains(&"grantAgentAccess") {
        features.push(FEATURE_TASK_GRANT);
    }
    // fileUtility's inspect and moveDocuments actions: the server only plans them for a desktop that says it has them.
    if names.contains(&"fileUtility") {
        features.extend(["file-inspect-v1", "bulk-document-move-v1"]);
    }
    let terminal = engine
        .terminal()
        .filter(|_| engine.settings().capability_enabled(CODE_CAPABILITY));
    if let Some(terminal) = terminal {
        features.extend(terminal.features());
        if engine.settings().full_access() {
            features.push("terminal-full-access-v1");
        }
    }
    // The server may run own-folder reads here without the phone's round trip (grants.rs `read_without_phone`).
    if engine.settings().reads_without_phone() {
        features.push("agent.readOnly");
    }
    if engine.settings().full_access() {
        features.push(FEATURE_FULL_ACCESS);
    } else if engine.services().access.active().is_some() {
        features.push(FEATURE_TASK_ACTIVE);
    }
    json!({
        "type": "register",
        "protocolVersion": PROTOCOL_VERSION,
        "deviceId": runtime.identity.device_id,
        "displayName": runtime.identity.display_name,
        "platform": platform_name(engine.platform()),
        "appVersion": env!("CARGO_PKG_VERSION"),
        "capabilities": engine.capabilities(),
        "features": features,
        "terminalProfile": terminal.and_then(|terminal| terminal.profile()).unwrap_or(Value::Null),
        "applicationTargets": engine.application_targets(),
        "authorities": engine.authorities(),
    })
}

enum Incoming {
    Command {
        command_id: String,
        request: Value,
        origin_is_agent: bool,
        /// Which run, step and role the command belongs to; echoed back with the answer, never signed.
        run: Value,
    },
    Terminal {
        command_id: String,
        request: Value,
    },
    PairingRevoked(String),
    Ignored,
}

fn parse_incoming(text: &str) -> Incoming {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Incoming::Ignored;
    };
    let kind = value["type"].as_str().unwrap_or("").to_string();
    let command_id = value["commandId"].as_str().map(str::to_string);
    match kind.as_str() {
        "interactiveCommand" | "agentCommand" => match command_id {
            Some(command_id) => Incoming::Command {
                command_id,
                request: value["request"].clone(),
                origin_is_agent: kind == "agentCommand",
                run: value["run"].clone(),
            },
            None => Incoming::Ignored,
        },
        "pairingRevoked" => value["pairingId"]
            .as_str()
            .map(|id| Incoming::PairingRevoked(id.to_string()))
            .unwrap_or(Incoming::Ignored),
        "terminalCommand" => match command_id {
            Some(command_id) => Incoming::Terminal {
                command_id,
                request: value["request"].clone(),
            },
            None => Incoming::Ignored,
        },
        _ => Incoming::Ignored,
    }
}

/// One line per finished command: what, how it ended, the error code, how long it took (desktop.log; read by "Sorun bildir").
fn log_result(command_id: &str, kind: &str, result: &CommandResult, took: Duration) {
    let code = result
        .error
        .as_ref()
        .map(|error| error.code.as_str())
        .unwrap_or("-");
    tracing::info!(
        "result {command_id} {kind} status={:?} code={code} ms={}",
        result.status,
        took.as_millis()
    );
}

fn answer(command_id: &str, result: &CommandResult) -> String {
    json!({ "type": "interactiveCommandResult", "commandId": command_id, "result": result })
        .to_string()
}

fn answer_with_run(command_id: &str, result: &CommandResult, run: &Value) -> String {
    if run.is_null() {
        return answer(command_id, result);
    }
    json!({ "type": "interactiveCommandResult", "commandId": command_id, "result": result, "run": run })
        .to_string()
}

/// One terminal request: the session it may use (the grant check sees the phones paired right now), then the terminal's own answer.
pub async fn handle_terminal(
    runtime: &Arc<Runtime>,
    outbox: &Outbox,
    command_id: &str,
    request: &Value,
) -> Option<CommandResult> {
    let terminal = runtime.engine.terminal()?.clone();
    let digest = wiqo_contract::canonical_json(request)
        .map(|text| hex::encode(<sha2::Sha256 as sha2::Digest>::digest(text.as_bytes())))
        .unwrap_or_default();
    // The server's own full-access grant changes files with no phone in the loop: "PC'de onayla" holds such a step for a click here.
    let action = request["action"].as_str().unwrap_or("run");
    let reads = matches!(action, "read_file" | "search" | "get_job" | "cancel_job");
    if !reads && request["grant"]["signature"] == terminal::LOCAL_FULL_ACCESS {
        let detail = request["command"]
            .as_str()
            .or(request["filePath"].as_str())
            .unwrap_or(action)
            .chars()
            .take(120)
            .collect::<String>();
        if !runtime
            .engine
            .approved_on_pc(&format!("terminal.{action}"), &detail)
            .await
        {
            let refused = WireError::new(
                codes::DENIED_ON_PC,
                "Bu adım PC'de onaylanmadı; işlem yapılmadı.",
                "permission",
            );
            return Some(CommandResult::failed(
                Uuid::new_v4(),
                CommandStatus::Denied,
                refused,
                Utc::now(),
            ));
        }
    }
    let checker = runtime.clone();
    let session = Session {
        services: runtime.engine.services().clone(),
        grants: Arc::new(move |grant| {
            terminal::accepts(grant, &checker.phones.list(), checker.engine.settings())
        }),
        outbox: outbox.clone(),
        code_enabled: runtime
            .engine
            .settings()
            .capability_enabled(CODE_CAPABILITY),
    };
    Some(terminal.handle(session, command_id, request, &digest).await)
}

fn refusal(request_id: Uuid, code: &str, message: &str, category: &str) -> CommandResult {
    CommandResult::failed(
        request_id,
        CommandStatus::Failed,
        WireError::new(code, message, category),
        Utc::now(),
    )
}

/// Checks and runs one command from the server, returning the text to send back.
pub async fn handle_command(
    runtime: &Runtime,
    command_id: &str,
    raw_request: Value,
    from_agent: bool,
) -> String {
    handle(runtime, command_id, raw_request, from_agent).await.0
}

async fn handle(
    runtime: &Runtime,
    command_id: &str,
    raw_request: Value,
    from_agent: bool,
) -> (String, Option<(String, CommandResult)>) {
    let request: Request = match serde_json::from_value(raw_request.clone()) {
        Ok(request) => request,
        Err(_) => {
            return (
                answer(
                    command_id,
                    &refusal(
                        Uuid::nil(),
                        codes::INVALID_PAYLOAD,
                        "Komut çözülemedi; Wiqo'yu güncelleyin.",
                        "validation",
                    ),
                ),
                None,
            )
        }
    };
    let origin = if from_agent {
        Origin::Agent
    } else {
        let phones = runtime.phones.list();
        let keys = phones
            .iter()
            .map(|phone| (phone.pairing_id.as_str(), phone.token.as_str()));
        match signer_of(
            &request,
            &raw_request["payload"],
            keys,
            &runtime.nonces,
            Utc::now(),
        ) {
            Ok(pairing_id) => Origin::Phone { pairing_id },
            Err(rejection) => {
                let refused = match rejection {
                    Rejection::VersionRequired => refusal(
                        request.id,
                        codes::SIGNATURE_VERSION_REQUIRED,
                        "Güvenli komut imzası için Wiqo uygulamasını güncelleyin.",
                        "permission",
                    ),
                    Rejection::Invalid => refusal(
                        request.id,
                        codes::INVALID_SIGNATURE,
                        "Komut kriptografik imzası geçersiz.",
                        "permission",
                    ),
                };
                return (
                    answer(command_id, &refused),
                    Some((request.kind.clone(), refused)),
                );
            }
        }
    };
    let result = runtime.engine.execute(&request, &origin).await;
    (answer(command_id, &result), Some((request.kind, result)))
}

pub struct Relay {
    runtime: Arc<Runtime>,
    api: Api,
    observer: Arc<dyn Observer>,
    refresh: Arc<Notify>,
    outbox: Outbox,
}

impl Relay {
    pub fn new(runtime: Arc<Runtime>, api_base: &str) -> Relay {
        let refresh = runtime.engine.services().access.changed();
        Relay {
            runtime,
            api: Api::new(api_base),
            observer: Arc::new(Silent),
            refresh,
            outbox: Outbox::default(),
        }
    }

    pub fn with_observer(mut self, observer: Arc<dyn Observer>) -> Relay {
        self.observer = observer;
        self
    }

    /// Notify this to register again on the open connection (after a setting changed what the desktop offers).
    pub fn refresher(&self) -> Arc<Notify> {
        self.refresh.clone()
    }

    /// Stays connected until the task is dropped: reconnects with a growing pause, renews the access token when it is about to end.
    pub async fn run(&self) {
        let mut pause = Duration::from_secs(1);
        loop {
            if self.runtime.credentials.get().is_none() {
                self.observer.state(&ConnectionState::NotPaired);
                return;
            }
            self.observer.state(&ConnectionState::Connecting);
            match self.session().await {
                Ok(()) => pause = Duration::from_secs(1),
                Err(error) => {
                    tracing::warn!("relay: {error}");
                    self.observer.state(&ConnectionState::Retrying(error));
                }
            }
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(Duration::from_secs(30));
        }
    }

    async fn fresh_token(&self) -> Result<String, String> {
        let store = &self.runtime.credentials;
        let credentials = store.get().ok_or("this desktop is not paired yet")?;
        if credentials.access_expires_at - Utc::now().timestamp() > REFRESH_MARGIN_SECONDS {
            return Ok(credentials.access_token);
        }
        let tokens = self
            .api
            .refresh(&self.runtime.identity.device_id, &credentials.refresh_token)
            .await
            .map_err(|error| format!("token refresh: {error}"))?;
        let updated = Credentials {
            api_base: credentials.api_base,
            access_token: tokens.access_token.clone(),
            refresh_token: tokens.refresh_token,
            access_expires_at: Utc::now().timestamp() + tokens.access_token_expires_in,
        };
        store
            .set(updated)
            .map_err(|error| format!("saving tokens: {error}"))?;
        Ok(tokens.access_token)
    }

    async fn session(&self) -> Result<(), String> {
        let token = self.fresh_token().await?;
        let mut request = self
            .api
            .socket_url()
            .into_client_request()
            .map_err(|error| error.to_string())?;
        request.headers_mut().insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|error| error.to_string())?,
        );
        let (socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|error| format!("connect: {error}"))?;
        let (mut sink, mut stream) = socket.split();
        sink.send(Message::text(registration(&self.runtime).to_string()))
            .await
            .map_err(|error| error.to_string())?;
        tracing::info!(
            "connected to the relay as {}",
            self.runtime.identity.device_id
        );
        self.observer.state(&ConnectionState::Online);

        // Answers leave through one channel, so any number of commands can run at once and still write to the socket one at a time.
        let (outgoing, mut answers) = mpsc::channel::<String>(64);
        self.outbox.attach(Some(outgoing.clone()));
        let result = self
            .serve(&mut sink, &mut stream, &outgoing, &mut answers)
            .await;
        self.outbox.attach(None);
        result
    }

    async fn serve<S, R>(
        &self,
        sink: &mut S,
        stream: &mut R,
        outgoing: &mpsc::Sender<String>,
        answers: &mut mpsc::Receiver<String>,
    ) -> Result<(), String>
    where
        S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
        R: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + Unpin,
    {
        loop {
            tokio::select! {
                _ = self.refresh.notified() => {
                    sink.send(Message::text(registration(&self.runtime).to_string())).await.map_err(|error| error.to_string())?;
                }
                Some(text) = answers.recv() => {
                    sink.send(Message::text(text)).await.map_err(|error| error.to_string())?;
                }
                message = stream.next() => {
                    let Some(message) = message else { return Err("the server closed the connection".into()) };
                    match message.map_err(|error| error.to_string())? {
                        Message::Text(text) => self.dispatch(text.as_str(), outgoing),
                        Message::Close(frame) => return Err(format!("closed by the server: {frame:?}")),
                        _ => {}
                    }
                }
            }
        }
    }

    fn dispatch(&self, text: &str, outgoing: &mpsc::Sender<String>) {
        match parse_incoming(text) {
            Incoming::Command {
                command_id,
                request,
                origin_is_agent,
                run,
            } => {
                let (runtime, outgoing, observer) = (
                    self.runtime.clone(),
                    outgoing.clone(),
                    self.observer.clone(),
                );
                tokio::spawn(async move {
                    tracing::info!(
                        "command {command_id} ({})",
                        request["type"].as_str().unwrap_or("?")
                    );
                    observer.started(
                        &command_id,
                        request["type"].as_str().unwrap_or("?"),
                        &crate::engine::describe(&request["payload"]),
                        run["id"].as_str(),
                    );
                    let clock = std::time::Instant::now();
                    let (reply, finished) =
                        handle(&runtime, &command_id, request, origin_is_agent).await;
                    if let Some((kind, result)) = &finished {
                        log_result(&command_id, kind, result, clock.elapsed());
                    }
                    let reply = match &finished {
                        Some((_, result)) if !run.is_null() => {
                            answer_with_run(&command_id, result, &run)
                        }
                        _ => reply,
                    };
                    let _ = outgoing.send(reply).await;
                    if let Some((kind, result)) = finished {
                        observer.command(&command_id, &kind, &result);
                    }
                    observer.finished(&command_id);
                });
            }
            Incoming::Terminal {
                command_id,
                request,
            } => {
                let (runtime, outgoing, observer, outbox) = (
                    self.runtime.clone(),
                    outgoing.clone(),
                    self.observer.clone(),
                    self.outbox.clone(),
                );
                tokio::spawn(async move {
                    let clock = std::time::Instant::now();
                    let action = request["action"].as_str().unwrap_or("run").to_string();
                    tracing::info!("terminal {command_id} ({action})");
                    let detail: String = request["command"]
                        .as_str()
                        .or(request["filePath"].as_str())
                        .unwrap_or("")
                        .chars()
                        .take(120)
                        .collect();
                    observer.started(
                        &command_id,
                        &format!("terminal.{action}"),
                        &detail,
                        request["grant"]["id"].as_str(),
                    );
                    let result =
                        match handle_terminal(&runtime, &outbox, &command_id, &request).await {
                            Some(result) => result,
                            None => refusal(
                                Uuid::nil(),
                                codes::UNSUPPORTED_OPERATION,
                                "Terminal oturumları bu Wiqo sürümünde yok.",
                                "validation",
                            ),
                        };
                    log_result(
                        &command_id,
                        &format!("terminal.{action}"),
                        &result,
                        clock.elapsed(),
                    );
                    let _ = outgoing.send(answer(&command_id, &result)).await;
                    observer.command(&command_id, &format!("terminal.{action}"), &result);
                    observer.finished(&command_id);
                });
            }
            Incoming::PairingRevoked(pairing_id) => match self.runtime.phones.remove(&pairing_id) {
                Ok(true) => {
                    tracing::info!("phone {pairing_id} was unpaired; its key is gone");
                    self.observer.phones_changed();
                }
                Ok(false) => {}
                Err(error) => tracing::warn!("could not drop the key of {pairing_id}: {error}"),
            },
            Incoming::Ignored => {}
        }
    }
}
