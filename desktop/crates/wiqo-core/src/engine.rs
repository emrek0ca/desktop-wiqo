//! Runs one command: is it known on this platform, is it allowed, was it already run, then the operation itself inside its lane and time budget.
//!
//! The order is fixed and is the same for every operation: unsupported -> not allowed -> already answered -> run (one lane at a time for anything that changes
//! something, several for reads) -> answer within the contract's time budget -> remember the answer.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, Semaphore};
use uuid::Uuid;
use wiqo_contract::{canonical_json, Manifest, Platform};

use crate::codes;
use crate::grants::Access;
use crate::ledger::Ledger;
use crate::storage::{CredentialStore, DataDir, Settings};
use crate::wire::{CommandResult, CommandStatus, Request, WireError};

/// How long a command that overran its budget may keep its lane before it is abandoned.
const GRACE: Duration = Duration::from_secs(30);
const READ_LANES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Looks only: several at once.
    ReadOnly,
    /// May change something: one at a time.
    SideEffect,
}

/// What an operation reports; the engine turns it into the contract's `CommandResult`.
pub struct Outcome {
    pub message: String,
    pub data: Option<Value>,
    pub error: Option<WireError>,
}

impl Outcome {
    pub fn ok(message: impl Into<String>, data: Value) -> Outcome {
        Outcome {
            message: message.into(),
            data: Some(data),
            error: None,
        }
    }

    pub fn failed(error: WireError) -> Outcome {
        Outcome {
            message: error.message.clone(),
            data: None,
            error: Some(error),
        }
    }
}

/// What an operation may use besides its payload: the id of the command it serves and the desktop's own services.
#[derive(Clone)]
pub struct Context {
    pub command_id: Uuid,
    pub services: Arc<Services>,
}

/// The desktop's own state that operations need: where it keeps files, and the tokens for talking to the server (saving an artifact downloads it from there).
pub struct Services {
    pub data_dir: DataDir,
    pub credentials: Arc<CredentialStore>,
    /// The phone's task grant for the server's agent (and the signal to register again).
    pub access: Arc<Access>,
}

impl Services {
    /// Services over a fresh temporary folder, for tests.
    pub fn temporary() -> Arc<Services> {
        let dir =
            DataDir::new(std::env::temp_dir().join(format!("wiqo-services-{}", Uuid::new_v4())))
                .expect("temporary folder");
        Arc::new(Services {
            credentials: Arc::new(CredentialStore::open(&dir)),
            data_dir: dir,
            access: Arc::new(Access::default()),
        })
    }
}

impl Context {
    /// A context over temporary services, for tests.
    pub fn for_tests() -> Context {
        Context {
            command_id: Uuid::new_v4(),
            services: Services::temporary(),
        }
    }
}

#[async_trait]
pub trait Operation: Send + Sync {
    /// The operation's name in `operations.json` (the command `type`).
    fn name(&self) -> &'static str;

    fn lane(&self) -> Lane {
        Lane::SideEffect
    }

    async fn run(&self, ctx: &Context, payload: &Value) -> Outcome;
}

/// Asks the person at this computer to allow one step of the server's agent ("PC'de onayla"). `false` (or no answer in time) refuses it.
#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, kind: &str, detail: &str) -> bool;
}

/// The name a person reads for a capability (the server shows it to the model and in the phone's PC sheet).
pub fn capability_name(capability: &str) -> &str {
    match capability {
        "runtime.status" => "Durum bilgisi",
        "device.files.list" => "Dosyaları listeleme ve arama",
        "device.files.read" => "Dosya okuma ve telefona gönderme",
        "device.files.write" => "Dosya yazma",
        "device.files.move" => "Dosya taşıma ve yeniden adlandırma",
        "device.files.copy" => "Dosya kopyalama ve PDF'e çevirme",
        "device.files.delete" => "Dosyaları çöpe atma",
        "device.files.utility" => "Dosya işleri",
        "document.read" => "Belge okuma",
        "document.edit" => "Belge yazma",
        "artifact.save" => "Dosya kaydetme",
        "device.app.list" => "Açık uygulamalar",
        "device.app.open" | "device.app.open_by_name" => "Uygulama açma",
        "device.app.control" => "Uygulamayı öne getirme",
        "device.app.quit" => "Uygulama kapatma",
        "device.url.open" => "Web adresi açma",
        "device.system.power" => "Kilitleme ve uyku",
        "device.system.utility" => "Sistem işleri",
        "device.audio" => "Ses seviyesi",
        "device.media" => "Medya kontrolü",
        "device.display" => "Ekran parlaklığı",
        "device.clipboard" => "Pano",
        "device.screen.capture" => "Ekran görüntüsü",
        "device.code.execute" => "Kod ve terminal",
        "device.shortcut.run" => "Kestirmeler",
        "device.messaging.send" => "Mesaj gönderme",
        "mail.send" => "E-posta gönderme",
        "notes.write" => "Not yazma",
        "calendar.read" => "Takvimi okuma",
        "calendar.write" => "Takvime ekleme",
        "reminders.read" => "Hatırlatıcıları okuma",
        "reminders.write" => "Hatırlatıcı ekleme",
        other => other,
    }
}

/// A short line about what a command touches, for the progress panel and the approval card.
pub fn describe(payload: &Value) -> String {
    let value = &payload["value"];
    let field = |name: &str| {
        value[name]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    let mut parts: Vec<String> = [
        "path",
        "filename",
        "name",
        "target",
        "recipient",
        "to",
        "title",
        "query",
        "url",
        "action",
    ]
    .iter()
    .filter_map(|name| field(name))
    .take(2)
    .collect();
    if let Some(destination) = field("destination") {
        parts.push(format!("→ {destination}"));
    }
    let text = if parts.is_empty() {
        value.as_str().unwrap_or("").to_string()
    } else {
        parts.join(" ")
    };
    text.chars().take(120).collect()
}

/// Who asked: a paired phone (its signature was checked) or the server's PC agent (allowed only while the owner has full access on).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Phone {
        pairing_id: String,
    },
    Agent,
    /// The person typed the request in this computer's own window (the chat): like a phone's task, it authorizes the step.
    Local,
}

pub struct Engine {
    platform: Platform,
    settings: Arc<Settings>,
    ledger: Arc<Ledger>,
    services: Arc<Services>,
    targets: Option<fn() -> Vec<Value>>,
    terminal: Option<Arc<dyn crate::terminal::Terminal>>,
    approver: Option<Arc<dyn Approver>>,
    blockers: Option<Arc<dyn Fn() -> HashMap<String, String> + Send + Sync>>,
    operations: HashMap<&'static str, Arc<dyn Operation>>,
    readers: Arc<Semaphore>,
    writer: Arc<Semaphore>,
    running: Mutex<HashMap<Uuid, broadcast::Sender<CommandResult>>>,
}

impl Engine {
    pub fn new(
        platform: Platform,
        settings: Arc<Settings>,
        ledger: Arc<Ledger>,
        services: Arc<Services>,
    ) -> Engine {
        Engine {
            platform,
            settings,
            ledger,
            services,
            targets: None,
            terminal: None,
            approver: None,
            blockers: None,
            operations: HashMap::new(),
            readers: Arc::new(Semaphore::new(READ_LANES)),
            writer: Arc::new(Semaphore::new(1)),
            running: Mutex::new(HashMap::new()),
        }
    }

    /// Adds an operation. Only operations the manifest lists for this platform are accepted: the desktop never offers what the contract does not know.
    pub fn register(&mut self, operation: Arc<dyn Operation>) -> Result<(), String> {
        let name = operation.name();
        if !Manifest::get().supports(name, self.platform) {
            return Err(format!(
                "{name} is not an operation of {:?} in operations.json",
                self.platform
            ));
        }
        self.operations.insert(name, operation);
        Ok(())
    }

    /// Terminal sessions (`terminalCommand`), when this build has them.
    pub fn set_terminal(&mut self, terminal: Arc<dyn crate::terminal::Terminal>) {
        self.terminal = Some(terminal);
    }

    pub fn terminal(&self) -> Option<&Arc<dyn crate::terminal::Terminal>> {
        self.terminal.as_ref()
    }

    /// Capabilities a missing system permission blocks right now, with the sentence that fixes it: they register as `unavailable`, so the
    /// server never plans a step that cannot run and tells the person what to allow instead.
    pub fn set_blockers(
        &mut self,
        blockers: Arc<dyn Fn() -> HashMap<String, String> + Send + Sync>,
    ) {
        self.blockers = Some(blockers);
    }

    pub fn set_approver(&mut self, approver: Arc<dyn Approver>) {
        self.approver = Some(approver);
    }

    /// Asks the person at this computer, whatever the setting (a chat step that changes or deletes); `false` when nobody can be asked.
    pub async fn ask_on_pc(&self, kind: &str, detail: &str) -> bool {
        match &self.approver {
            Some(approver) => approver.approve(kind, detail).await,
            None => false,
        }
    }

    /// Whether the person allows this step of the server's agent here: always when "PC'de onayla" is off or nobody can be asked.
    pub async fn approved_on_pc(&self, kind: &str, detail: &str) -> bool {
        match (&self.approver, self.settings.approve_on_pc()) {
            (Some(approver), true) => approver.approve(kind, detail).await,
            _ => true,
        }
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    pub fn operation_names(&self) -> Vec<&'static str> {
        self.operations.keys().copied().collect()
    }

    pub fn services(&self) -> &Arc<Services> {
        &self.services
    }

    /// Application targets the registration carries (installed and running apps), supplied by the operations crate.
    pub fn set_application_targets(&mut self, provider: fn() -> Vec<Value>) {
        self.targets = Some(provider);
    }

    pub fn application_targets(&self) -> Vec<Value> {
        self.targets.map(|provider| provider()).unwrap_or_default()
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The capabilities this desktop really implements: the only list it reports to the server.
    pub fn capabilities(&self) -> BTreeSet<String> {
        let manifest = Manifest::get();
        self.operations
            .keys()
            .filter_map(|name| manifest.operation(name))
            .map(|operation| operation.capability.clone())
            .collect()
    }

    /// For each capability: whether the owner allows it, and whether a phone has to confirm it.
    pub fn authorities(&self) -> Vec<Value> {
        let manifest = Manifest::get();
        let blocked = self
            .blockers
            .as_ref()
            .map(|blockers| blockers())
            .unwrap_or_default();
        self.capabilities()
            .into_iter()
            .map(|capability| {
                if let Some(message) = blocked.get(&capability) {
                    return json!({ "capability": capability, "displayName": capability_name(&capability), "access": "unavailable", "requiredPermissions": ["system"], "message": message });
                }
                let needs_presence = manifest.operations().iter().any(|operation| operation.capability == capability && self.operations.contains_key(operation.operation.as_str()) && operation.requires_user_presence);
                let access = if !self.settings.capability_enabled(&capability) { "denied" } else if needs_presence { "confirmation" } else { "automatic" };
                json!({ "capability": capability, "displayName": capability_name(&capability), "access": access, "requiredPermissions": [], "message": "" })
            })
            .collect()
    }

    pub async fn execute(&self, request: &Request, origin: &Origin) -> CommandResult {
        let started = Utc::now();
        let id = request.id;
        let denied = |code: &str, message: &str| {
            CommandResult::failed(
                id,
                CommandStatus::Denied,
                WireError::new(code, message, "permission"),
                started,
            )
        };

        let manifest = Manifest::get();
        let Some(entry) = manifest
            .operation(&request.kind)
            .filter(|entry| entry.supported_platforms.contains(&self.platform))
        else {
            return CommandResult::failed(
                id,
                CommandStatus::Failed,
                WireError::new(
                    codes::UNSUPPORTED_OPERATION,
                    "Bu işlem bu bilgisayarda desteklenmiyor.",
                    "validation",
                ),
                started,
            );
        };
        let Some(operation) = self.operations.get(request.kind.as_str()).cloned() else {
            return CommandResult::failed(
                id,
                CommandStatus::Failed,
                WireError::new(
                    codes::UNSUPPORTED_OPERATION,
                    "Bu işlem bu Wiqo sürümünde henüz yok.",
                    "validation",
                ),
                started,
            );
        };
        // The server's agent runs here with "Tam yetki" on, or inside the phone's task grant.
        let phone_free_read = self.settings.reads_without_phone()
            && crate::grants::read_without_phone(&request.kind, &request.payload);
        if *origin == Origin::Agent && !self.settings.full_access() && !phone_free_read {
            if self.services.access.active().is_none() {
                return denied(codes::FULL_ACCESS_DISABLED, "Bu bilgisayarda tam yetki kapalı ve geçerli bir görev izni yok; telefondan onaylanmadan ajan komut çalıştıramaz.");
            }
            if let Some(reason) = self.services.access.denial(&request.kind, &request.payload) {
                return denied(codes::AGENT_SCOPE, &reason);
            }
        }
        if !self.settings.capability_enabled(&entry.capability) {
            return denied(
                codes::CAPABILITY_DISABLED,
                "Bu yetenek bu bilgisayardaki Wiqo ayarlarında kapalı.",
            );
        }

        let digest = request_digest(request);
        if let Some(done) = self.ledger.lookup(id) {
            return if done.digest == digest {
                done.result
            } else {
                CommandResult::failed(
                    id,
                    CommandStatus::Failed,
                    WireError::new(
                        codes::INVALID_PAYLOAD,
                        "Bu komut kimliği başka bir işlem için kullanıldı.",
                        "validation",
                    ),
                    started,
                )
            };
        }
        if *origin == Origin::Agent
            && entry.is_destructive
            && !self
                .approved_on_pc(&request.kind, &describe(&request.payload))
                .await
        {
            return denied(
                codes::DENIED_ON_PC,
                "Bu adım PC'de onaylanmadı; işlem yapılmadı.",
            );
        }
        // The same command arriving while it still runs waits for that run's answer instead of starting a second one.
        let (sender, joined) = {
            let mut running = self.running.lock().expect("running lock");
            match running.get(&id) {
                Some(sender) => (sender.clone(), true),
                None => {
                    let (sender, _) = broadcast::channel(1);
                    running.insert(id, sender.clone());
                    (sender, false)
                }
            }
        };
        if joined {
            let mut receiver = sender.subscribe();
            return receiver.recv().await.unwrap_or_else(|_| {
                CommandResult::failed(
                    id,
                    CommandStatus::Unknown,
                    WireError::new(
                        codes::OUTCOME_UNKNOWN,
                        "Komutun sonucu alınamadı.",
                        "timeout",
                    ),
                    started,
                )
            });
        }

        let result = self
            .run_in_lane(operation, request, entry.timeout_class.budget(), started)
            .await;
        self.ledger.record(id, digest, result.clone());
        self.running.lock().expect("running lock").remove(&id);
        let _ = sender.send(result.clone());
        result
    }

    async fn run_in_lane(
        &self,
        operation: Arc<dyn Operation>,
        request: &Request,
        budget: Duration,
        started: chrono::DateTime<Utc>,
    ) -> CommandResult {
        let id = request.id;
        let lane = match operation.lane() {
            Lane::ReadOnly => self.readers.clone(),
            Lane::SideEffect => self.writer.clone(),
        };
        let payload = request.payload.clone();
        let context = Context {
            command_id: id,
            services: self.services.clone(),
        };
        // The permit travels with the task: if the operation outlives its budget the lane stays held until it ends (or is abandoned after the grace period),
        // so a stuck operation can never be overlapped by the next one that changes something.
        let mut task = tokio::spawn(async move {
            let _permit = lane.acquire_owned().await.ok()?;
            Some(operation.run(&context, &payload).await)
        });
        match tokio::time::timeout(budget, &mut task).await {
            Ok(Ok(Some(outcome))) => match outcome.error {
                None => CommandResult::completed(id, outcome.message, outcome.data, started),
                Some(error) => CommandResult {
                    data: outcome.data,
                    ..CommandResult::failed(id, CommandStatus::Failed, error, started)
                },
            },
            Ok(Ok(None)) => CommandResult::failed(
                id,
                CommandStatus::Failed,
                WireError::new(
                    codes::OUTCOME_UNKNOWN,
                    "Komut çalıştırılamadı.",
                    "tool_failure",
                ),
                started,
            ),
            Ok(Err(_)) => CommandResult::failed(
                id,
                CommandStatus::Unknown,
                WireError::new(
                    codes::OUTCOME_UNKNOWN,
                    "İşlem beklenmedik şekilde durdu; sonucu bilinmiyor.",
                    "tool_failure",
                ),
                started,
            ),
            Err(_) => {
                tokio::spawn(async move {
                    if tokio::time::timeout(GRACE, &mut task).await.is_err() {
                        task.abort();
                    }
                });
                CommandResult::failed(
                    id,
                    CommandStatus::Failed,
                    WireError::new(
                        codes::COMMAND_TIMEOUT,
                        "İşlem zamanında bitmedi; sonucu bilinmiyor.",
                        "timeout",
                    ),
                    started,
                )
            }
        }
    }
}

/// What was asked, without how it was delivered: the id, the type and the payload. Two requests with the same id and digest are the same command.
pub fn request_digest(request: &Request) -> String {
    let asked = json!({ "id": request.id.to_string().to_lowercase(), "type": request.kind, "payload": request.payload });
    hex::encode(Sha256::digest(
        canonical_json(&asked).unwrap_or_default().as_bytes(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::DataDir;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting {
        runs: Arc<AtomicUsize>,
        delay: Duration,
    }

    #[async_trait]
    impl Operation for Counting {
        fn name(&self) -> &'static str {
            "ping"
        }

        fn lane(&self) -> Lane {
            Lane::ReadOnly
        }

        async fn run(&self, _ctx: &Context, _payload: &Value) -> Outcome {
            self.runs.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            Outcome::ok("Pong.", json!({"kind": "ping", "value": {}}))
        }
    }

    fn test_services() -> (DataDir, Arc<Services>) {
        let dir =
            DataDir::new(std::env::temp_dir().join(format!("wiqo-engine-{}", Uuid::new_v4())))
                .unwrap();
        let services = Arc::new(Services {
            data_dir: dir.clone(),
            credentials: Arc::new(CredentialStore::open(&dir)),
            access: Arc::new(Access::default()),
        });
        (dir, services)
    }

    fn engine(delay: Duration) -> (Engine, Arc<AtomicUsize>) {
        let (dir, services) = test_services();
        let mut engine = Engine::new(
            Platform::MacOs,
            Arc::new(Settings::open(&dir)),
            Arc::new(Ledger::in_memory()),
            services,
        );
        let runs = Arc::new(AtomicUsize::new(0));
        engine
            .register(Arc::new(Counting {
                runs: runs.clone(),
                delay,
            }))
            .unwrap();
        (engine, runs)
    }

    fn request(kind: &str) -> Request {
        Request {
            id: Uuid::new_v4(),
            kind: kind.into(),
            payload: json!({"kind": "none"}),
            created_at: crate::wire::timestamp(Utc::now()),
            signature_version: Some(2),
            created_at_epoch_ms: None,
            signature: None,
            nonce: None,
        }
    }

    fn phone() -> Origin {
        Origin::Phone {
            pairing_id: "p".into(),
        }
    }

    #[tokio::test]
    async fn a_command_runs_once_and_a_repeat_gets_the_stored_answer() {
        let (engine, runs) = engine(Duration::ZERO);
        let ping = request("ping");
        let first = engine.execute(&ping, &phone()).await;
        let second = engine.execute(&ping, &phone()).await;
        assert_eq!(first.status, CommandStatus::Completed);
        assert_eq!(first, second);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        let mut other = ping.clone();
        other.payload = json!({"kind": "url", "value": "x"});
        let refused = engine.execute(&other, &phone()).await;
        assert_eq!(refused.error.unwrap().code, codes::INVALID_PAYLOAD);
    }

    #[tokio::test]
    async fn the_same_command_delivered_twice_at_once_runs_once() {
        let (engine, runs) = engine(Duration::from_millis(100));
        let engine = Arc::new(engine);
        let ping = request("ping");
        let origin = phone();
        let (a, b) = tokio::join!(
            engine.execute(&ping, &origin),
            engine.execute(&ping, &origin)
        );
        assert_eq!(a.status, CommandStatus::Completed);
        assert_eq!(a, b);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_unimplemented_and_unallowed_commands_are_refused_with_contract_codes() {
        let (engine, _) = engine(Duration::ZERO);
        assert_eq!(
            engine
                .execute(&request("noSuchThing"), &phone())
                .await
                .error
                .unwrap()
                .code,
            codes::UNSUPPORTED_OPERATION
        );
        assert_eq!(
            engine
                .execute(&request("getSystemStatus"), &phone())
                .await
                .error
                .unwrap()
                .code,
            codes::UNSUPPORTED_OPERATION
        );
        // Reads without the phone are on by default (ping is one); with them off, the agent needs full access.
        assert_eq!(
            engine
                .execute(&request("ping"), &Origin::Agent)
                .await
                .status,
            CommandStatus::Completed
        );
        engine
            .settings()
            .update(|data| data.reads_without_phone = false)
            .unwrap();
        let agent = engine.execute(&request("ping"), &Origin::Agent).await;
        assert_eq!(
            (agent.status, agent.error.unwrap().code),
            (
                CommandStatus::Denied,
                codes::FULL_ACCESS_DISABLED.to_string()
            )
        );
        engine
            .settings()
            .update(|data| {
                data.full_access = true;
            })
            .unwrap();
        assert_eq!(
            engine
                .execute(&request("ping"), &Origin::Agent)
                .await
                .status,
            CommandStatus::Completed
        );
        engine
            .settings()
            .update(|data| {
                data.disabled_capabilities.insert("runtime.status".into());
            })
            .unwrap();
        assert_eq!(
            engine
                .execute(&request("ping"), &phone())
                .await
                .error
                .unwrap()
                .code,
            codes::CAPABILITY_DISABLED
        );
    }

    #[test]
    fn only_operations_of_the_contract_can_be_registered_and_only_they_are_reported() {
        let (engine, _) = engine(Duration::ZERO);
        assert!(engine
            .capabilities()
            .contains(&Manifest::get().operation("ping").unwrap().capability));
        struct Unknown;
        #[async_trait]
        impl Operation for Unknown {
            fn name(&self) -> &'static str {
                "madeUp"
            }
            async fn run(&self, _ctx: &Context, _payload: &Value) -> Outcome {
                Outcome::ok("", json!(null))
            }
        }
        let (dir, services) = test_services();
        let mut other = Engine::new(
            Platform::MacOs,
            Arc::new(Settings::open(&dir)),
            Arc::new(Ledger::in_memory()),
            services,
        );
        assert!(other.register(Arc::new(Unknown)).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_command_over_its_budget_answers_timeout_and_keeps_its_lane_until_it_ends() {
        let (engine, _) = engine(Duration::from_secs(200));
        let result = engine.execute(&request("ping"), &phone()).await;
        assert_eq!(result.error.unwrap().code, codes::COMMAND_TIMEOUT);
    }
}
