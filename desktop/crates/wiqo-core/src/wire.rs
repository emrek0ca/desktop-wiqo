//! The JSON shapes of a command and its result, as phones, the server and every desktop write them.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Denied,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

impl WireError {
    pub fn new(code: &str, message: impl Into<String>, category: &str) -> WireError {
        WireError {
            code: code.to_string(),
            message: message.into(),
            category: Some(category.to_string()),
        }
    }
}

/// `CommandRequest`: what is asked, and for phone commands the signature that proves who asked.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
    pub created_at: String,
    #[serde(default)]
    pub signature_version: Option<u32>,
    #[serde(default)]
    pub created_at_epoch_ms: Option<i64>,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub nonce: Option<String>,
}

/// `CommandResult`: the one answer to a command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    #[serde(rename = "commandID")]
    pub command_id: Uuid,
    pub status: CommandStatus,
    pub message: String,
    pub started_at: String,
    pub finished_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<WireError>,
}

pub fn timestamp(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl CommandResult {
    pub fn completed(
        command_id: Uuid,
        message: impl Into<String>,
        data: Option<Value>,
        started: DateTime<Utc>,
    ) -> CommandResult {
        CommandResult {
            command_id,
            status: CommandStatus::Completed,
            message: message.into(),
            started_at: timestamp(started),
            finished_at: timestamp(Utc::now()),
            data,
            error: None,
        }
    }

    pub fn failed(
        command_id: Uuid,
        status: CommandStatus,
        error: WireError,
        started: DateTime<Utc>,
    ) -> CommandResult {
        CommandResult {
            command_id,
            status,
            message: error.message.clone(),
            started_at: timestamp(started),
            finished_at: timestamp(Utc::now()),
            data: None,
            error: Some(error),
        }
    }
}
