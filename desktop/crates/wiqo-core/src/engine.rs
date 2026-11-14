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

/// How far a long command has come (a big copy, a zip): what the panel and the phone's step line show while it runs.
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    started: std::time::Instant,
}

impl Progress {
    pub fn percent(&self) -> u64 {
        (self.done.min(self.total) * 100)
            .checked_div(self.total)
            .unwrap_or(0)
    }

    /// The time left at the pace so far; nothing until there is a pace to go by.
    pub fn remaining(&self) -> Option<Duration> {
        let elapsed = self.started.elapsed();
        if self.done == 0 || self.done >= self.total || elapsed < Duration::from_secs(1) {
            return None;
        }
        Some(elapsed.mul_f64((self.total - self.done) as f64 / self.done as f64))
    }

    /// "%45 · ~20 sn kaldı", for the end of a step's line.
    pub fn words(&self) -> String {
        match self.remaining().map(|left| left.as_secs().max(1)) {
            Some(seconds) if seconds >= 90 => {
                format!("%{} · ~{} dk kaldı", self.percent(), seconds.div_ceil(60))
            }
            Some(seconds) => format!("%{} · ~{seconds} sn kaldı", self.percent()),
            None => format!("%{}", self.percent()),
        }
    }
}

fn progress_board() -> &'static Mutex<HashMap<Uuid, Progress>> {
    static BOARD: std::sync::OnceLock<Mutex<HashMap<Uuid, Progress>>> = std::sync::OnceLock::new();
    BOARD.get_or_init(Default::default)
}

impl Context {
    /// Reports how much of the command's work is done (bytes, files: any unit, `total` the whole of it).
    pub fn progress(&self, done: u64, total: u64) {
        let mut board = progress_board().lock().expect("progress lock");
        let started = board
            .get(&self.command_id)
            .map_or_else(std::time::Instant::now, |known| known.started);
        board.insert(
            self.command_id,
            Progress {
                done,
                total,
                started,
            },
        );
    }
}

/// The progress a running command reported last, if it reports any.
pub fn progress_of(command_id: Uuid) -> Option<Progress> {
    progress_board()
        .lock()
        .expect("progress lock")
        .get(&command_id)
        .copied()
}

/// The desktop's own state that operations need: where it keeps files, and the tokens for talking to the server (saving an artifact downloads it from there).
pub struct Services {
    pub data_dir: DataDir,
    pub credentials: Arc<CredentialStore>,
    /// Rung when something the server reads from the registration changed: the desktop registers again.
    pub announce: Arc<tokio::sync::Notify>,
    /// Where a long job says it started and ended (the agent forwards these to the server); nothing is sent while no sink is attached.
    pub job_events: Arc<JobEvents>,
}

/// A small sink for job events from threads that are not async: the agent attaches a channel, the terminal emits into it.
#[derive(Default)]
pub struct JobEvents(std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Value>>>);

impl JobEvents {
    pub fn attach(&self, sender: tokio::sync::mpsc::UnboundedSender<Value>) {
        *self.0.lock().expect("job events lock") = Some(sender);
    }

    pub fn emit(&self, event: Value) {
        if let Some(sender) = self.0.lock().expect("job events lock").as_ref() {
            let _ = sender.send(event);
        }
    }
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
            announce: Default::default(),
            job_events: Default::default(),
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

pub struct Engine {
    platform: Platform,
    settings: Arc<Settings>,
    ledger: Arc<Ledger>,
    services: Arc<Services>,
    targets: Option<fn() -> Vec<Value>>,
    terminal: Option<Arc<dyn crate::terminal::Terminal>>,
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
                let access = if needs_presence { "confirmation" } else { "automatic" };
                json!({ "capability": capability, "displayName": capability_name(&capability), "access": access, "requiredPermissions": [], "message": "" })
            })
            .collect()
    }

    pub async fn execute(&self, request: &Request) -> CommandResult {
        let started = Utc::now();
        let id = request.id;
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
        progress_board().lock().expect("progress lock").remove(&id);
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
        let reads_only = operation.lane() == Lane::ReadOnly;
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
                // What only looks cannot have changed anything: its time ran out, and it may be tried once more (narrower). What may change something
                // is unknown after its time runs out, and is never repeated by itself.
                let error = if reads_only {
                    WireError::new(
                        codes::READ_TIMEOUT,
                        "Okuma zamanında bitmedi; hiçbir şey değişmedi. Daha dar bir klasör ya da daha kısa bir aramayla bir kez daha dene.",
                        "tool_failure",
                    )
                } else {
                    WireError::new(
                        codes::COMMAND_TIMEOUT,
                        "İşlem zamanında bitmedi; sonucu bilinmiyor.",
                        "timeout",
                    )
                };
                CommandResult::failed(id, CommandStatus::Failed, error, started)
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
    #[test]
    fn progress_says_how_far_and_how_long() {
        let ctx = Context::for_tests();
        assert!(progress_of(ctx.command_id).is_none());
        ctx.progress(0, 200);
        assert_eq!(progress_of(ctx.command_id).unwrap().words(), "%0");
        let mut known = progress_of(ctx.command_id).unwrap();
        known.started -= Duration::from_secs(10);
        known.done = 50;
        assert_eq!(known.percent(), 25);
        assert_eq!(known.words(), "%25 · ~30 sn kaldı");
        known.started -= Duration::from_secs(50);
        assert_eq!(known.words(), "%25 · ~3 dk kaldı");
        assert_eq!(
            Progress {
                done: 5,
                total: 0,
                started: std::time::Instant::now()
            }
            .percent(),
            0
        );
    }

    use super::*;
    use crate::storage::DataDir;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting {
        runs: Arc<AtomicUsize>,
        delay: Duration,
        lane: Lane,
    }

    #[async_trait]
    impl Operation for Counting {
        fn name(&self) -> &'static str {
            "ping"
        }

        fn lane(&self) -> Lane {
            self.lane
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
            announce: Default::default(),
            job_events: Default::default(),
        });
        (dir, services)
    }

    fn engine(delay: Duration) -> (Engine, Arc<AtomicUsize>) {
        engine_in(delay, Lane::ReadOnly)
    }

    fn engine_in(delay: Duration, lane: Lane) -> (Engine, Arc<AtomicUsize>) {
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
                lane,
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

    #[tokio::test]
    async fn a_command_runs_once_and_a_repeat_gets_the_stored_answer() {
        let (engine, runs) = engine(Duration::ZERO);
        let ping = request("ping");
        let first = engine.execute(&ping).await;
        let second = engine.execute(&ping).await;
        assert_eq!(first.status, CommandStatus::Completed);
        assert_eq!(first, second);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        let mut other = ping.clone();
        other.payload = json!({"kind": "url", "value": "x"});
        let refused = engine.execute(&other).await;
        assert_eq!(refused.error.unwrap().code, codes::INVALID_PAYLOAD);
    }

    #[tokio::test]
    async fn the_same_command_delivered_twice_at_once_runs_once() {
        let (engine, runs) = engine(Duration::from_millis(100));
        let engine = Arc::new(engine);
        let ping = request("ping");
        let (a, b) = tokio::join!(engine.execute(&ping), engine.execute(&ping));
        assert_eq!(a.status, CommandStatus::Completed);
        assert_eq!(a, b);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_unimplemented_and_unallowed_commands_are_refused_with_contract_codes() {
        let (engine, _) = engine(Duration::ZERO);
        assert_eq!(
            engine
                .execute(&request("noSuchThing"))
                .await
                .error
                .unwrap()
                .code,
            codes::UNSUPPORTED_OPERATION
        );
        assert_eq!(
            engine
                .execute(&request("getSystemStatus"))
                .await
                .error
                .unwrap()
                .code,
            codes::UNSUPPORTED_OPERATION
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
        let (engine, _) = engine_in(Duration::from_secs(200), Lane::SideEffect);
        let result = engine.execute(&request("ping")).await;
        assert_eq!(result.error.unwrap().code, codes::COMMAND_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_over_its_budget_changed_nothing_and_says_so() {
        let (engine, _) = engine(Duration::from_secs(200));
        let result = engine.execute(&request("ping")).await;
        let error = result.error.unwrap();
        assert_eq!(error.code, codes::READ_TIMEOUT);
        assert_ne!(error.category.as_deref(), Some("timeout"));
    }
}
