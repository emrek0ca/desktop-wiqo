//! The chat in the window: the person signs in with their own Wiqo account and talks to the same `/copilot/chat` the phone uses, with this computer
//! as the device. PC steps the answer asks for run right here (the person typed the request at this computer, like a phone's task authorizes its
//! steps); a step that changes, deletes or sends waits for a click in the corner panel. Results go back to the server until the task is done.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use uuid::Uuid;
use wiqo_core::engine::{Engine, Origin};
use wiqo_core::storage::DataDir;
use wiqo_core::vault::{Account, Vault};
use wiqo_core::wire::{CommandStatus, Request};

use crate::Event;

const CLIENT_FEATURES: [&str; 3] = [
    "artifact-directory-v1",
    "relay-execution-v1",
    "receipt-card-v1",
];
const MAX_ROUNDS: usize = 8;

pub struct Chat {
    server: String,
    vault: Arc<Vault>,
    file: std::path::PathBuf,
    account: Mutex<Option<Account>>,
    http: reqwest::Client,
    stop: AtomicBool,
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
                .build()
                .unwrap_or_default(),
            stop: AtomicBool::new(false),
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

    fn token(&self) -> Result<String> {
        self.account
            .lock()
            .expect("account lock")
            .as_ref()
            .map(|account| account.token.clone())
            .ok_or_else(|| anyhow!("Önce Wiqo hesabınla giriş yap."))
    }

    fn store(&self, account: Option<Account>) -> Result<()> {
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
        *self.account.lock().expect("account lock") = account;
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
            let _ = self.store(None);
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
        self.store(Some(Account {
            email: email.clone(),
            token,
        }))?;
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
        self.store(Some(Account {
            email: email.clone(),
            token,
        }))?;
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
        self.store(Some(Account {
            email: email.clone(),
            token,
        }))?;
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
        Ok(
            json!({ "providers": providers["providers"], "connections": connections["connections"], "capabilities": capabilities["capabilities"] }),
        )
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
        self.store(None)
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
            let _ = self.store(None);
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

    /// Answers a card the server asked to confirm (a mail, a delete, a terminal folder). A terminal card's grant is signed here with a paired
    /// phone's key, which is exactly what this desktop verifies before it runs a terminal command.
    pub async fn confirm(
        &self,
        engine_phones: &[wiqo_core::storage::PairedPhone],
        device_id: &str,
        confirmation: &Value,
        approved: bool,
    ) -> Result<Value> {
        let token = self.token()?;
        let id = confirmation["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Onay kartı okunamadı."))?;
        let mut body = json!({ "confirmationId": id, "approved": approved });
        let details = &confirmation["details"];
        if approved && !details["wiqoGrantId"].is_null() {
            let grant = (|| {
                let phone = engine_phones.first()?;
                if !details["wiqoGrantDeviceId"].as_str()?.eq_ignore_ascii_case(device_id) {
                    return None;
                }
                let expires: u64 = details["wiqoGrantExpiresAt"].as_str()?.parse().ok()?;
                let signature = wiqo_contract::terminal_grant_signature(
                    &phone.token,
                    details["wiqoGrantId"].as_str()?,
                    details["wiqoGrantRoot"].as_str()?,
                    details["wiqoGrantMode"].as_str()?,
                    expires,
                );
                Some(json!({ "grantId": details["wiqoGrantId"], "signature": signature, "scope": "task" }))
            })()
            .ok_or_else(|| anyhow!("Terminal izni imzalanamadı; bu bilgisayarın bir telefonla eşleşmesi gerekiyor."))?;
            body["terminalGrant"] = grant;
        }
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

    /// "Durdur": the running turn stops before its next PC step, and the server is told to stop the task.
    pub async fn cancel(&self, task: &str, conversation: &str) {
        self.stop.store(true, Ordering::SeqCst);
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

    /// One message: the answer streams to the window as `chat` events (`turn` names this message); PC steps run and their results go back until
    /// the server's answer has no more steps.
    #[allow(clippy::too_many_arguments)]
    pub async fn send(
        &self,
        engine: &Engine,
        device_id: &str,
        events: &broadcast::Sender<Event>,
        turn: &str,
        conversation: &str,
        history: Value,
        text: &str,
        answer_to: Option<(String, String)>,
        attachments: Value,
    ) -> Result<()> {
        self.stop.store(false, Ordering::SeqCst);
        let token = self.token()?;
        let emit = |event: Value| {
            let _ = events.send(Event::Chat {
                turn: turn.to_string(),
                event,
            });
        };
        // An answer to a question the running task asked keeps that task's id and the question's id.
        let task = answer_to
            .as_ref()
            .map(|(task, _)| task.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let context = device_context(engine, device_id);
        let mut messages: Vec<Value> = history.as_array().cloned().unwrap_or_default();
        messages.push(json!({ "role": "user", "content": text }));
        let mut body = json!({
            "messages": messages, "conversationId": conversation, "requestId": Uuid::new_v4().to_string(), "taskId": task,
            "clientFeatures": CLIENT_FEATURES, "locale": "tr",
        });
        if let Some((_, question)) = &answer_to {
            body["questionId"] = json!(question);
        }
        // Pictures the person added: {name, mimeType, dataBase64}, at most four.
        if let Some(items) = attachments.as_array().filter(|items| !items.is_empty()) {
            body["attachments"] = Value::Array(items.iter().take(4).cloned().collect());
        }
        merge(&mut body, &context);
        emit(json!({ "type": "task", "taskId": task }));
        let mut answer = self.stream(&token, &body, &emit).await?;
        for _ in 0..MAX_ROUNDS {
            let steps = answer["steps"].as_array().cloned().unwrap_or_default();
            if steps.is_empty() || answer["type"] == "error" {
                return Ok(());
            }
            let mut results = Vec::new();
            let mut uncertain = false;
            for step in &steps {
                if self.stop.load(Ordering::SeqCst) {
                    results.push(json!({ "stepId": step["id"], "type": step["type"], "status": "denied", "message": "Kullanıcı görevi durdurdu; bu adım çalıştırılmadı.", "errorCode": "PERMISSION_DENIED" }));
                    break;
                }
                emit(
                    json!({ "type": "step_update", "step": { "id": step["id"], "summary": step["summary"], "status": "running" } }),
                );
                let result = run_step(engine, step).await;
                emit(
                    json!({ "type": "step_update", "step": { "id": step["id"], "summary": step["summary"], "status": result["status"], "message": result["message"] } }),
                );
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
            let mut next = json!({
                "messages": [
                    { "role": "user", "content": text },
                    { "role": "assistant", "content": answer["reply"].as_str().unwrap_or("") },
                    { "role": "user", "content": "Yerel PC sonuçları hazır. Bu sonuçları kullanarak önceki isteği şimdi tamamla; belge içeriğini yeniden isteme." },
                ],
                "conversationId": conversation, "requestId": Uuid::new_v4().to_string(), "taskId": answer["taskId"].as_str().unwrap_or(&task),
                "deviceResults": results, "clientFeatures": CLIENT_FEATURES, "locale": "tr",
            });
            merge(&mut next, &context);
            emit(json!({ "type": "reset" }));
            answer = self.stream(&token, &next, &emit).await?;
            if uncertain {
                return Ok(());
            }
        }
        emit(
            json!({ "type": "status", "text": "Görev adım sınırına ulaştı; devam etmek için yeni bir mesaj gönder." }),
        );
        Ok(())
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
            .map_err(|_| anyhow!("Sunucuya ulaşılamadı; internet bağlantını kontrol et."))?;
        if response.status().as_u16() == 401 {
            let _ = self.store(None);
            bail!("Oturumun sona erdi; yeniden giriş yap.");
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
        while let Some(chunk) = bytes.next().await {
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

    async fn follow(&self, token: &str, request_id: &str, emit: &impl Fn(Value)) -> Result<Value> {
        let mut delay = 1;
        for _ in 0..120 {
            tokio::time::sleep(Duration::from_secs(delay)).await;
            delay = (delay * 2).min(8);
            let Ok(state) = self
                .call(
                    reqwest::Method::GET,
                    &format!("copilot/requests/{request_id}"),
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
    json!({ "deviceId": device_id, "availableApps": apps, "availableCapabilities": capabilities, "applicationTargets": targets, "authorities": authorities })
}

/// Runs one step of the answer here and describes the result the way the server reads `deviceResults`.
async fn run_step(engine: &Engine, step: &Value) -> Value {
    let kind = step["type"].as_str().unwrap_or("unknown").to_string();
    let refused = |message: &str, code: &str| json!({ "stepId": step["id"], "type": kind, "status": "denied", "message": message, "errorCode": code });
    let Some(id) = step["id"].as_str().and_then(|id| Uuid::parse_str(id).ok()) else {
        return refused("PC adımı okunamadı; işlem yapılmadı.", "INVALID_STEP");
    };
    let payload = match &step["payload"] {
        Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
        other => other.clone(),
    };
    if payload.is_null() {
        return refused("PC adımı okunamadı; işlem yapılmadı.", "INVALID_STEP");
    }
    let summary = step["summary"].as_str().unwrap_or("").to_string();
    let sensitive = step["approvalRequired"] == true
        || step["isDestructive"] == true
        || matches!(kind.as_str(), "deleteFile" | "sendEmail" | "sendMessage");
    if sensitive
        && !engine
            .ask_on_pc(&kind, if summary.is_empty() { &kind } else { &summary })
            .await
    {
        return refused("Kullanıcı PC adımına izin vermedi.", "PERMISSION_DENIED");
    }
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
    let result = engine.execute(&request, &Origin::Local).await;
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
            let text = shown.to_string();
            fields["dataText"] = json!(if text.len() <= 11_500 {
                text
            } else {
                json!({ "kind": "result", "truncated": true, "preview": text.chars().take(5_000).collect::<String>() }).to_string()
            });
        }
    }
    fields
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
