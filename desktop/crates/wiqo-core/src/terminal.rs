//! Terminal sessions as the runtime sees them: the work lives behind the `Terminal` trait (wiqo-ops); a step of the chat's stream runs there under a
//! grant this computer gave itself (one folder, a mode, an end time). Also the outbox the frames for the phones leave through.

use std::collections::VecDeque;
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

/// What the server or a phone waits for and nobody will say again: the receipt of a step this computer ran, the end of a phone's turn, how a question
/// ended, a job that finished. These are kept while there is no connection. A stream event is not: the phone reads the answer from the server then.
fn is_awaited(message: &Value) -> bool {
    match message["type"].as_str() {
        Some("automationStepDone" | "agentStepDone" | "watchObserved") => true,
        Some("companionFrame") => matches!(
            message["frame"]["kind"].as_str(),
            Some("turnEnd" | "approvalResolved" | "jobFinished")
        ),
        _ => false,
    }
}

/// How many kept messages wait at most, and for how long: the server gives up on a step's receipt long before, and refuses a late one by itself.
const KEPT_MOST: usize = 200;
const KEPT_FOR: std::time::Duration = std::time::Duration::from_secs(30 * 60);

#[derive(Default)]
struct Pipe {
    sender: Option<mpsc::Sender<String>>,
    kept: VecDeque<(std::time::Instant, String)>,
}

/// Frames for the phones and this desktop's own answers to the server: they go out on whichever connection to the server is open. While there is none,
/// what is awaited (`is_awaited`) is kept and leaves first on the next connection, in the order it was written; everything else is dropped.
#[derive(Clone, Default)]
pub struct Outbox(Arc<Mutex<Pipe>>);

impl Outbox {
    /// Waits for room on the socket instead of dropping the message when it is busy (a chat's stream to the phone must arrive whole and in order).
    pub async fn deliver(&self, message: &Value) {
        let sender = self.0.lock().expect("outbox lock").sender.clone();
        let text = message.to_string();
        let sent = match sender {
            Some(sender) => sender.send(text.clone()).await.is_ok(),
            None => false,
        };
        if !sent && is_awaited(message) {
            self.keep(text);
        }
    }

    fn keep(&self, text: String) {
        let mut pipe = self.0.lock().expect("outbox lock");
        if pipe.kept.len() >= KEPT_MOST {
            pipe.kept.pop_front();
        }
        pipe.kept.push_back((std::time::Instant::now(), text));
    }

    /// A message that was on its way when the connection ended (written as text already): kept if it is awaited.
    pub fn keep_unsent(&self, text: String) {
        if serde_json::from_str::<Value>(&text).is_ok_and(|message| is_awaited(&message)) {
            self.keep(text);
        }
    }

    /// What was kept while there was no connection, oldest first; the relay sends it before anything else on a new connection.
    pub fn take_kept(&self) -> Vec<String> {
        let mut pipe = self.0.lock().expect("outbox lock");
        pipe.kept
            .drain(..)
            .filter(|(at, _)| at.elapsed() < KEPT_FOR)
            .map(|(_, text)| text)
            .collect()
    }

    /// The socket the messages leave through (the relay attaches its own; nothing is sent while none is attached).
    pub fn attach(&self, sender: Option<mpsc::Sender<String>>) {
        self.0.lock().expect("outbox lock").sender = sender;
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
    /// Called once when the desktop starts: jobs the previous run left open are over, and are told as "not known".
    fn recover(&self, _services: &crate::engine::Services) {}
    /// Stops a running job by its id (the phone asked: `cancelJob`); false when no such job runs.
    fn cancel_job(&self, _job_id: &str) -> bool {
        false
    }
    /// The running jobs as (id, label), for a progress line while the person is away.
    fn running_jobs(&self) -> Vec<(String, String)> {
        Vec::new()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn what_is_awaited_waits_for_a_connection_and_a_stream_event_does_not() {
        let outbox = Outbox::default();
        outbox
            .deliver(&json!({ "type": "automationStepDone", "stepId": 1 }))
            .await;
        outbox
            .deliver(&json!({ "type": "companionFrame", "frame": { "kind": "streamEvent", "requestId": "r" } }))
            .await;
        outbox
            .deliver(&json!({ "type": "activityUpdate", "requestId": "r" }))
            .await;
        outbox
            .deliver(&json!({ "type": "companionFrame", "frame": { "kind": "turnEnd", "requestId": "r" } }))
            .await;
        let kept: Vec<Value> = outbox
            .take_kept()
            .iter()
            .map(|text| serde_json::from_str(text).unwrap())
            .collect();
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0]["type"], "automationStepDone");
        assert_eq!(kept[1]["frame"]["kind"], "turnEnd");
        assert!(outbox.take_kept().is_empty(), "what was taken is sent once");
    }

    #[tokio::test]
    async fn a_receipt_written_to_a_connection_that_ended_is_kept_and_one_that_left_is_not() {
        let outbox = Outbox::default();
        let (sender, mut receiver) = mpsc::channel(4);
        outbox.attach(Some(sender));
        outbox
            .deliver(&json!({ "type": "agentStepDone", "unitId": 7 }))
            .await;
        assert!(receiver.recv().await.is_some());
        assert!(outbox.take_kept().is_empty());
        drop(receiver);
        outbox
            .deliver(&json!({ "type": "agentStepDone", "unitId": 8 }))
            .await;
        outbox.keep_unsent(json!({ "type": "watchObserved", "triggerId": 3 }).to_string());
        outbox.keep_unsent(json!({ "type": "automationPull" }).to_string());
        assert_eq!(outbox.take_kept().len(), 2);
    }
}
