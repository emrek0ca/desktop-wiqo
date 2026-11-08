//! Terminal sessions as the runtime sees them: the work lives behind the `Terminal` trait (wiqo-ops); a step of the chat's stream runs there under a
//! grant this computer gave itself (one folder, a mode, an end time). Also the outbox the frames for the phones leave through.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::engine::Services;
use crate::wire::CommandResult;

pub const CODE_CAPABILITY: &str = "device.code.execute";

/// `TerminalGrantPayload`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    pub id: String,
    pub root_directory: String,
    pub mode: String,
    pub expires_at: i64,
    #[serde(default)]
    pub signature: String,
}

/// Whether a grant is this computer's own and still open (a plan re-checks before every step).
pub type GrantCheck = Arc<dyn Fn(&Grant) -> bool + Send + Sync>;

/// Frames for the phones: they go out on whichever connection to the server is open, and are dropped while there is none.
#[derive(Clone, Default)]
pub struct Outbox(Arc<Mutex<Option<mpsc::Sender<String>>>>);

impl Outbox {
    /// Waits for room on the socket instead of dropping the message when it is busy (a chat's stream to the phone must arrive whole and in order).
    pub async fn deliver(&self, message: &Value) {
        let sender = self.0.lock().expect("outbox lock").clone();
        if let Some(sender) = sender {
            let _ = sender.send(message.to_string()).await;
        }
    }

    /// The socket the messages leave through (the relay attaches its own; nothing is sent while none is attached).
    pub fn attach(&self, sender: Option<mpsc::Sender<String>>) {
        *self.0.lock().expect("outbox lock") = sender;
    }
}

/// What one terminal request may use.
#[derive(Clone)]
pub struct Session {
    pub services: Arc<Services>,
    pub grants: GrantCheck,
    /// Code execution is on in this desktop's settings.
    pub code_enabled: bool,
}

#[async_trait]
pub trait Terminal: Send + Sync {
    /// The `terminal-session-v*` features this terminal implements.
    fn features(&self) -> Vec<&'static str>;
    /// The developer-environment summary for the model (OS, shell, tools, projects), once it is ready.
    fn profile(&self) -> Option<Value>;
    /// Runs one terminal request. `digest` identifies the request: the same command id with the same request gets the stored answer, never a second run.
    async fn handle(
        &self,
        session: Session,
        command_id: &str,
        request: &Value,
        digest: &str,
    ) -> CommandResult;
    /// "Durdur": stops every running background job.
    fn cancel_all(&self);
}
