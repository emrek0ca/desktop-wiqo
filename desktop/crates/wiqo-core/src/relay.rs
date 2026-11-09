//! The connection to the server: register this desktop, take the phones' frames, send frames back, and stay connected.

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
use crate::engine::Engine;
use crate::storage::{CredentialStore, Credentials, Identity, PairedPhones};
use crate::terminal::Outbox;
use crate::verify::{signer_of, NonceCache, Rejection};
use crate::wire::{CommandResult, CommandStatus, Request, WireError};

const PROTOCOL_VERSION: u32 = 2;
const REFRESH_MARGIN_SECONDS: i64 = 120;
/// How often the desktop asks for its due automation steps while connected.
const AUTOMATION_PULL: Duration = Duration::from_secs(15);

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
    /// A frame from a paired phone whose signature was checked: a message or an approval answer (contracts/companion/frames.json). A PC
    /// screen's own command never comes here: it runs and is answered on the relay.
    fn remote(&self, _request: Request) {}
    /// Due automation steps the server handed over on this desktop's own question (`automationPull`): each in the chat's step shape, to run
    /// and answer with `automation_done`.
    fn automation(&self, _steps: Vec<(i64, Value)>) {}
    /// Signed-in pages due for a check, handed over with the same answer (`{triggerId, kind, url, selector}`): read each in Wiqo's own browser
    /// profile and answer with `watchObserved`. The server compares; nothing is decided here.
    fn watches(&self, _watches: Vec<Value>) {}
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
    /// The open socket to the server: frames for the phones and this desktop's own questions leave through it.
    pub outbox: Outbox,
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
    // Only what this desktop really does: saving into a sub-folder of the Desktop.
    // "desktop.approvalGate": this companion asks before a press that cannot be taken back. The server sends desktop actions only to one that does.
    let mut features: Vec<&str> = vec!["artifact-directory-v1", "desktop.approvalGate"];
    let names = engine.operation_names();
    // fileUtility's inspect and moveDocuments actions: the server only plans them for a desktop that says it has them.
    if names.contains(&"fileUtility") {
        features.extend(["file-inspect-v1", "bulk-document-move-v1"]);
    }
    let terminal = engine.terminal();
    if let Some(terminal) = terminal {
        features.extend(terminal.features());
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
        "cloudFolders": crate::cloud_folders::cloud_folders(),
        "wake": crate::wake::wake_targets(),
    })
}

enum Incoming {
    PairingRevoked(String),
    /// A frame from a phone, through the server's pipe (contracts/companion/frames.json).
    Frame(Value),
    /// The answer to this desktop's `automationPull`: due automation steps and due signed-in page checks.
    AutomationSteps(Vec<(i64, Value)>, Vec<Value>),
    Ignored,
}

fn parse_incoming(text: &str) -> Incoming {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Incoming::Ignored;
    };
    let kind = value["type"].as_str().unwrap_or("").to_string();
    match kind.as_str() {
        "companionFrame" if value["frame"].is_object() => Incoming::Frame(value["frame"].clone()),
        "automationSteps" => Incoming::AutomationSteps(
            value["steps"]
                .as_array()
                .map(|steps| {
                    steps
                        .iter()
                        .filter_map(|item| Some((item["stepId"].as_i64()?, item["step"].clone())))
                        .collect()
                })
                .unwrap_or_default(),
            value["watches"]
                .as_array()
                .map(|watches| {
                    watches
                        .iter()
                        .filter(|watch| watch["triggerId"].is_i64() && watch["url"].as_str().is_some_and(|url| url.starts_with("https://")))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
        ),
        "pairingRevoked" => value["pairingId"]
            .as_str()
            .map(|id| Incoming::PairingRevoked(id.to_string()))
            .unwrap_or(Incoming::Ignored),
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

/// A frame for the person's phones (contracts/companion/frames.json): the server passes it on as it is. `notify` (an approval's id) asks the server for
/// the generic approval push, for a phone whose app is closed.
pub fn to_phone(frame: Value, notify: Option<&str>) -> Value {
    match notify {
        Some(id) => json!({ "type": "companionFrame", "frame": frame, "notify": id }),
        None => json!({ "type": "companionFrame", "frame": frame }),
    }
}

/// None when a paired phone signed this command (signed command v2 with its pairing key and a fresh nonce), else the refusal to send back.
fn unsigned(runtime: &Runtime, request: &Request, raw_payload: &Value) -> Option<CommandResult> {
    let phones = runtime.phones.list();
    let keys = phones
        .iter()
        .map(|phone| (phone.pairing_id.as_str(), phone.token.as_str()));
    signer_of(request, raw_payload, keys, &runtime.nonces, Utc::now())
        .err()
        .map(|rejection| match rejection {
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
        })
}

/// One frame from a phone. A message or an approval answer goes to the observer once its signature holds; anything
/// else is a PC screen's own command: it runs here as that phone's command and its result goes back as a `commandResult` frame.
async fn phone_frame(runtime: &Runtime, observer: &dyn Observer, frame: Value) {
    let Ok(request) = serde_json::from_value::<Request>(frame.clone()) else {
        tracing::warn!("frame: unreadable, dropped");
        return;
    };
    if wiqo_contract::frames::Frames::get().is_to_desktop(&request.kind) {
        match unsigned(runtime, &request, &frame["payload"]) {
            None => observer.remote(request),
            Some(refused) => tracing::warn!(
                "frame {} ({}): {}",
                request.id,
                request.kind,
                refused.error.map(|error| error.code).unwrap_or_default()
            ),
        }
        return;
    }
    let command_id = request.id.to_string();
    let clock = std::time::Instant::now();
    observer.started(
        &command_id,
        &request.kind,
        &crate::engine::describe(&frame["payload"]),
        None,
    );
    let kind = request.kind.clone();
    let result = match unsigned(runtime, &request, &frame["payload"]) {
        None => {
            let mut request = request;
            vouch_for_person(&mut request);
            runtime.engine.execute(&request).await
        }
        Some(refused) => refused,
    };
    log_result(&command_id, &kind, &result, clock.elapsed());
    runtime
        .outbox
        .deliver(&to_phone(
            json!({ "kind": wiqo_contract::frames::COMMAND_RESULT, "commandId": command_id, "result": result }),
            None,
        ))
        .await;
    observer.command(&command_id, &kind, &result);
    observer.finished(&command_id);
}

fn refusal(request_id: Uuid, code: &str, message: &str, category: &str) -> CommandResult {
    CommandResult::failed(
        request_id,
        CommandStatus::Failed,
        WireError::new(code, message, category),
        Utc::now(),
    )
}

/// What a desktop action may say about the person is decided here and nowhere else: `person` only inside a phone's own signature, and never
/// `cardApproved` (only this computer's own chat sets it, after the person's yes). Whatever else came in that field is dropped.
fn vouch_for_person(request: &mut Request) {
    if request.kind != "computerDesktop" {
        return;
    }
    if let Some(value) = request
        .payload
        .get_mut("value")
        .and_then(Value::as_object_mut)
    {
        value.remove("cardApproved");
    }
}

pub struct Relay {
    runtime: Arc<Runtime>,
    api: Api,
    observer: Arc<dyn Observer>,
    refresh: Arc<Notify>,
}

impl Relay {
    pub fn new(runtime: Arc<Runtime>, api_base: &str) -> Relay {
        let refresh = runtime.engine.services().announce.clone();
        Relay {
            runtime,
            api: Api::new(api_base),
            observer: Arc::new(Silent),
            refresh,
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
            .map_err(|error| {
                if error.is_unauthorized() {
                    let _ = self
                        .runtime
                        .engine
                        .settings()
                        .update(|data| data.connection_stopped = true);
                    let _ = store.clear();
                }
                format!("token refresh: {error}")
            })?;
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

        // Frames for the phones leave through one channel, so any number of commands can run at once and still write to the socket one at a time.
        let (outgoing, mut answers) = mpsc::channel::<String>(64);
        self.runtime.outbox.attach(Some(outgoing));
        let result = self.serve(&mut sink, &mut stream, &mut answers).await;
        self.runtime.outbox.attach(None);
        result
    }

    async fn serve<S, R>(
        &self,
        sink: &mut S,
        stream: &mut R,
        answers: &mut mpsc::Receiver<String>,
    ) -> Result<(), String>
    where
        S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
        R: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + Unpin,
    {
        // This desktop asks for its due automation steps now and then; the server never sends work on its own.
        let mut pull = tokio::time::interval(AUTOMATION_PULL);
        loop {
            tokio::select! {
                _ = pull.tick() => {
                    sink.send(Message::text(json!({ "type": "automationPull" }).to_string())).await.map_err(|error| error.to_string())?;
                }
                _ = self.refresh.notified() => {
                    sink.send(Message::text(registration(&self.runtime).to_string())).await.map_err(|error| error.to_string())?;
                }
                Some(text) = answers.recv() => {
                    sink.send(Message::text(text)).await.map_err(|error| error.to_string())?;
                }
                message = stream.next() => {
                    let Some(message) = message else { return Err("the server closed the connection".into()) };
                    match message.map_err(|error| error.to_string())? {
                        Message::Text(text) => self.dispatch(text.as_str()),
                        Message::Close(frame) => return Err(format!("closed by the server: {frame:?}")),
                        _ => {}
                    }
                }
            }
        }
    }

    fn dispatch(&self, text: &str) {
        match parse_incoming(text) {
            Incoming::AutomationSteps(steps, watches) => {
                if !steps.is_empty() {
                    self.observer.automation(steps);
                }
                if !watches.is_empty() {
                    self.observer.watches(watches);
                }
            }
            Incoming::Frame(frame) => {
                let (runtime, observer) = (self.runtime.clone(), self.observer.clone());
                tokio::spawn(async move { phone_frame(&runtime, observer.as_ref(), frame).await });
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

#[cfg(test)]
mod frame_tests {
    use super::*;
    use std::sync::Mutex;

    struct Recorder(Mutex<Vec<String>>);
    impl Observer for Recorder {
        fn remote(&self, request: Request) {
            self.0.lock().unwrap().push(request.kind);
        }
    }

    fn runtime() -> Runtime {
        let dir = crate::storage::DataDir::new(
            std::env::temp_dir().join(format!("wiqo-frames-{}", Uuid::new_v4())),
        )
        .unwrap();
        let phones = PairedPhones::open(&dir);
        phones
            .add(crate::storage::PairedPhone {
                pairing_id: "p1".into(),
                name: "Telefon".into(),
                platform: "iOS".into(),
                token: "pairing-key".into(),
            })
            .unwrap();
        Runtime {
            identity: Identity {
                device_id: "PC".into(),
                display_name: "PC".into(),
            },
            outbox: Outbox::default(),
            credentials: Arc::new(CredentialStore::open(&dir)),
            phones,
            engine: Engine::new(
                wiqo_contract::Platform::current(),
                Arc::new(crate::storage::Settings::open(&dir)),
                Arc::new(crate::ledger::Ledger::open(&dir)),
                crate::engine::Services::temporary(),
            ),
            nonces: NonceCache::default(),
        }
    }

    /// A frame as a phone sends it: a signed command v2 with its pairing key.
    fn frame(key: &str, kind: &str, payload: Value) -> Value {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let epoch_ms = now.timestamp_millis();
        let nonce = Uuid::new_v4().to_string();
        let signature = wiqo_contract::command_signature(
            key,
            &id.to_string(),
            kind,
            epoch_ms as u64,
            &nonce,
            &payload,
        )
        .unwrap();
        json!({ "id": id, "type": kind, "payload": payload, "createdAt": crate::wire::timestamp(now), "signatureVersion": 2,
            "createdAtEpochMs": epoch_ms, "nonce": nonce, "signature": signature })
    }

    fn message() -> Value {
        json!({ "kind": "remoteMessage", "value": { "requestId": "request-0001", "messages": [{ "role": "user", "content": "selam" }] } })
    }

    #[tokio::test]
    async fn a_paired_phones_signed_message_reaches_the_chat_and_a_replay_or_a_stranger_does_not() {
        let runtime = runtime();
        let recorder = Recorder(Mutex::new(Vec::new()));
        let signed = frame("pairing-key", "remoteMessage", message());
        phone_frame(&runtime, &recorder, signed.clone()).await;
        phone_frame(&runtime, &recorder, signed).await;
        phone_frame(
            &runtime,
            &recorder,
            frame("another-key", "remoteMessage", message()),
        )
        .await;
        let mut forged = frame(
            "pairing-key",
            "approvalDecision",
            json!({ "kind": "approvalDecision", "value": { "approvalId": "a", "approved": true } }),
        );
        forged["payload"]["value"]["approved"] = json!(false);
        phone_frame(&runtime, &recorder, forged).await;
        assert_eq!(
            *recorder.0.lock().unwrap(),
            vec!["remoteMessage".to_string()]
        );
    }

    #[tokio::test]
    async fn a_pc_screens_command_is_answered_with_a_command_result_frame() {
        let runtime = runtime();
        let (sender, mut sent) = mpsc::channel(4);
        runtime.outbox.attach(Some(sender));
        let recorder = Recorder(Mutex::new(Vec::new()));
        let command = frame("pairing-key", "listFiles", json!({ "kind": "none" }));
        phone_frame(&runtime, &recorder, command.clone()).await;
        let answer: Value = serde_json::from_str(&sent.recv().await.unwrap()).unwrap();
        assert_eq!(answer["type"], "companionFrame");
        assert_eq!(answer["frame"]["kind"], "commandResult");
        assert_eq!(answer["frame"]["commandId"], command["id"]);
        assert!(answer["frame"]["result"]["status"].is_string());
        assert!(
            recorder.0.lock().unwrap().is_empty(),
            "a command is not a chat message"
        );

        let unsigned = json!({ "id": Uuid::new_v4(), "type": "listFiles", "payload": { "kind": "none" }, "createdAt": crate::wire::timestamp(Utc::now()) });
        phone_frame(&runtime, &recorder, unsigned).await;
        let refused: Value = serde_json::from_str(&sent.recv().await.unwrap()).unwrap();
        assert_eq!(
            refused["frame"]["result"]["error"]["code"],
            codes::SIGNATURE_VERSION_REQUIRED
        );
    }

    #[test]
    fn only_a_companion_frame_with_a_body_is_a_frame() {
        assert!(matches!(
            parse_incoming(r#"{"type":"companionFrame","frame":{"id":"x"}}"#),
            Incoming::Frame(_)
        ));
        assert!(matches!(
            parse_incoming(r#"{"type":"companionFrame","frame":"x"}"#),
            Incoming::Ignored
        ));
        match parse_incoming(
            r#"{"type":"automationSteps","steps":[{"stepId":4,"step":{"id":"c"}},{"step":{}}]}"#,
        ) {
            Incoming::AutomationSteps(steps, watches) => {
                assert_eq!(steps, vec![(4, json!({ "id": "c" }))], "a step without its id is dropped");
                assert!(watches.is_empty());
            }
            _ => panic!("automationSteps"),
        }
        match parse_incoming(
            r#"{"type":"automationSteps","steps":[],"watches":[{"triggerId":7,"kind":"page","url":"https://mhrs.gov.tr/"},{"triggerId":8,"url":"http://x.test"}]}"#,
        ) {
            Incoming::AutomationSteps(_, watches) => assert_eq!(watches.len(), 1, "only an https page with its trigger is read"),
            _ => panic!("automationSteps"),
        }
        assert_eq!(to_phone(json!({"kind":"ack"}), Some("a1"))["notify"], "a1");
        assert!(to_phone(json!({"kind":"ack"}), None)
            .get("notify")
            .is_none());
    }
}
