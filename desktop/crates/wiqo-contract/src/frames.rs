//! `contracts/companion/frames.json`: the frames between the phone and the desktop. The server only passes them on; the desktop holds the chat and
//! runs every PC step, the phone is its remote screen.

use std::sync::OnceLock;

use serde::Deserialize;

/// Frames to the desktop (signed commands v2; `type` is one of these or an operation).
pub const REMOTE_MESSAGE: &str = "remoteMessage";
pub const APPROVAL_DECISION: &str = "approvalDecision";
pub const CANCEL_JOB: &str = "cancelJob";
pub const CANCEL_TURN: &str = "cancelTurn";
pub const APPROVALS_OPEN: &str = "approvalsOpen";
/// The live remote screen: join the session the phone opened on the server, and stream the screen to it (wiqo-agent remote_screen).
pub const REMOTE_SCREEN: &str = "remoteScreen";

/// Frames to the phone (`kind`).
pub const ACK: &str = "ack";
pub const STREAM_EVENT: &str = "streamEvent";
pub const TURN_END: &str = "turnEnd";
pub const APPROVAL_REQUEST: &str = "approvalRequest";
pub const APPROVAL_RESOLVED: &str = "approvalResolved";
pub const COMMAND_RESULT: &str = "commandResult";
pub const JOB_FINISHED: &str = "jobFinished";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Timing {
    ack_seconds: u64,
    approval_seconds: u64,
    /// How long a step of a chat waits for the person's yes: the person may be away from the phone for a while, and the task waits for them.
    #[serde(default = "default_chat_approval")]
    chat_approval_seconds: u64,
}

fn default_chat_approval() -> u64 {
    600
}

#[derive(Debug, Deserialize)]
struct ToDesktop {
    types: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ToPhone {
    kinds: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    timing: Timing,
    to_desktop: ToDesktop,
    to_phone: ToPhone,
}

#[derive(Debug)]
pub struct Frames {
    pub ack_seconds: u64,
    pub approval_seconds: u64,
    pub chat_approval_seconds: u64,
    to_desktop: Vec<String>,
    to_phone: Vec<String>,
}

impl Frames {
    pub fn get() -> &'static Frames {
        static FRAMES: OnceLock<Frames> = OnceLock::new();
        FRAMES.get_or_init(|| {
            let file: File =
                serde_json::from_str(crate::FRAMES_JSON).expect("frames.json is valid");
            Frames {
                ack_seconds: file.timing.ack_seconds,
                approval_seconds: file.timing.approval_seconds,
                chat_approval_seconds: file.timing.chat_approval_seconds,
                to_desktop: file.to_desktop.types.keys().cloned().collect(),
                to_phone: file.to_phone.kinds.keys().cloned().collect(),
            }
        })
    }

    pub fn is_to_desktop(&self, kind: &str) -> bool {
        self.to_desktop.iter().any(|item| item == kind)
    }

    pub fn is_to_phone(&self, kind: &str) -> bool {
        self.to_phone.iter().any(|item| item == kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_frame_this_desktop_speaks_is_in_the_contract_and_nothing_else_is() {
        let frames = Frames::get();
        let mut to_desktop = vec![
            REMOTE_MESSAGE,
            APPROVAL_DECISION,
            CANCEL_JOB,
            CANCEL_TURN,
            APPROVALS_OPEN,
        ];
        let mut to_phone = vec![
            ACK,
            STREAM_EVENT,
            TURN_END,
            APPROVAL_REQUEST,
            APPROVAL_RESOLVED,
            COMMAND_RESULT,
            JOB_FINISHED,
        ];
        to_desktop.sort();
        to_phone.sort();
        let mut listed_desktop: Vec<&str> = frames.to_desktop.iter().map(String::as_str).collect();
        let mut listed_phone: Vec<&str> = frames.to_phone.iter().map(String::as_str).collect();
        listed_desktop.sort();
        listed_phone.sort();
        assert_eq!(to_desktop, listed_desktop);
        assert_eq!(to_phone, listed_phone);
        assert_eq!(frames.ack_seconds, 3);
        assert_eq!(frames.approval_seconds, 90);
        assert_eq!(frames.chat_approval_seconds, 600);
    }
}
