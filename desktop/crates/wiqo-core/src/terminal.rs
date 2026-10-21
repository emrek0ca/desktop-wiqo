//! Terminal sessions as the runtime sees them: a `terminalCommand` from the server carries a grant the phone signed (one folder, a mode, an end time),
//! and the work itself lives behind the `Terminal` trait (wiqo-ops). The runtime checks the grant's signature, hands over the request, and carries the
//! answer and the background-job updates back to the server.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use wiqo_contract::{terminal_grant_signature, verify_signature};

use crate::engine::Services;
use crate::storage::{PairedPhone, Settings};
use crate::wire::CommandResult;

/// Longest grant a phone may sign (terminal-session-v3: folder grants of up to 30 days).
pub const MAX_GRANT_SECONDS: i64 = 30 * 24 * 60 * 60;
/// The server's own grant while full access is on: never signed, at most 30 minutes, full mode.
pub const LOCAL_FULL_ACCESS: &str = "local-full-access-v1";
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

/// Whether a grant is valid right now: signed by one of the paired phones and not expired, or the server's own full-access grant while full access is on.
pub fn accepts(grant: &Grant, phones: &[PairedPhone], settings: &Settings) -> bool {
    let now = Utc::now().timestamp();
    if grant.signature == LOCAL_FULL_ACCESS {
        return settings.full_access()
            && settings.capability_enabled(CODE_CAPABILITY)
            && grant.expires_at > now
            && grant.expires_at <= now + 1805
            && uuid::Uuid::parse_str(&grant.id).is_ok()
            && grant.mode == "full";
    }
    if !matches!(grant.mode.as_str(), "read" | "full")
        || grant.expires_at <= now
        || grant.expires_at > now + MAX_GRANT_SECONDS
    {
        return false;
    }
    phones.iter().any(|phone| {
        let expected = terminal_grant_signature(
            &phone.token,
            &grant.id,
            &grant.root_directory,
            &grant.mode,
            grant.expires_at as u64,
        );
        verify_signature(&expected, &grant.signature)
    })
}

/// Checks a grant against the phones paired at the moment of the check (a plan re-checks before every step).
pub type GrantCheck = Arc<dyn Fn(&Grant) -> bool + Send + Sync>;

/// Messages the terminal sends on its own (`terminalJobUpdate`): they go out on whichever connection is open, and are dropped while there is none.
#[derive(Clone, Default)]
pub struct Outbox(Arc<Mutex<Option<mpsc::Sender<String>>>>);

impl Outbox {
    pub fn send(&self, message: &Value) {
        if let Some(sender) = self.0.lock().expect("outbox lock").as_ref() {
            let _ = sender.try_send(message.to_string());
        }
    }

    pub(crate) fn attach(&self, sender: Option<mpsc::Sender<String>>) {
        *self.0.lock().expect("outbox lock") = sender;
    }
}

/// What one terminal request may use.
#[derive(Clone)]
pub struct Session {
    pub services: Arc<Services>,
    pub grants: GrantCheck,
    pub outbox: Outbox,
    /// Code execution is on in this desktop's settings.
    pub code_enabled: bool,
}

#[async_trait]
pub trait Terminal: Send + Sync {
    /// The `terminal-session-v*` features this terminal implements.
    fn features(&self) -> Vec<&'static str>;
    /// The developer-environment summary for the model (OS, shell, tools, projects), once it is ready.
    fn profile(&self) -> Option<Value>;
    /// Runs one `terminalCommand`. `digest` identifies the request: the same command id with the same request gets the stored answer, never a second run.
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

    #[test]
    fn a_grant_is_valid_only_with_a_paired_phones_signature_and_in_time() {
        let dir = crate::storage::DataDir::new(
            std::env::temp_dir().join(format!("wiqo-grant-{}", uuid::Uuid::new_v4())),
        )
        .unwrap();
        let settings = Settings::open(&dir);
        let phone = PairedPhone {
            pairing_id: "p".into(),
            name: "iPhone".into(),
            platform: "iOS".into(),
            token: "secret".into(),
        };
        let expires = Utc::now().timestamp() + 600;
        let mut grant = Grant {
            id: uuid::Uuid::new_v4().to_string(),
            root_directory: "/Users/a/app".into(),
            mode: "full".into(),
            expires_at: expires,
            signature: String::new(),
        };
        grant.signature = terminal_grant_signature(
            "secret",
            &grant.id,
            &grant.root_directory,
            &grant.mode,
            expires as u64,
        );
        assert!(accepts(&grant, std::slice::from_ref(&phone), &settings));
        assert!(!accepts(&grant, &[], &settings), "no phone, no grant");
        let wider = Grant {
            root_directory: "/Users/a".into(),
            ..grant.clone()
        };
        assert!(
            !accepts(&wider, std::slice::from_ref(&phone), &settings),
            "the signature covers the folder"
        );
        let local = Grant {
            signature: LOCAL_FULL_ACCESS.into(),
            ..grant.clone()
        };
        assert!(
            !accepts(&local, &[], &settings),
            "the server's own grant needs full access on"
        );
        settings.update(|data| data.full_access = true).unwrap();
        assert!(accepts(&local, &[], &settings));
    }
}
