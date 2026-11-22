//! The chat: the person signs in with their own Wiqo account and talks to `/copilot/chat` with this computer as the device. This computer holds the
//! stream and runs every PC step the answer asks for, whether the message was typed in this window or handed over by a paired phone (then the phone
//! is the remote screen: every event goes to it as well, and its steps run as that phone's). Results go back to the server until the task is done.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use uuid::Uuid;
use wiqo_core::engine::Engine;
use wiqo_core::storage::DataDir;
use wiqo_core::vault::{Account, Vault};
use wiqo_core::wire::{CommandStatus, Request};

use crate::Event;

mod digest;
mod ledger;
mod local;
mod progress;
mod repo;
mod vault;

const CLIENT_FEATURES: [&str; 6] = [
    "artifact-directory-v1",
    // PC work runs on this computer's own agent (chat/local.rs); the server only answers its model calls.
    "local-agent-v1",
    // This build takes a handoff in any round of a task (the model reached for the terminal after other PC steps), not only as the first answer.
    "local-agent-v2",
    "relay-execution-v1",
    "receipt-card-v1",
    // A look at a window that says little carries its picture, which the server reads for what the controls do not name.
    "screen-sight-v1",
];
/// The most a step's data may take on its way to the server (which cuts anything longer).
const DATA_TEXT_MOST: usize = 11_500;
/// A task goes on as long as it moves: there is no fixed number of rounds. It stops after this long, or after `STALL_ROUNDS` rounds in a row in which
/// no step completed (the server's own stall rules usually end such a task earlier, honestly).
const TASK_MINUTES: u64 = 120;
const STALL_ROUNDS: usize = 3;
/// The server writes a ping into an open answer every 20 seconds. An answer that has said nothing for this long (a network that changed, a laptop that
/// woke up) is dead even if the connection does not say so: the result is then read from the server's record of the request instead of waited for.
const SILENCE: Duration = Duration::from_secs(75);
/// How long the results of steps this computer already ran are held for a server that cannot be reached, before the turn gives up on telling them.
const RESULTS_KEPT_FOR: Duration = Duration::from_secs(10 * 60);
/// How long a step that was asked to stop has to end by itself before the turn leaves it.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// The server could not be reached at all: nothing was refused, so the same request may be sent again.
#[derive(Debug)]
struct Unreachable;

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sunucuya ulaşılamadı; internet bağlantını kontrol et.")
    }
}

impl std::error::Error for Unreachable {}

/// One turn's "Durdur". It belongs to that turn alone: another turn running at the same moment (a phone's message, a background task's step) neither
/// sees it nor clears it.
#[derive(Default)]
pub struct Stop {
    asked: AtomicBool,
    told: tokio::sync::Notify,
}

impl Stop {
    pub fn is_asked(&self) -> bool {
        self.asked.load(Ordering::SeqCst)
    }

    fn ask(&self) {
        self.asked.store(true, Ordering::SeqCst);
        self.told.notify_waiters();
    }

    /// Returns once the person asked this turn to stop (at once when they already did).
    async fn asked(&self) {
        loop {
            let told = self.told.notified();
            tokio::pin!(told);
            // Registered before the flag is read, so an ask between the two is not missed.
            told.as_mut().enable();
            if self.is_asked() {
                return;
            }
            told.await;
        }
    }
}

/// What the person answered to a question before a step: yes, no, or nothing in time (they were away; the step did not run and the task says why).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Yes,
    No,
    TimedOut,
}

/// Asks the person before a step that changes, deletes or sends (the operation and the sentence they read).
pub type Approve =
    dyn Fn(String, String) -> futures_util::future::BoxFuture<'static, Decision> + Send + Sync;

/// Whether a step of the answer waits for the person's yes: one that changes, deletes or sends something (`isDestructive`, cleared by the server
/// only for what the person allowed once and for all) or that the server marked. A read never asks (`requiresUserPresence` marks reads of private
/// files too); a press on the screen that cannot be taken back is asked by the computer itself (DESKTOP_APPROVAL_REQUIRED).
pub(crate) fn needs_yes(step: &Value) -> bool {
    // Full access (the person's own switch on this computer): the work goes on without a yes for each step. Sends and anything that removes still ask;
    // a press on the screen that cannot be taken back is asked by the computer itself either way.
    if wiqo_ops::full_access() && !asks_to_send(step) && !removes_something(step) {
        return false;
    }
    step["approvalRequired"] == true || step["isDestructive"] == true || asks_to_send(step)
}

/// The words the person approves are the server's own summary, so the arguments that will really run are put under it: where, to whom, which command.
fn with_facts(step: &Value, question: String) -> String {
    let payload = payload_of(step);
    let value = payload.get("value").unwrap_or(&payload);
    let mut facts = Vec::new();
    for key in ["path", "from", "to", "source", "destination", "target", "recipient", "recipients", "email", "subject", "workingDirectory", "command", "url", "app", "name"] {
        let shown = match &value[key] {
            Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
            Value::Array(items) if !items.is_empty() => Some(items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")),
            _ => None,
        };
        if let Some(text) = shown.filter(|text| !text.is_empty()) {
            facts.push(format!("{key}: {}", text.chars().take(200).collect::<String>()));
        }
    }
    if facts.is_empty() { question } else { format!("{question}\n\n{}", facts.join("\n")) }
}

/// The step's payload as an object, whether the server wrote it as JSON text or as an object.
fn payload_of(step: &Value) -> Value {
    match &step["payload"] {
        Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
        other => other.clone(),
    }
}

/// The step as it runs again after the person's yes to a press that cannot be taken back. In a list of steps the ones that already ran are left
/// out: the yes is for the press that asked, not for doing the earlier steps a second time.
fn rest_after_yes(step: &Value, refused: &Value) -> Value {
    let mut approved = step.clone();
    let mut payload = payload_of(step);
    let ran = refused["dataText"]
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|data| data["value"]["evidence"].as_array().cloned())
        .map(|evidence| evidence.iter().take_while(|entry| entry["ok"] == true).count())
        .unwrap_or(0);
    if let Some(value) = payload["value"].as_object_mut() {
        value.insert("cardApproved".into(), json!(true));
        for list in ["steps", "fields"] {
            if let Some(items) = value.get_mut(list).and_then(Value::as_array_mut) {
                if ran < items.len() {
                    items.drain(..ran);
                }
            }
        }
    }
    approved["payload"] = payload;
    approved
}

/// What the screen showed, made to fit the way to the server: the picture travels beside the data, and a window with more controls than fit loses the
/// last of its list (counted in `moreTargets`) instead of arriving as a cut-off piece of text nobody can read.
fn fit_desktop(data: &Value) -> (Value, Option<String>) {
    let mut shown = data.clone();
    let picture = shown["value"]
        .as_object_mut()
        .and_then(|value| value.remove("picture"))
        .and_then(|picture| picture["dataBase64"].as_str().map(str::to_string))
        .filter(|image| image.len() >= 32 && image.len() <= 1_200_000);
    for list in ["targets", "addedTargets"] {
        while shown.to_string().len() > DATA_TEXT_MOST {
            let Some(items) = shown["value"][list].as_array_mut().filter(|items| !items.is_empty()) else {
                break;
            };
            let keep = items.len() - (items.len() / 8).max(1);
            let cut = items.len() - keep;
            items.truncate(keep);
            let more = shown["value"]["moreTargets"].as_u64().unwrap_or(0) + cut as u64;
            shown["value"]["moreTargets"] = json!(more);
        }
    }
    (shown, picture)
}

/// A browser step that asks to send from the person's signed-in accounts (`sendApproved`): that claim comes from the server, so this computer asks
/// for its own yes (and an unattended automation refuses it) instead of believing it.
fn removes_something(step: &Value) -> bool {
    let kind = step["type"].as_str().unwrap_or("").to_lowercase();
    ["delete", "trash", "remove", "uninstall", "empty"].iter().any(|word| kind.contains(word))
}

fn asks_to_send(step: &Value) -> bool {
    let payload = payload_of(step);
    payload["sendApproved"] == true || payload["value"]["sendApproved"] == true
}

/// What only this computer may say about the person: `person` (a phone's own signature, see wiqo-core relay) and `cardApproved` (this computer's
/// own yes to a press that cannot be taken back) are dropped from whatever the server planned; `sendApproved` stays only after this computer's yes.
fn without_server_authority(mut payload: Value, card_approved: bool, send_approved: bool) -> Value {
    let clean = |object: &mut serde_json::Map<String, Value>| {
        object.remove("person");
        if !card_approved {
            object.remove("cardApproved");
        }
        if !send_approved {
            object.remove("sendApproved");
        }
    };
    if let Some(object) = payload.as_object_mut() {
        clean(object);
    }
    if let Some(object) = payload.get_mut("value").and_then(Value::as_object_mut) {
        clean(object);
    }
    payload
}

/// Who a turn is for.
pub enum Asker {
    /// The person typed it in this window. `mission`: the message came from a Mission in Keşfet (someone else's words); the server reads the whole
    /// task as an outsider's instruction.
    Window { mission: Option<String> },
    /// A paired phone handed its message here (frames.json `remoteMessage`): `request` is the `/copilot/chat` body the phone would have sent itself. It
    /// goes out as it is, written from the phone with the phone's features and language; only what describes the computer is this computer's. Every
    /// event of the answer also goes to `forward`.
    Phone {
        request: Value,
        forward: Box<dyn Fn(Value) + Send + Sync>,
    },
}

impl Asker {
    /// The request's `clientFeatures` and `locale`, and where the person writes from.
    fn client(&self, context: &Value) -> Value {
        match self {
            Asker::Window { .. } => {
                let mut client = context.clone();
                merge(
                    &mut client,
                    &json!({ "clientFeatures": CLIENT_FEATURES, "locale": "tr" }),
                );
                client
            }
            Asker::Phone { request, .. } => {
                let mut client = context.clone();
                if let Some(object) = client.as_object_mut() {
                    object.remove("sourceDeviceId");
                }
                // The phone's own features, plus the one that says who runs the steps: this computer, which holds the stream.
                let mut features: Vec<Value> = request["clientFeatures"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                for own in ["relay-execution-v1", "local-agent-v1", "local-agent-v2"] {
                    if !features.iter().any(|feature| feature == own) {
                        features.push(json!(own));
                    }
                }
                merge(
                    &mut client,
                    &json!({ "surface": "phone", "clientFeatures": features, "locale": request["locale"].as_str().unwrap_or("tr") }),
                );
                client
            }
        }
    }

    /// What the phone's own request says beyond the message (the files read on the phone, the agent it writes to, a card it edits…). What describes the
    /// computer, and the results of PC steps, are never taken from it.
    fn own_fields(&self) -> Value {
        match self {
            Asker::Window { mission: Some(code) } => json!({ "missionCode": code }),
            Asker::Window { mission: None } => json!({}),
            Asker::Phone { request, .. } => {
                let mut fields = request.clone();
                if let Some(object) = fields.as_object_mut() {
                    for key in [
                        "deviceId",
                        "sourceDeviceId",
                        "surface",
                        "availableApps",
                        "availableCapabilities",
                        "applicationTargets",
                        "authorities",
                        "deviceResults",
                    ] {
                        object.remove(key);
                    }
                }
                fields
            }
        }
    }
}

pub struct Chat {
    server: String,
    vault: Arc<Vault>,
    file: std::path::PathBuf,
    account: Mutex<Option<Account>>,
    http: reqwest::Client,
    /// The running turns' stops, under each name a turn is known by: its first request's id and its task's id.
    stops: Mutex<HashMap<String, Arc<Stop>>>,
    account_changed: tokio::sync::Notify,
    verifier: Mutex<Option<String>>,
}

fn rand_bytes() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    for chunk in bytes.chunks_mut(16) {
        chunk.copy_from_slice(&Uuid::new_v4().into_bytes());
    }
    bytes
}

impl Chat {
    pub fn new(server: &str, vault: Arc<Vault>, dir: &DataDir) -> Chat {
        let file = dir.file("account.json");
        let account = if vault.enabled() {
            vault.load().account
        } else {
            std::fs::read(&file)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        };
        Chat {
            server: server.trim_end_matches('/').to_string(),
            vault,
            file,
            account: Mutex::new(account),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .user_agent(concat!("wiqo-desktop/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_default(),
            stops: Mutex::new(HashMap::new()),
            account_changed: tokio::sync::Notify::new(),
            verifier: Mutex::new(None),
        }
    }

    pub fn email(&self) -> Option<String> {
        self.account
            .lock()
            .expect("account lock")
            .as_ref()
            .map(|account| account.email.clone())
    }

    /// The project-task evals (`wiqo-agent eval`): one task through the real local agent loop, against the gateway the runner names. The account is
    /// held in memory only, approvals are the runner's (`approve`), and every event goes to `emit` (the last one is `done` with the reply).
    pub async fn eval_task(&self, engine: &Engine, device_id: &str, handoff: &Value, token: &str, approve: &Approve, emit: &(dyn Fn(Value) + Sync)) -> Result<()> {
        *self.account.lock().expect("account lock") = Some(Account { email: "eval@wiqo.invalid".into(), token: token.to_string() });
        let goal = handoff["goal"].as_str().unwrap_or("").to_string();
        let stop = Stop::default();
        let conversation = Uuid::new_v4().to_string();
        self.local_agent(engine, device_id, handoff, &conversation, &json!([]), &goal, token, approve, &stop, &|event: Value| emit(event)).await
    }

    fn token(&self) -> Result<String> {
        self.account
            .lock()
            .expect("account lock")
            .as_ref()
            .map(|account| account.token.clone())
            .ok_or_else(|| anyhow!("Önce Wiqo hesabınla giriş yap."))
    }

    fn store(&self, account: Option<Account>, expected_token: Option<&str>) -> Result<()> {
        let mut current = self.account.lock().expect("account lock");
        if expected_token.is_some()
            && current.as_ref().map(|item| item.token.as_str()) != expected_token
        {
            return Ok(());
        }
        if self.vault.enabled() {
            self.vault
                .update(|secrets| secrets.account = account.clone())?;
        } else {
            match &account {
                Some(account) => std::fs::write(&self.file, serde_json::to_vec(account)?)?,
                None => {
                    let _ = std::fs::remove_file(&self.file);
                }
            }
        }
        *current = account;
        self.account_changed.notify_waiters();
        Ok(())
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> Result<Value> {
        let mut request = self
            .http
            .request(method, format!("{}/{path}", self.server))
            .timeout(Duration::from_secs(30));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| anyhow!("Sunucuya ulaşılamadı; internet bağlantını kontrol et."))?;
        let status = response.status();
        let value: Value = response.json().await.unwrap_or(Value::Null);
        if status.as_u16() == 401 && token.is_some() {
            let _ = self.store(None, token);
            bail!("Oturumun sona erdi; yeniden giriş yap.");
        }
        if !status.is_success() {
            let message = value["error"]["message"]
                .as_str()
                .or(value["error"].as_str())
                .or(value["message"].as_str())
                .unwrap_or("İstek reddedildi.");
            bail!("{message}");
        }
        Ok(value)
    }

    pub async fn sign_in(&self, email: &str, password: &str) -> Result<String> {
        let value = self
            .call(
                reqwest::Method::POST,
                "auth/login",
                Some(json!({ "email": email.trim(), "password": password })),
                None,
            )
            .await?;
        let token = value["token"]
            .as_str()
            .ok_or_else(|| anyhow!("Giriş yapılamadı."))?
            .to_string();
        let email = value["email"].as_str().unwrap_or(email.trim()).to_string();
        self.store(
            Some(Account {
                email: email.clone(),
                token,
            }),
            None,
        )?;
        Ok(email)
    }

    pub async fn register(&self, email: &str, password: &str, invite: &str) -> Result<String> {
        let mut body = json!({ "email": email.trim(), "password": password });
        if !invite.trim().is_empty() {
            body["inviteCode"] = json!(invite.trim());
        }
        let value = self
            .call(reqwest::Method::POST, "auth/register", Some(body), None)
            .await?;
        self.keep(&value, email)
    }

    fn keep(&self, value: &Value, email: &str) -> Result<String> {
        let token = value["token"]
            .as_str()
            .ok_or_else(|| anyhow!("Giriş yapılamadı."))?
            .to_string();
        let email = value["email"].as_str().unwrap_or(email.trim()).to_string();
        self.store(
            Some(Account {
                email: email.clone(),
                token,
            }),
            None,
        )?;
        Ok(email)
    }

    /// Google, the way desktop apps do it (RFC 8252): Wiqo listens on a one-time loopback address, the browser goes to Google and comes back
    /// to that address with a one-time code, and the code is traded (with the PKCE verifier kept here) for a session. The `wiqo://` scheme
    /// is the fallback for a server that does not accept a loopback address yet. `cancel` stops the wait.
    pub async fn google_sign_in(
        &self,
        open: impl Fn(&str),
        cancel: &tokio::sync::Notify,
    ) -> Result<String> {
        use base64::Engine as _;
        use sha2::Digest as _;
        let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand_bytes());
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(verifier.as_bytes()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| anyhow!("Google girişi başlatılamadı."))?;
        let return_to = format!(
            "http://127.0.0.1:{}/callback",
            listener.local_addr()?.port()
        );
        let start = self
            .call(
                reqwest::Method::POST,
                "auth/google/start",
                Some(json!({ "codeChallenge": challenge, "returnTo": return_to })),
                None,
            )
            .await;
        let (value, loopback) = match start {
            Ok(value) => (value, true),
            // An older server refuses the loopback address: the wiqo:// link brings the code back instead (google_finish).
            Err(_) => (
                self.call(
                    reqwest::Method::POST,
                    "auth/google/start",
                    Some(json!({ "codeChallenge": challenge })),
                    None,
                )
                .await?,
                false,
            ),
        };
        let url = value["url"]
            .as_str()
            .filter(|url| url.starts_with("https://accounts.google.com/"))
            .ok_or_else(|| anyhow!("Google girişi başlatılamadı."))?;
        *self.verifier.lock().expect("verifier lock") = Some(verifier);
        open(url);
        if !loopback {
            bail!("WAITING_FOR_LINK");
        }
        let query = tokio::select! {
            query = receive_callback(&listener) => query?,
            _ = tokio::time::sleep(Duration::from_secs(300)) => bail!("Google girişi zaman aşımına uğradı; yeniden dene."),
            _ = cancel.notified() => bail!("Google girişi iptal edildi."),
        };
        self.finish_google(&query).await
    }

    /// The `wiqo://oauth-callback?...` link (fallback path).
    pub async fn google_finish(&self, callback: &str) -> Result<String> {
        let url = url::Url::parse(callback).map_err(|_| anyhow!("Google girişi tamamlanamadı."))?;
        let query: std::collections::HashMap<String, String> = url
            .query_pairs()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        self.finish_google(&query).await
    }

    async fn finish_google(
        &self,
        query: &std::collections::HashMap<String, String>,
    ) -> Result<String> {
        if query.get("status").map(String::as_str) != Some("google-login") {
            bail!(
                "{}",
                query
                    .get("message")
                    .cloned()
                    .unwrap_or_else(|| "Google ile giriş tamamlanamadı.".into())
            );
        }
        let code = query
            .get("code")
            .ok_or_else(|| anyhow!("Google ile giriş tamamlanamadı."))?;
        let verifier = self
            .verifier
            .lock()
            .expect("verifier lock")
            .take()
            .ok_or_else(|| anyhow!("Google girişi zaman aşımına uğradı; yeniden dene."))?;
        let value = self
            .call(
                reqwest::Method::POST,
                "auth/google/exchange",
                Some(json!({ "code": code, "codeVerifier": verifier })),
                None,
            )
            .await?;
        let token = value["token"]
            .as_str()
            .ok_or_else(|| anyhow!("Google ile giriş tamamlanamadı."))?
            .to_string();
        // The exchange answers with a token only: the address comes from the session itself.
        let me = self
            .call(reqwest::Method::GET, "auth/me", None, Some(&token))
            .await
            .unwrap_or(Value::Null);
        let email = me["email"].as_str().unwrap_or("").to_string();
        self.store(
            Some(Account {
                email: email.clone(),
                token,
            }),
            None,
        )?;
        Ok(email)
    }

    /// "Şifremi unuttum": the server mails a six-digit code (it answers the same whether the address exists or not).
    pub async fn forgot_password(&self, email: &str) -> Result<String> {
        let value = self
            .call(
                reqwest::Method::POST,
                "auth/forgot-password",
                Some(json!({ "email": email.trim() })),
                None,
            )
            .await?;
        Ok(value["message"]
            .as_str()
            .unwrap_or("E-posta kayıtlıysa sıfırlama kodu gönderildi.")
            .to_string())
    }

    /// The code from the mail and a new password; then signs in with it.
    pub async fn reset_password(&self, email: &str, code: &str, password: &str) -> Result<String> {
        self.call(
            reqwest::Method::POST,
            "auth/reset-password",
            Some(json!({ "email": email.trim(), "code": code.trim(), "newPassword": password })),
            None,
        )
        .await?;
        self.sign_in(email, password).await
    }

    /// The account's name and picture for the window (`/profile`).
    pub async fn profile(&self) -> Result<Value> {
        let token = self.token()?;
        let value = self
            .call(reqwest::Method::GET, "profile", None, Some(&token))
            .await?;
        Ok(json!({
            "email": value["email"], "displayName": value["displayName"], "avatarUrl": value["avatarUrl"], "avatarColor": value["avatarColor"],
            "authProvider": value["authProvider"],
        }))
    }

    /// The apps that can be connected, the connections and what each can do, in one answer for the "Uygulamalar" page.
    pub async fn connectors(&self) -> Result<Value> {
        let token = self.token()?;
        let providers = self
            .call(reqwest::Method::GET, "oauth/providers", None, Some(&token))
            .await?;
        let connections = self
            .call(reqwest::Method::GET, "oauth", None, Some(&token))
            .await?;
        let capabilities = self
            .call(
                reqwest::Method::GET,
                "oauth/capabilities",
                None,
                Some(&token),
            )
            .await?;
        // The MCP apps the person added, with each tool's state (allowed, altered by the app since it was connected); an older server has no such list.
        let apps = self
            .call(reqwest::Method::GET, "apps", None, Some(&token))
            .await
            .unwrap_or_default();
        let servers: Vec<&Value> = apps["apps"]
            .as_array()
            .map(|list| list.iter().filter(|app| app["kind"] == "mcp").collect())
            .unwrap_or_default();
        Ok(
            json!({ "providers": providers["providers"], "connections": connections["connections"], "capabilities": capabilities["capabilities"], "servers": servers }),
        )
    }

    /// The person has looked at the tools an MCP app altered: they are kept as they are now and stop asking before every use.
    pub async fn mcp_accept_changes(&self, server: u64) -> Result<()> {
        let token = self.token()?;
        self.call(
            reqwest::Method::POST,
            &format!("mcp/servers/{server}/tools/accept"),
            None,
            Some(&token),
        )
        .await?;
        Ok(())
    }

    /// Which tools of an MCP app Wiqo may use; an empty list allows all of them.
    pub async fn mcp_allow_tools(&self, server: u64, tools: &[String]) -> Result<()> {
        let token = self.token()?;
        self.call(
            reqwest::Method::PUT,
            &format!("mcp/servers/{server}/allow"),
            Some(json!({ "allowTools": tools })),
            Some(&token),
        )
        .await?;
        Ok(())
    }

    pub async fn mcp_remove(&self, server: u64) -> Result<()> {
        let token = self.token()?;
        self.call(
            reqwest::Method::DELETE,
            &format!("mcp/servers/{server}"),
            None,
            Some(&token),
        )
        .await?;
        Ok(())
    }

    /// The provider's consent page; the caller opens it in the browser and the answer returns as a `wiqo://oauth-callback` link.
    pub async fn connector_start(&self, provider: &str) -> Result<String> {
        let token = self.token()?;
        let value = self
            .call(
                reqwest::Method::GET,
                &format!("oauth/{provider}/start"),
                None,
                Some(&token),
            )
            .await?;
        value["url"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("Bağlantı adresi alınamadı."))
    }

    pub async fn connector_disconnect(&self, provider: &str) -> Result<()> {
        let token = self.token()?;
        self.call(
            reqwest::Method::DELETE,
            &format!("oauth/{provider}"),
            None,
            Some(&token),
        )
        .await?;
        Ok(())
    }

    pub async fn connector_access(&self, provider: &str, level: &str) -> Result<()> {
        let token = self.token()?;
        self.call(
            reqwest::Method::PUT,
            &format!("oauth/{provider}/access"),
            Some(json!({ "accessLevel": level })),
            Some(&token),
        )
        .await?;
        Ok(())
    }

    pub fn token_for_device(&self) -> Option<String> {
        self.token().ok()
    }

    pub fn sign_out(&self) -> Result<()> {
        self.store(None, None)
    }

    pub async fn conversations(&self) -> Result<Value> {
        let token = self.token()?;
        self.call(reqwest::Method::GET, "conversations", None, Some(&token))
            .await
    }

    /// The agent chat's list: the bots (one thread each) and the groups.
    pub async fn threads(&self) -> Result<Value> {
        let token = self.token()?;
        self.call(reqwest::Method::GET, "agents/threads", None, Some(&token))
            .await
    }

    pub async fn thread_open(&self, thread: &str) -> Result<Value> {
        check_id(thread)?;
        let token = self.token()?;
        self.call(
            reqwest::Method::GET,
            &format!("agents/threads/{thread}"),
            None,
            Some(&token),
        )
        .await
    }

    /// A new group (`{ title?, agentIds: [2-4] }`).
    pub async fn thread_create(&self, body: Value) -> Result<Value> {
        let token = self.token()?;
        self.call(
            reqwest::Method::POST,
            "agents/threads",
            Some(body),
            Some(&token),
        )
        .await
    }

    /// Renames a group and/or changes its members.
    pub async fn thread_update(&self, thread: &str, body: Value) -> Result<Value> {
        check_id(thread)?;
        let token = self.token()?;
        self.call(
            reqwest::Method::PATCH,
            &format!("agents/threads/{thread}"),
            Some(body),
            Some(&token),
        )
        .await
    }

    pub async fn thread_delete(&self, thread: &str) -> Result<Value> {
        check_id(thread)?;
        let token = self.token()?;
        self.call(
            reqwest::Method::DELETE,
            &format!("agents/threads/{thread}"),
            None,
            Some(&token),
        )
        .await
    }

    /// Stops the answer being written in a thread ("Durdur").
    pub async fn thread_stop(&self, thread: &str) -> Result<Value> {
        check_id(thread)?;
        let token = self.token()?;
        self.call(
            reqwest::Method::POST,
            &format!("agents/threads/{thread}/stop"),
            Some(json!({})),
            Some(&token),
        )
        .await
    }

    /// Stops the group's running job.
    pub async fn work_cancel(&self, task: u64) -> Result<Value> {
        let token = self.token()?;
        self.call(
            reqwest::Method::POST,
            &format!("agent-tasks/{task}/cancel"),
            Some(json!({})),
            Some(&token),
        )
        .await
    }

    /// Clears what an agent remembers about the person.
    pub async fn agent_forget(&self, id: u64) -> Result<Value> {
        let token = self.token()?;
        self.call(
            reqwest::Method::POST,
            &format!("agents/{id}/forget"),
            Some(json!({})),
            Some(&token),
        )
        .await
    }

    /// Keşfet (rova-backend social/routes.ts): one call under `missions/`. A refusal comes back as `{ "status", "error" }` and not as an error, so
    /// the window can show what blocked a Mission (its findings) next to the field each one is in.
    pub async fn missions(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let path = path.trim_start_matches('/');
        // `profile` is the one profile (name, handle, headline, colour, cover…) Settings edits, and its free cover library; Keşfet reads the same row through `missions/`.
        let own_profile = (path == "profile" && matches!(method, "GET" | "PUT")) || (path.starts_with("profile/cover-library") && method == "GET");
        if !(own_profile || path == "missions" || path.starts_with("missions/") || path.starts_with("missions?")) || path.contains("..") {
            bail!("Bu Keşfet isteği tanınmıyor.");
        }
        let method = match method {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "PATCH" => reqwest::Method::PATCH,
            "DELETE" => reqwest::Method::DELETE,
            _ => bail!("Bu Keşfet isteği tanınmıyor."),
        };
        let token = self.token()?;
        // A draft and a publish each wait on a model call on the server.
        let mut request = self.http.request(method, format!("{}/{path}", self.server)).timeout(Duration::from_secs(90)).bearer_auth(&token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|_| anyhow!("Sunucuya ulaşılamadı; internet bağlantını kontrol et."))?;
        let status = response.status();
        let value: Value = response.json().await.unwrap_or(Value::Null);
        if status.as_u16() == 401 {
            let _ = self.store(None, Some(&token));
            bail!("Oturumun sona erdi; yeniden giriş yap.");
        }
        if status.is_success() {
            return Ok(value);
        }
        let error = if value["error"].is_object() { value["error"].clone() } else { json!({ "message": "İstek reddedildi." }) };
        Ok(json!({ "status": status.as_u16(), "error": error }))
    }

    /// The person's recipes ("Tariflerim", rova-backend agent/skills.ts): their own, recorded and imported, most used first.
    pub async fn skills(&self) -> Result<Value> {
        let token = self.token()?;
        self.call(reqwest::Method::GET, "skills", None, Some(&token)).await
    }

    /// A recording made on this computer becomes a recipe: the server's model writes the request that repeats it. `events` are what the person
    /// read and approved in the panel; `note` is their own sentence about the task, if they wrote one.
    pub async fn skill_from_recording(&self, events: Value, note: Option<&str>) -> Result<Value> {
        let token = self.token()?;
        self.call(reqwest::Method::POST, "skills/from-recording", Some(json!({ "events": events, "note": note })), Some(&token)).await
    }

    /// `action`: "used" (the recipe went into the composer) or "delete". Sharing goes through Keşfet (`missions`).
    pub async fn skill_action(&self, id: u64, action: &str) -> Result<Value> {
        let token = self.token()?;
        let (method, path) = match action {
            "used" => (reqwest::Method::POST, format!("skills/{id}/used")),
            "delete" => (reqwest::Method::DELETE, format!("skills/{id}")),
            _ => bail!("Bu tarif işlemi tanınmıyor."),
        };
        self.call(method, &path, None, Some(&token)).await
    }

    /// Recipe packs ("Paketler"): the list without an id, or that pack's recipes added to this person's recipes.
    pub async fn skill_packs(&self, import: Option<&str>) -> Result<Value> {
        let token = self.token()?;
        match import {
            None => self.call(reqwest::Method::GET, "skills/packs", None, Some(&token)).await,
            Some(id) => {
                let id: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
                self.call(reqwest::Method::POST, &format!("skills/packs/{id}/import"), Some(json!({})), Some(&token)).await
            }
        }
    }

    /// Every agent of this person (the four ready-made ones included), for the agent form.
    pub async fn agent_list(&self) -> Result<Value> {
        let token = self.token()?;
        self.call(reqwest::Method::GET, "agents", None, Some(&token))
            .await
    }

    /// Makes (no id) or edits (id) an agent: `{ name, role, personality, persona }`.
    pub async fn agent_save(&self, id: Option<u64>, body: Value) -> Result<Value> {
        let token = self.token()?;
        match id {
            Some(id) => {
                self.call(
                    reqwest::Method::PATCH,
                    &format!("agents/{id}"),
                    Some(body),
                    Some(&token),
                )
                .await
            }
            None => {
                self.call(reqwest::Method::POST, "agents", Some(body), Some(&token))
                    .await
            }
        }
    }

    pub async fn agent_remove(&self, id: u64) -> Result<Value> {
        let token = self.token()?;
        self.call(
            reqwest::Method::DELETE,
            &format!("agents/{id}"),
            None,
            Some(&token),
        )
        .await
    }

    /// One message to an agent thread; each event of the answer goes to `emit` (typing, text pieces, finished messages). The server finishes the turn even when
    /// this connection drops, so a break only ends the wait: the window reads the thread again.
    pub async fn thread_stream(
        &self,
        thread: &str,
        request_id: &str,
        text: &str,
        emit: &impl Fn(Value),
    ) -> Result<()> {
        check_id(thread)?;
        let token = self.token()?;
        let response = self
            .http
            .post(format!("{}/agents/threads/{thread}/messages", self.server))
            .bearer_auth(&token)
            .header("Accept", "text/event-stream")
            .timeout(Duration::from_secs(5 * 60))
            .json(&json!({ "message": text, "requestId": request_id }))
            .send()
            .await
            .map_err(|_| anyhow!("Sunucuya ulaşılamadı; internet bağlantını kontrol et."))?;
        if response.status().as_u16() == 401 {
            let _ = self.store(None, Some(&token));
            bail!("Oturumun sona erdi; yeniden giriş yap.");
        }
        if !response.status().is_success() {
            let value: Value = response.json().await.unwrap_or(Value::Null);
            bail!(
                "{}",
                value["error"]["message"]
                    .as_str()
                    .unwrap_or("Mesaj gönderilemedi.")
            );
        }
        let mut bytes = response.bytes_stream();
        let mut buffer = String::new();
        while let Some(chunk) = bytes.next().await {
            let Ok(chunk) = chunk else { break };
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(end) = buffer.find('\n') {
                let line: String = buffer.drain(..=end).collect();
                let Some(event) = parse_event_line(&line) else {
                    continue;
                };
                let last = matches!(event["type"].as_str(), Some("thread_done") | Some("error"));
                emit(event);
                if last {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Everything Wiqo does for this person (reminders, routines, watches, jobs, to-dos, goals, helpers) in the one shape the phones draw too.
    pub async fn agents(&self) -> Result<Value> {
        let token = self.token()?;
        self.call(reqwest::Method::GET, "agents/overview", None, Some(&token))
            .await
    }

    /// One action ("pause", "resume", "run", "stop", "delete", "complete") on one item by its ref ("automation:12").
    pub async fn agent_action(&self, reference: &str, action: &str) -> Result<Value> {
        let valid = reference
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == ':')
            && action.chars().all(|c| c.is_ascii_lowercase());
        if !valid {
            bail!("Bu işlem yapılamıyor.");
        }
        let token = self.token()?;
        self.call(
            reqwest::Method::POST,
            &format!("agents/items/{reference}/{action}"),
            Some(serde_json::json!({})),
            Some(&token),
        )
        .await
    }

    pub async fn messages(&self, conversation: &str) -> Result<Value> {
        let token = self.token()?;
        self.call(
            reqwest::Method::GET,
            &format!("conversations/{conversation}/messages"),
            None,
            Some(&token),
        )
        .await
    }

    /// Answers a card the server asked to confirm (a mail, a calendar change: the server's own tools).
    pub async fn confirm(&self, confirmation: &Value, approved: bool) -> Result<Value> {
        let token = self.token()?;
        let id = confirmation["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Onay kartı okunamadı."))?;
        let body = json!({ "confirmationId": id, "approved": approved });
        self.call(
            reqwest::Method::POST,
            "copilot/confirm",
            Some(body),
            Some(&token),
        )
        .await
    }

    /// Continues a reply that waits for a place (the weather): the city, or nothing to let the server answer without one.
    pub async fn continue_location(&self, request_id: &str, city: Option<&str>) -> Result<Value> {
        let token = self.token()?;
        if request_id.is_empty()
            || request_id.len() > 128
            || !request_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("İşlem kimliği geçersiz.");
        }
        let city = city.map(str::trim).filter(|city| !city.is_empty());
        self.call(
            reqwest::Method::POST,
            &format!("copilot/requests/{request_id}/location"),
            Some(json!({ "city": city })),
            Some(&token),
        )
        .await
    }

    /// A file the server made for this account (`/copilot/artifacts/:id`): its type and bytes. Only this server's artifact addresses are fetched.
    pub async fn artifact(&self, id: &str) -> Result<(String, Vec<u8>)> {
        let token = self.token()?;
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("Dosya kimliği geçersiz.");
        }
        let response = self
            .http
            .get(format!("{}/copilot/artifacts/{id}", self.server))
            .bearer_auth(token)
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .map_err(|_| anyhow!("Dosya indirilemedi."))?;
        if !response.status().is_success() {
            bail!("Dosya indirilemedi ({}).", response.status().as_u16());
        }
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = response
            .bytes()
            .await
            .map_err(|_| anyhow!("Dosya indirilemedi."))?;
        if bytes.len() > 100 * 1024 * 1024 {
            bail!("Dosya çok büyük.");
        }
        Ok((mime, bytes.to_vec()))
    }

    /// Stops the turn known by this name (its request's id or its task's id): the step it runs is left, no further step starts. False when no such turn
    /// runs here.
    pub fn stop_turn(&self, name: &str) -> bool {
        let stop = self
            .stops
            .lock()
            .expect("stops lock")
            .get(&name.to_ascii_lowercase())
            .cloned();
        stop.inspect(|stop| stop.ask()).is_some()
    }

    /// "Durdur": that task's turn stops (the step it runs is left, no further step starts), and the server is told to stop the task.
    pub async fn cancel(&self, task: &str, conversation: &str) {
        self.stop_turn(task);
        if let Ok(token) = self.token() {
            let _ = self
                .call(
                    reqwest::Method::POST,
                    &format!("copilot/tasks/{task}/cancel"),
                    Some(json!({ "conversationId": conversation })),
                    Some(&token),
                )
                .await;
        }
    }

    /// One message: the answer streams to the window as `chat` events (`turn` names this message) and, for a phone's message, to the phone; PC
    /// steps run here and their results go back until the server's answer has no more steps. `request_id` is the first request's id (a phone's own id,
    /// so the server runs that request once whichever way it arrives).
    #[allow(clippy::too_many_arguments)]
    pub async fn send(
        &self,
        engine: &Engine,
        device_id: &str,
        events: &broadcast::Sender<Event>,
        turn: &str,
        request_id: String,
        conversation: &str,
        history: Value,
        text: &str,
        task: Option<String>,
        question: Option<String>,
        attachments: Value,
        asker: &Asker,
        approve: &Approve,
    ) -> Result<()> {
        let token = self.token()?;
        // An answer to a question the running task asked keeps that task's id and the question's id; a phone names its own task.
        let task = task.unwrap_or_else(|| Uuid::new_v4().to_string());
        let stop = Arc::new(Stop::default());
        // Ids are compared without their case: a phone writes the same id in capitals in one place and in small letters in another.
        let names = [request_id.to_ascii_lowercase(), task.to_ascii_lowercase()];
        {
            let mut stops = self.stops.lock().expect("stops lock");
            for name in &names {
                stops.insert(name.clone(), stop.clone());
            }
        }
        let outcome = self
            .turn(
                engine,
                device_id,
                events,
                turn,
                request_id,
                conversation,
                history,
                text,
                task,
                question,
                attachments,
                asker,
                approve,
                &token,
                &stop,
            )
            .await;
        // The names are given back only if they are still this turn's: a later turn of the same task has its own stop under the task's id.
        let mut stops = self.stops.lock().expect("stops lock");
        for name in &names {
            if stops
                .get(name)
                .is_some_and(|current| Arc::ptr_eq(current, &stop))
            {
                stops.remove(name);
            }
        }
        outcome
    }

    #[allow(clippy::too_many_arguments)]
    async fn turn(
        &self,
        engine: &Engine,
        device_id: &str,
        events: &broadcast::Sender<Event>,
        turn: &str,
        request_id: String,
        conversation: &str,
        history: Value,
        text: &str,
        task: String,
        question: Option<String>,
        attachments: Value,
        asker: &Asker,
        approve: &Approve,
        token: &str,
        stop: &Stop,
    ) -> Result<()> {
        let emit = |event: Value| {
            if let Asker::Phone { forward, .. } = asker {
                forward(event.clone());
            }
            let _ = events.send(Event::Chat {
                turn: turn.to_string(),
                event,
            });
        };
        let context = asker.client(&device_context(engine, device_id));
        let mut messages: Vec<Value> = history.as_array().cloned().unwrap_or_default();
        messages.push(json!({ "role": "user", "content": text }));
        let mut body = json!({
            "messages": messages, "conversationId": conversation, "requestId": request_id, "taskId": task,
        });
        if let Some(question) = &question {
            body["questionId"] = json!(question);
        }
        merge(&mut body, &asker.own_fields());
        // Pictures the person added: {name, mimeType, dataBase64}, at most four.
        if let Some(items) = attachments.as_array().filter(|items| !items.is_empty()) {
            body["attachments"] = Value::Array(items.iter().take(4).cloned().collect());
        }
        merge(&mut body, &context);
        emit(json!({ "type": "task", "taskId": task }));
        let mut answer = self.stream(token, &body, &emit).await?;
        let started = std::time::Instant::now();
        let mut stalls = 0usize;
        while started.elapsed() < Duration::from_secs(TASK_MINUTES * 60) {
            if answer["type"] == "error" {
                return Ok(());
            }
            // PC work handed to this computer's own agent (at the first answer, or in a later round, when the model reached for the terminal): the loop
            // runs here from now on.
            if answer["localAgent"].is_object() {
                return self.local_agent(engine, device_id, &answer["localAgent"], conversation, &history, text, token, approve, stop, &emit).await;
            }
            // The server's turn ran out of time with the task not done: a new window of the same task, from its notes (rova-backend continueTask).
            if answer["continueTask"] == true {
                let mut next = json!({
                    "messages": [{ "role": "user", "content": text }],
                    "conversationId": conversation, "requestId": Uuid::new_v4().to_string(), "taskId": answer["taskId"].as_str().unwrap_or(&task),
                    "resumeTask": true,
                });
                merge(&mut next, &context);
                emit(json!({ "type": "reset" }));
                answer = self.stream(token, &next, &emit).await?;
                continue;
            }
            let steps = answer["steps"].as_array().cloned().unwrap_or_default();
            if steps.is_empty() {
                return Ok(());
            }
            if answer["deviceConnectivity"]["deviceId"]
                .as_str()
                .is_some_and(|target| !target.eq_ignore_ascii_case(device_id))
            {
                bail!("Bu görevin hedefi başka bir PC; adımlar bu bilgisayarda çalıştırılmadı.");
            }
            let mut results = Vec::new();
            let mut uncertain = false;
            for step in &steps {
                if stop.is_asked() {
                    results.push(json!({ "stepId": step["id"], "type": step["type"], "status": "denied", "message": "Kullanıcı görevi durdurdu; bu adım çalıştırılmadı.", "errorCode": "PERMISSION_DENIED" }));
                    break;
                }
                emit(progress::planned(step, "running", None, None));
                let summary = step["summary"].as_str().unwrap_or("").to_string();
                let kind = step["type"].as_str().unwrap_or("").to_string();
                // "Önce prova": a batch says what it would do before the yes is asked, so the question lists the files and where each goes.
                let question = if needs_yes(step) { with_facts(step, with_preview(engine, step, &summary)) } else { summary.clone() };
                let asked = needs_yes(step);
                let answer = if asked { approve(kind.clone(), question).await } else { Decision::Yes };
                let mut result = match answer {
                    Decision::Yes => run_with_progress(engine, step, &emit, asked, stop).await,
                    Decision::No => refused_step(step, "Onay verilmedi; bu adım çalıştırılmadı."),
                    Decision::TimedOut => unanswered_step(step),
                };
                // The computer itself asks before a press that cannot be taken back: the person's yes is asked with its own sentence, and the same
                // step runs once more, under a new id, carrying that yes.
                if result["errorCode"] == "DESKTOP_APPROVAL_REQUIRED" {
                    let sentence = result["error"].as_str().unwrap_or("").to_string();
                    let answer = approve(kind, sentence).await;
                    result = if answer == Decision::Yes {
                        let mut approved = rest_after_yes(step, &result);
                        approved["id"] = json!(Uuid::new_v4().to_string());
                        let mut again = run_step_as(engine, &approved, true, false).await;
                        again["stepId"] = step["id"].clone();
                        again
                    } else if answer == Decision::TimedOut {
                        unanswered_step(step)
                    } else {
                        refused_step(step, "Onay verilmedi; bu adım çalıştırılmadı.")
                    };
                }
                emit(progress::planned(step, result["status"].as_str().unwrap_or("failed"), result["message"].as_str(), None));
                uncertain = result["sideEffectUncertain"] == true;
                let ok = result["status"] == "completed";
                results.push(result);
                if !ok {
                    break;
                }
            }
            if uncertain {
                emit(
                    json!({ "type": "status", "text": "PC işleminin sonucu belirsiz; aynı adım yeniden çalıştırılmadı." }),
                );
            }
            let progressed = results.iter().any(|result| result["status"] == "completed");
            let mut next = json!({
                "messages": [
                    { "role": "user", "content": text },
                    { "role": "assistant", "content": answer["reply"].as_str().unwrap_or("") },
                    { "role": "user", "content": "Yerel PC sonuçları hazır. Bu sonuçları kullanarak önceki isteği şimdi tamamla; belge içeriğini yeniden isteme." },
                ],
                "conversationId": conversation, "requestId": Uuid::new_v4().to_string(), "taskId": answer["taskId"].as_str().unwrap_or(&task),
                "deviceResults": results,
            });
            merge(&mut next, &context);
            emit(json!({ "type": "reset" }));
            // These steps ran: their results are told even if the server cannot be reached for a while (the same request, so it is taken once).
            answer = self.stream_kept(token, &next, &emit).await?;
            if uncertain {
                return Ok(());
            }
            stalls = if progressed { 0 } else { stalls + 1 };
            if stalls >= STALL_ROUNDS {
                emit(json!({ "type": "status", "text": "Üç turdur hiçbir adım tamamlanmadı; görevi burada durdurdum. Neyi farklı yapmamı istediğini yazarsan devam ederim." }));
                return Ok(());
            }
        }
        emit(
            json!({ "type": "status", "text": "Görev 30 dakikayı aştı; tamamlanan adımlar kayıtlı. Devam etmemi istersen yaz." }),
        );
        Ok(())
    }

    /// Read-only observer: only the existing relay or originating request executes actions.
    pub async fn watch_runs(&self, events: &broadcast::Sender<Event>) {
        let mut delay = 1;
        loop {
            let changed = self.account_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let account = self.account.lock().expect("account lock").clone();
            let Some(account) = account else {
                changed.await;
                continue;
            };
            let token = account.token;
            let request = self
                .http
                .get(format!("{}/runs/events", self.server))
                .bearer_auth(&token)
                .timeout(Duration::from_secs(15 * 60));
            let result =
                tokio::select! { result = request.send() => result, _ = &mut changed => continue };
            if let Ok(response) = result {
                if response.status().as_u16() == 401
                    && self.token().ok().as_deref() == Some(token.as_str())
                {
                    let _ = self.store(None, Some(&token));
                }
                if response.status().is_success() {
                    let mut bytes = response.bytes_stream();
                    let mut buffer = Vec::new();
                    loop {
                        let next = tokio::select! { chunk = bytes.next() => chunk, _ = &mut changed => break };
                        let Some(chunk) = next else {
                            break;
                        };
                        if self.token().ok().as_deref() != Some(token.as_str()) {
                            break;
                        }
                        let Ok(chunk) = chunk else {
                            break;
                        };
                        buffer.extend_from_slice(&chunk);
                        if buffer.len() > 2_000_000 {
                            break;
                        }
                        while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
                            let line: Vec<u8> = buffer.drain(..=end).collect();
                            if let Some(event) = parse_event_line(&String::from_utf8_lossy(&line)) {
                                delay = 1;
                                let _ = events.send(Event::RunSync {
                                    account: account.email.clone(),
                                    event,
                                });
                            }
                        }
                    }
                }
            }
            tokio::select! { _ = tokio::time::sleep(Duration::from_secs(delay)) => {}, _ = &mut changed => continue }
            delay = (delay * 2).min(30);
        }
    }

    /// Streams one request's events; when the stream breaks before its end, follows the request on the server instead.
    async fn stream(&self, token: &str, body: &Value, emit: &impl Fn(Value)) -> Result<Value> {
        let request_id = body["requestId"].as_str().unwrap_or("").to_string();
        let response = self
            .http
            .post(format!("{}/copilot/chat", self.server))
            .bearer_auth(token)
            .header("Accept", "text/event-stream")
            .timeout(Duration::from_secs(20 * 60))
            .json(body)
            .send()
            .await
            .map_err(|_| anyhow::Error::new(Unreachable))?;
        if response.status().as_u16() == 401 {
            let _ = self.store(None, Some(token));
            bail!("Oturumun sona erdi; yeniden giriş yap.");
        }
        // A server that is restarting or overloaded answers through its gateway: the request was not taken, like one that never arrived.
        if matches!(response.status().as_u16(), 502..=504) {
            return Err(anyhow::Error::new(Unreachable));
        }
        if !response.status().is_success() {
            let value: Value = response.json().await.unwrap_or(Value::Null);
            bail!(
                "{}",
                value["error"]["message"]
                    .as_str()
                    .unwrap_or("Sohbet isteği reddedildi.")
            );
        }
        let mut bytes = response.bytes_stream();
        let mut buffer = String::new();
        // Silence longer than the server's own pings allow is a dead connection: the answer is then followed from the server's record.
        while let Ok(Some(chunk)) = tokio::time::timeout(SILENCE, bytes.next()).await {
            let Ok(chunk) = chunk else { break };
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(end) = buffer.find('\n') {
                let line: String = buffer.drain(..=end).collect();
                let Some(payload) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<Value>(payload.trim()) else {
                    continue;
                };
                let last = matches!(event["type"].as_str(), Some("done") | Some("error"));
                emit(event.clone());
                if last {
                    return Ok(event);
                }
            }
        }
        emit(json!({ "type": "status", "text": "Bağlantı koptu; sonuç sunucudan alınıyor…" }));
        self.follow(token, &request_id, emit).await
    }

    /// `stream` for a request that carries the results of steps this computer already ran. A server that cannot be reached is tried again for
    /// `RESULTS_KEPT_FOR` with the same request (the server takes a request id once, so a repeat neither runs it twice nor loses it).
    async fn stream_kept(&self, token: &str, body: &Value, emit: &impl Fn(Value)) -> Result<Value> {
        let started = std::time::Instant::now();
        let mut pause = Duration::from_secs(2);
        loop {
            match self.stream(token, body, emit).await {
                Err(error) if error.is::<Unreachable>() && started.elapsed() < RESULTS_KEPT_FOR => {
                    emit(json!({ "type": "status", "text": "Sunucuya ulaşılamıyor; PC adımlarının sonucu bağlantı gelince iletilecek…" }));
                    tokio::time::sleep(pause).await;
                    pause = (pause * 2).min(Duration::from_secs(30));
                }
                other => return other,
            }
        }
    }

    async fn follow(&self, token: &str, request_id: &str, emit: &impl Fn(Value)) -> Result<Value> {
        let mut delay = 1;
        for _ in 0..120 {
            tokio::time::sleep(Duration::from_secs(delay)).await;
            delay = (delay * 2).min(8);
            let Ok(state) = self
                .call(
                    reqwest::Method::GET,
                    &format!("copilot/requests/{request_id}?handoff=1"),
                    None,
                    Some(token),
                )
                .await
            else {
                continue;
            };
            match state["status"].as_str() {
                Some("done") => {
                    let mut done = state["response"].clone();
                    done["type"] = json!("done");
                    emit(done.clone());
                    return Ok(done);
                }
                Some("failed") => bail!(
                    "{}",
                    state["error"]["message"]
                        .as_str()
                        .unwrap_or("İşlem tamamlanamadı.")
                ),
                Some("awaiting_backend_confirmation") => emit(
                    json!({ "type": "confirmation_needed", "backendConfirmations": state["response"]["backendConfirmations"] }),
                ),
                _ => emit(json!({ "type": "status", "text": "İşlem sunucuda devam ediyor…" })),
            }
        }
        bail!("İşlem sunucuda devam ediyor olabilir; sohbet geçmişinden kontrol et.")
    }
}

/// Waits for the browser's GET /callback?… on the loopback listener, answers it with a small page, and returns the query.
async fn receive_callback(
    listener: &tokio::net::TcpListener,
) -> Result<std::collections::HashMap<String, String>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    loop {
        let (mut socket, _) = listener.accept().await?;
        let mut buffer = vec![0u8; 8192];
        let read = tokio::time::timeout(Duration::from_secs(10), socket.read(&mut buffer))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(0);
        let request = String::from_utf8_lossy(&buffer[..read]).to_string();
        let target = request.split_whitespace().nth(1).unwrap_or("/").to_string();
        if !target.starts_with("/callback") {
            let _ = socket
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let url = url::Url::parse(&format!("http://127.0.0.1{target}"))?;
        let query: std::collections::HashMap<String, String> = url
            .query_pairs()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let ok = query.get("status").map(String::as_str) == Some("google-login");
        let page = callback_page(ok, query.get("message").map(String::as_str));
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{page}", page.len());
        let _ = socket.write_all(response.as_bytes()).await;
        return Ok(query);
    }
}

/// What the browser tab shows after Google: done (go back to Wiqo) or why not.
fn callback_page(ok: bool, message: Option<&str>) -> String {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let (title, body) = if ok {
        (
            "Giriş yapıldı",
            "Wiqo'ya dönebilirsin; bu sekmeyi kapatabilirsin.".to_string(),
        )
    } else {
        (
            "Giriş tamamlanamadı",
            escape(message.unwrap_or("Wiqo'ya dönüp yeniden dene.")),
        )
    };
    format!(
        "<!doctype html><html lang=\"tr\"><head><meta charset=\"utf-8\"><title>Wiqo</title><style>\
         body{{margin:0;height:100vh;display:grid;place-items:center;font:15px -apple-system,BlinkMacSystemFont,Segoe UI,sans-serif;background:#fff;color:#111}}\
         @media(prefers-color-scheme:dark){{body{{background:#0b0b0c;color:#f5f5f5}}}}\
         .c{{text-align:center;max-width:360px;padding:24px}}.m{{width:44px;height:44px;border-radius:50%;display:grid;place-items:center;margin:0 auto 14px;\
         background:{};color:#fff;font-size:22px}}h1{{font-size:20px;margin:0 0 6px}}p{{margin:0;color:#6b6b6b}}</style></head>\
         <body><div class=\"c\"><div class=\"m\">{}</div><h1>{title}</h1><p>{body}</p></div><script>setTimeout(()=>window.close(),1500)</script></body></html>",
        if ok { "#15803d" } else { "#b91c1c" },
        if ok { "✓" } else { "!" }
    )
}

fn merge(target: &mut Value, extra: &Value) {
    if let (Some(target), Some(extra)) = (target.as_object_mut(), extra.as_object()) {
        target.extend(extra.clone());
    }
}

fn single_line(value: &Value, max: usize) -> Option<String> {
    let text: String = value
        .as_str()?
        .replace(['\r', '\n', '\0'], " ")
        .trim()
        .chars()
        .take(max)
        .collect();
    (!text.is_empty()).then_some(text)
}

/// What this computer offers, in exactly the shape `/copilot/chat` accepts (rova-backend routes/deviceContextSchema.ts: strict objects, so any
/// extra field fails the whole request).
fn device_context(engine: &Engine, device_id: &str) -> Value {
    let targets: Vec<Value> = engine
        .application_targets()
        .into_iter()
        .filter_map(|target| {
            let name = single_line(&target["name"], 160)?;
            let actions: Vec<&str> = target["supportedActions"]
                .as_array()
                .map(|actions| actions.iter().filter_map(Value::as_str).filter(|action| matches!(*action, "open" | "activate" | "quit")).take(3).collect())
                .unwrap_or_default();
            let mut entry = json!({ "name": name, "isRunning": target["isRunning"].as_bool().unwrap_or(false), "supportedActions": actions });
            if let Some(bundle) = single_line(&target["bundleIdentifier"], 200) {
                entry["bundleIdentifier"] = json!(bundle);
            }
            Some(entry)
        })
        .take(200)
        .collect();
    let authorities: Vec<Value> = engine
        .authorities()
        .into_iter()
        .filter_map(|authority| {
            let access = authority["access"].as_str().filter(|access| matches!(*access, "automatic" | "confirmation" | "denied" | "unavailable"))?;
            Some(json!({
                "capability": single_line(&authority["capability"], 120)?,
                "displayName": single_line(&authority["displayName"], 160)?,
                "access": access,
                "requiredPermissions": authority["requiredPermissions"].as_array().map(|items| items.iter().filter_map(|item| single_line(item, 120)).take(12).collect::<Vec<_>>()).unwrap_or_default(),
                "message": authority["message"].as_str().map(|text| text.replace(['\r', '\n'], " ").chars().take(500).collect::<String>()),
            }))
        })
        .take(100)
        .collect();
    let apps: Vec<Value> = targets
        .iter()
        .filter_map(|target| target["name"].as_str())
        .take(50)
        .map(|name| json!(name.chars().take(80).collect::<String>()))
        .collect();
    let capabilities: Vec<String> = engine.capabilities().into_iter().take(300).collect();
    json!({ "surface": "desktop", "sourceDeviceId": device_id, "deviceId": device_id, "availableApps": apps, "availableCapabilities": capabilities, "applicationTargets": targets, "authorities": authorities })
}

/// A step that did not run because the person said no (or did not answer in time).
/// The question before a batch of file changes, with what it would do: the first files and where each goes, and how many more. Any other step,
/// or a batch that cannot say, keeps its own sentence.
pub(crate) fn with_preview(engine: &Engine, step: &Value, summary: &str) -> String {
    const SHOWN: usize = 12;
    if step["type"] != "fileUtility" {
        return summary.to_string();
    }
    let payload = match &step["payload"] {
        Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
        other => other.clone(),
    };
    let context = wiqo_core::engine::Context { command_id: Uuid::new_v4(), services: engine.services().clone() };
    let Some(preview) = wiqo_ops::batch_preview(&context, &payload) else {
        return summary.to_string();
    };
    let moves = preview["moves"].as_array().cloned().unwrap_or_default();
    if moves.is_empty() {
        return summary.to_string();
    }
    let name = |path: &str| path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
    let lines: Vec<String> = moves
        .iter()
        .take(SHOWN)
        .map(|item| {
            let (from, to) = (item["from"].as_str().unwrap_or(""), item["to"].as_str().unwrap_or(""));
            let (from_folder, to_folder) = (from.rsplit_once(['/', '\\']).map(|(dir, _)| dir).unwrap_or(""), to.rsplit_once(['/', '\\']).map(|(dir, _)| dir).unwrap_or(""));
            if from_folder == to_folder {
                format!("• {} → {}", name(from), name(to))
            } else {
                format!("• {} → {}/", name(from), name(to_folder))
            }
        })
        .collect();
    let more = moves.len().saturating_sub(SHOWN);
    let folders = preview["newFolders"].as_array().map(|list| list.len()).unwrap_or(0);
    format!(
        "{summary}\n\nÖnce prova: {} dosya{}\n{}{}\n\nOnaylarsan tek seferde yapılır ve 7 gün içinde tek dokunuşla geri alınabilir.",
        moves.len(),
        if folders > 0 { format!(", {folders} yeni klasör") } else { String::new() },
        lines.join("\n"),
        if more > 0 { format!("\n… ve {more} dosya daha") } else { String::new() }
    )
}

/// The person did not answer in time: nothing ran, and the task says so in words that tell the model not to ask again by itself (the contract: `APPROVAL_TIMEOUT`).
pub(crate) fn unanswered_step(step: &Value) -> Value {
    json!({ "stepId": step["id"], "type": step["type"], "status": "denied", "errorCode": "APPROVAL_TIMEOUT",
        "message": "Onay için süre doldu; kullanıcı yanıt vermedi ve bu adım çalıştırılmadı. Reddedilmiş sayma; kullanıcıya onayı beklerken süre dolduğunu söyle ve istersen aynı isteği yeniden göndermesini öner." })
}

pub(crate) fn refused_step(step: &Value, message: &str) -> Value {
    json!({ "stepId": step["id"], "type": step["type"], "status": "denied", "message": message, "errorCode": "DENIED_ON_PC" })
}

/// Runs one step of the answer here and describes the result the way the server reads `deviceResults`.
/// Runs a step and, while it runs, puts how far it has come at the end of its line ("Kopyala · %45 · ~20 sn kaldı") for the panel and the phone.
/// What a step the person stopped while it ran says: it may have done part of its work, so it is never run again by itself.
fn stopped_step(step: &Value) -> Value {
    json!({ "stepId": step["id"], "type": step["type"], "status": "denied", "message": "Kullanıcı görevi durdurdu; bu adım yarıda bırakıldı.", "errorCode": "PERMISSION_DENIED", "sideEffectUncertain": true })
}

async fn run_with_progress(engine: &Engine, step: &Value, emit: &impl Fn(Value), local_yes: bool, stop: &Stop) -> Value {
    let id = step["id"].as_str().and_then(|id| Uuid::parse_str(id).ok());
    let summary = step["summary"].as_str().unwrap_or("").to_string();
    let run = run_step_as(engine, step, false, local_yes);
    tokio::pin!(run);
    let mut shown = String::new();
    loop {
        tokio::select! {
            result = &mut run => return result,
            // What can be stopped on this computer was told to stop by whoever asked; the step gets a moment to end by itself and say what it did.
            _ = stop.asked() => {
                return match tokio::time::timeout(STOP_GRACE, &mut run).await {
                    Ok(result) => result,
                    Err(_) => stopped_step(step),
                };
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                let Some(progress) = id.and_then(wiqo_core::engine::progress_of) else { continue };
                let words = progress.words();
                if words != shown {
                    emit(progress::planned(step, "running", None, Some(&format!("{summary} · {words}"))));
                    shown = words;
                }
            }
        }
    }
}

pub(crate) async fn run_step(engine: &Engine, step: &Value) -> Value {
    run_step_as(engine, step, false, false).await
}

/// Runs a step the server planned. `card_approved` is set only by this computer's own re-run after the person's yes to an irreversible press,
/// `send_approved` only after this computer's yes to a send from a signed-in account.
pub(crate) async fn run_step_as(engine: &Engine, step: &Value, card_approved: bool, send_approved: bool) -> Value {
    run_step_whole(engine, step, card_approved, send_approved).await.0
}

/// `run_step_as`, with the step's data as the operation gave it. What goes to the server is fitted to a size (`dataText`); this computer's own agent
/// (chat/local.rs) reads the whole thing and keeps the end of a long output, where a build says what went wrong.
pub(crate) async fn run_step_whole(engine: &Engine, step: &Value, card_approved: bool, send_approved: bool) -> (Value, Option<Value>) {
    let kind = step["type"].as_str().unwrap_or("unknown").to_string();
    let refused = |message: &str, code: &str| json!({ "stepId": step["id"], "type": kind, "status": "denied", "message": message, "errorCode": code });
    let Some(id) = step["id"].as_str().and_then(|id| Uuid::parse_str(id).ok()) else {
        return (refused("PC adımı okunamadı; işlem yapılmadı.", "INVALID_STEP"), None);
    };
    let payload = payload_of(step);
    if payload.is_null() {
        return (refused("PC adımı okunamadı; işlem yapılmadı.", "INVALID_STEP"), None);
    }
    let payload = without_server_authority(payload, card_approved, send_approved);
    let summary = step["summary"].as_str().unwrap_or("").to_string();
    let request = Request {
        id,
        kind: kind.clone(),
        payload,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        signature_version: None,
        created_at_epoch_ms: None,
        signature: None,
        nonce: None,
    };
    let result = engine.execute(&request).await;
    let confirmed = result.status == CommandStatus::Completed && result.error.is_none();
    let status = if confirmed {
        "completed".to_string()
    } else if result.status == CommandStatus::Completed {
        "failed".to_string()
    } else {
        serde_json::to_value(result.status)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "failed".into())
    };
    let uncertain = result.status == CommandStatus::Unknown
        || result.error.as_ref().is_some_and(|error| {
            matches!(error.code.as_str(), "COMMAND_TIMEOUT" | "OUTCOME_UNKNOWN")
        });
    let mut fields = json!({
        "stepId": id.to_string(), "type": kind, "status": status, "message": result.message.chars().take(2_000).collect::<String>(),
        "verified": confirmed, "sideEffectUncertain": uncertain,
    });
    if !summary.is_empty() {
        fields["summary"] = json!(summary.chars().take(2_000).collect::<String>());
    }
    if let Some(error) = &result.error {
        fields["error"] = json!(error.message.chars().take(2_000).collect::<String>());
        fields["errorCode"] = json!(error.code);
    }
    if let Some(data) = &result.data {
        if data["kind"] == "screenshot" {
            if let Some(image) = data["value"]["dataBase64"]
                .as_str()
                .filter(|image| image.len() <= 1_200_000)
            {
                fields["imageBase64"] = json!(image);
            }
        }
        if data["kind"] != "fileTransfer" {
            let mut shown = data.clone();
            if shown["kind"] == "screenshot" {
                shown["value"]["dataBase64"] = json!("");
            }
            if shown["kind"] == "computerDesktop" {
                let (fitted, picture) = fit_desktop(&shown);
                shown = fitted;
                if let Some(picture) = picture {
                    fields["imageBase64"] = json!(picture);
                }
            }
            let text = shown.to_string();
            fields["dataText"] = json!(if text.len() <= DATA_TEXT_MOST {
                text
            } else {
                json!({ "kind": "result", "truncated": true, "preview": text.chars().take(5_000).collect::<String>() }).to_string()
            });
        }
    }
    (fields, result.data)
}

/// A thread or agent id from the window: letters, digits and dashes only, so it can never change the path it is put into.
fn check_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("Bu sohbet açılamıyor.");
    }
    Ok(())
}

/// One `data:` line of a server-sent event stream as JSON; comments (`: ping`), blank lines and broken lines are not events.
fn parse_event_line(line: &str) -> Option<Value> {
    let payload = line.trim().strip_prefix("data:")?;
    serde_json::from_str::<Value>(payload.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_lines_are_read_and_the_rest_is_ignored() {
        let event = parse_event_line("data: {\"type\":\"agent_typing\",\"agentId\":3}\n").unwrap();
        assert_eq!(event["type"], "agent_typing");
        assert_eq!(event["agentId"], 3);
        assert!(parse_event_line(": ping\n").is_none());
        assert!(parse_event_line("\n").is_none());
        assert!(parse_event_line("data: {broken\n").is_none());
        assert!(parse_event_line("event: message\n").is_none());
    }

    #[test]
    fn an_old_requests_unauthorized_reply_cannot_sign_out_the_new_account() {
        let dir =
            DataDir::new(std::env::temp_dir().join(format!("wiqo-auth-race-{}", Uuid::new_v4())))
                .unwrap();
        let chat = Chat::new("http://127.0.0.1:1", Arc::new(Vault::new(false)), &dir);
        chat.store(
            Some(Account {
                email: "new@test.invalid".into(),
                token: "new-test-token".into(),
            }),
            None,
        )
        .unwrap();
        chat.store(None, Some("old-test-token")).unwrap();
        assert_eq!(chat.email().as_deref(), Some("new@test.invalid"));
        chat.store(None, Some("new-test-token")).unwrap();
        assert!(chat.email().is_none());
    }

    fn chat_at(server: &str) -> Chat {
        let dir = DataDir::new(std::env::temp_dir().join(format!("wiqo-chat-test-{}", Uuid::new_v4()))).unwrap();
        Chat::new(server, Arc::new(Vault::new(false)), &dir)
    }

    #[test]
    fn a_window_with_more_controls_than_fit_keeps_its_shape_and_counts_what_was_cut() {
        let targets: Vec<Value> = (0..120)
            .map(|index| json!({ "ref": format!("d-{index:08x}"), "role": "button", "name": format!("Bir düğmenin uzunca adı {index}"),
                "frame": { "x": 0.12345, "y": 0.54321, "width": 0.04, "height": 0.02 }, "actions": ["Click"], "enabled": true, "secure": false, "value": "" }))
            .collect();
        let data = json!({ "kind": "computerDesktop", "value": { "taskId": "t", "action": "observe", "targets": targets, "moreTargets": 3,
            "picture": { "mimeType": "image/jpeg", "dataBase64": "A".repeat(64), "width": 10, "height": 10 } } });
        assert!(data.to_string().len() > DATA_TEXT_MOST);
        let (fitted, picture) = fit_desktop(&data);
        assert!(fitted.to_string().len() <= DATA_TEXT_MOST);
        assert_eq!(picture.as_deref(), Some("A".repeat(64).as_str()), "the picture travels beside the data");
        assert!(fitted["value"]["picture"].is_null());
        let kept = fitted["value"]["targets"].as_array().unwrap().len();
        assert!(kept > 20 && kept < 120);
        assert_eq!(fitted["value"]["moreTargets"], 3 + 120 - kept as u64);
        assert_eq!(fitted["value"]["targets"][0]["ref"], "d-00000000", "the first controls, the ones a person acts with, stay");
    }

    #[test]
    fn a_yes_in_the_middle_of_a_list_runs_only_what_is_left() {
        let step = json!({ "id": "x", "type": "computerDesktop", "payload": { "kind": "computerDesktop", "value": { "taskId": "t", "action": "runSteps",
            "steps": [{ "action": "type", "text": "merhaba" }, { "action": "click", "targetRef": "d-1" }, { "action": "wait" }] } } });
        let refused = json!({ "errorCode": "DESKTOP_APPROVAL_REQUIRED", "dataText": json!({ "kind": "computerDesktop", "value": { "evidence": [
            { "index": 0, "ok": true }, { "index": 1, "ok": false } ] } }).to_string() });
        let again = rest_after_yes(&step, &refused);
        let value = &again["payload"]["value"];
        assert_eq!(value["cardApproved"], true);
        let steps = value["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 2, "what was typed is not typed again");
        assert_eq!(steps[0]["action"], "click");
        // One step alone runs again as it is.
        let single = json!({ "id": "y", "type": "computerDesktop", "payload": json!({ "kind": "computerDesktop", "value": { "taskId": "t", "action": "click", "targetRef": "d-1" } }).to_string() });
        let again = rest_after_yes(&single, &json!({}));
        assert_eq!(again["payload"]["value"]["cardApproved"], true);
        assert_eq!(again["payload"]["value"]["action"], "click");
    }

    #[tokio::test]
    async fn stopping_one_turn_leaves_the_others_running() {
        let chat = chat_at("http://127.0.0.1:9");
        let (phone, background) = (Arc::new(Stop::default()), Arc::new(Stop::default()));
        {
            let mut stops = chat.stops.lock().unwrap();
            stops.insert("phone-request".into(), phone.clone());
            stops.insert("background-task".into(), background.clone());
        }
        assert!(chat.stop_turn("PHONE-Request"));
        assert!(phone.is_asked());
        assert!(!background.is_asked());
        assert!(!chat.stop_turn("a turn that is not running"));
        assert!(tokio::time::timeout(Duration::from_secs(1), phone.asked()).await.is_ok());
        assert!(tokio::time::timeout(Duration::from_millis(50), background.asked()).await.is_err());
    }

    #[tokio::test]
    async fn a_stop_asked_while_someone_waits_for_it_is_heard() {
        let stop = Arc::new(Stop::default());
        let waiting = {
            let stop = stop.clone();
            tokio::spawn(async move { stop.asked().await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        stop.ask();
        assert!(tokio::time::timeout(Duration::from_secs(1), waiting).await.is_ok());
    }

    #[tokio::test]
    async fn the_results_of_steps_that_ran_reach_a_server_that_was_away() {
        use std::io::{Read, Write};
        // Nobody listens at first: the address is taken and given back, and the server comes up a moment later.
        let address = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let server = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            let listener = std::net::TcpListener::bind(address).unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let body = format!("data: {}\n\n", json!({ "type": "done", "reply": "tamam" }));
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            String::from_utf8_lossy(&request[..read]).to_string()
        });
        let chat = chat_at(&format!("http://{address}"));
        let said = Mutex::new(Vec::new());
        let answer = chat
            .stream_kept("test-only-token", &json!({ "requestId": "the-same-request", "deviceResults": [] }), &|event| said.lock().unwrap().push(event))
            .await
            .unwrap();
        assert_eq!(answer["reply"], "tamam");
        assert!(said.lock().unwrap().iter().any(|event| event["type"] == "status"), "the person is told the results wait for the connection");
        assert!(server.join().unwrap().contains("the-same-request"), "the same request goes again, so the server takes it once");
    }

    #[tokio::test]
    async fn a_request_the_server_refused_is_not_sent_again() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).unwrap();
            let body = json!({ "error": { "message": "Günlük sınır doldu." } }).to_string();
            write!(stream, "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });
        let chat = chat_at(&format!("http://{address}"));
        let error = chat.stream_kept("test-only-token", &json!({ "requestId": "r" }), &|_| {}).await.unwrap_err();
        assert_eq!(error.to_string(), "Günlük sınır doldu.");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn a_remote_tasks_steps_are_not_run_on_the_writing_computer() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0u8; 4096];
            assert!(stream.read(&mut request).unwrap() > 0);
            let body = format!(
                "data: {}\n\n",
                json!({ "type": "done", "steps": [{ "id": Uuid::new_v4(), "type": "openURL" }], "deviceConnectivity": { "deviceId": "OTHER-PC" } })
            );
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });
        let dir =
            DataDir::new(std::env::temp_dir().join(format!("wiqo-target-test-{}", Uuid::new_v4())))
                .unwrap();
        let chat = Chat::new(
            &format!("http://{address}"),
            Arc::new(Vault::new(false)),
            &dir,
        );
        chat.store(
            Some(Account {
                email: "test@test.invalid".into(),
                token: "test-only-token".into(),
            }),
            None,
        )
        .unwrap();
        let engine = Engine::new(
            wiqo_contract::Platform::current(),
            Arc::new(wiqo_core::storage::Settings::open(&dir)),
            Arc::new(wiqo_core::ledger::Ledger::open(&dir)),
            wiqo_core::engine::Services::temporary(),
        );
        let (events, _) = broadcast::channel(16);
        let result = chat
            .send(
                &engine,
                "THIS-PC",
                &events,
                "turn",
                Uuid::new_v4().to_string(),
                "conversation",
                json!([]),
                "Test",
                None,
                None,
                json!([]),
                &Asker::Window { mission: None },
                &|_, _| Box::pin(async { Decision::Yes }),
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("başka bir PC"));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn a_phones_message_goes_out_as_written_from_the_phone_and_its_stream_goes_back_to_it() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 65536];
            while let Ok(read) = stream.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&request);
                if let Some(end) = text.find("\r\n\r\n") {
                    let length: usize = text
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let text = String::from_utf8_lossy(&request).to_string();
            let body = format!(
                "data: {}\n\ndata: {}\n\n",
                json!({ "type": "delta", "text": "Merhaba" }),
                json!({ "type": "done", "reply": "Merhaba", "steps": [] })
            );
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            text
        });
        let dir =
            DataDir::new(std::env::temp_dir().join(format!("wiqo-remote-test-{}", Uuid::new_v4())))
                .unwrap();
        let chat = Chat::new(
            &format!("http://{address}"),
            Arc::new(Vault::new(false)),
            &dir,
        );
        chat.store(
            Some(Account {
                email: "test@test.invalid".into(),
                token: "test-only-token".into(),
            }),
            None,
        )
        .unwrap();
        let engine = Engine::new(
            wiqo_contract::Platform::current(),
            Arc::new(wiqo_core::storage::Settings::open(&dir)),
            Arc::new(wiqo_core::ledger::Ledger::open(&dir)),
            wiqo_core::engine::Services::temporary(),
        );
        let (events, _) = broadcast::channel(16);
        let forwarded = Arc::new(Mutex::new(Vec::<Value>::new()));
        let sink = forwarded.clone();
        let asker = Asker::Phone {
            request: json!({ "requestId": "phone-request-1", "clientFeatures": ["receipt-card-v1", "voice-reply-v1"], "locale": "de", "agentId": 7,
                "files": [{ "name": "a.txt" }], "deviceId": "SOMEWHERE-ELSE", "deviceResults": [{ "stepId": "x" }] }),
            forward: Box::new(move |event| sink.lock().unwrap().push(event)),
        };
        chat.send(
            &engine,
            "THIS-PC",
            &events,
            "turn",
            "phone-request-1".into(),
            "conversation",
            json!([]),
            "Hallo",
            Some("phone-task".into()),
            None,
            json!([]),
            &asker,
            &|_, _| Box::pin(async { Decision::Yes }),
        )
        .await
        .unwrap();
        let request = server.join().unwrap();
        let body: Value =
            serde_json::from_str(&request[request.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(body["requestId"], "phone-request-1");
        assert_eq!(body["taskId"], "phone-task");
        assert_eq!(body["surface"], "phone");
        assert_eq!(body["deviceId"], "THIS-PC");
        assert!(body.get("sourceDeviceId").is_none());
        assert_eq!(body["locale"], "de");
        assert_eq!(
            body["clientFeatures"],
            json!(["receipt-card-v1", "voice-reply-v1", "relay-execution-v1", "local-agent-v1", "local-agent-v2"]),
            "the phone's features, run here"
        );
        assert_eq!(body["agentId"], 7, "the phone's own fields go as they are");
        assert_eq!(body["files"][0]["name"], "a.txt");
        assert!(
            body.get("deviceResults").is_none(),
            "results of PC steps are never taken from the phone"
        );
        let kinds: Vec<Value> = forwarded
            .lock()
            .unwrap()
            .iter()
            .map(|event| event["type"].clone())
            .collect();
        assert_eq!(kinds, vec![json!("task"), json!("delta"), json!("done")]);
    }

    #[test]
    fn a_change_a_delete_or_a_send_waits_for_a_yes_and_a_read_does_not() {
        assert!(needs_yes(
            &json!({ "type": "deleteFile", "approvalRequired": true, "isDestructive": true })
        ));
        assert!(needs_yes(
            &json!({ "type": "sendEmail", "isDestructive": true })
        ));
        assert!(needs_yes(
            &json!({ "type": "moveFile", "isDestructive": true, "requiresUserPresence": false })
        ));
        assert!(!needs_yes(
            &json!({ "type": "readFile", "isDestructive": false, "requiresUserPresence": true })
        ));
        assert!(
            !needs_yes(&json!({ "type": "moveFile", "isDestructive": false })),
            "allowed once and for all"
        );
        let refused = refused_step(
            &json!({ "id": "s1", "type": "deleteFile" }),
            "Onay verilmedi; bu adım çalıştırılmadı.",
        );
        assert_eq!(refused["status"], "denied");
        assert_eq!(refused["errorCode"], "DENIED_ON_PC");
    }

    #[test]
    fn only_plain_ids_can_be_put_in_a_path() {
        assert!(check_id("agent-dm-12").is_ok());
        assert!(check_id("agent-group-6f1c0c52-3c1e-4b0e-8d6e-0f3a2b1c9d7e").is_ok());
        assert!(check_id("").is_err());
        assert!(check_id("../profile").is_err());
        assert!(check_id("a/b").is_err());
        assert!(check_id("a?b=c").is_err());
    }

    /// Writes the chat's device context of a real engine to `WIQO_CONTEXT_OUT` (the backend's schema checks it: `npm run`-free, see the session notes).
    #[test]
    fn device_context_has_only_the_fields_the_server_accepts() {
        let dir = DataDir::new(std::env::temp_dir().join(format!("wiqo-chat-{}", Uuid::new_v4())))
            .unwrap();
        let settings = Arc::new(wiqo_core::storage::Settings::open(&dir));
        let mut engine = Engine::new(
            wiqo_contract::Platform::current(),
            settings,
            Arc::new(wiqo_core::ledger::Ledger::open(&dir)),
            wiqo_core::engine::Services::temporary(),
        );
        wiqo_ops::register_all(&mut engine).unwrap();
        let context = device_context(&engine, "ABC");
        assert_eq!(context["surface"], "desktop");
        assert_eq!(context["sourceDeviceId"], context["deviceId"]);
        for target in context["applicationTargets"].as_array().unwrap() {
            for key in target.as_object().unwrap().keys() {
                assert!(
                    ["name", "bundleIdentifier", "isRunning", "supportedActions"]
                        .contains(&key.as_str()),
                    "{key}"
                );
            }
        }
        for authority in context["authorities"].as_array().unwrap() {
            assert_eq!(authority.as_object().unwrap().len(), 5);
        }
        if let Ok(path) = std::env::var("WIQO_CONTEXT_OUT") {
            std::fs::write(path, context.to_string()).unwrap();
        }
    }
}
