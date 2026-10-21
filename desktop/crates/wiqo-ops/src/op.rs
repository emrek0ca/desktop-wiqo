//! The small vocabulary every operation module shares: what an operation hands back, how a payload is read, and a wrapper that runs blocking work off the async threads.

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;
use wiqo_core::engine::{Context, Lane, Operation, Outcome};
use wiqo_core::wire::WireError;

/// What a finished operation returns: the message for the person and the contract's `CommandData` (`{kind, value}`) when it has one.
#[derive(Debug)]
pub struct Done {
    pub message: String,
    pub data: Option<Value>,
    /// Set when part of the work happened before it failed: the person sees both what was done and why it stopped.
    pub error: Option<WireError>,
}

impl Done {
    pub fn new(message: impl Into<String>, data: Option<Value>) -> Done {
        Done {
            message: message.into(),
            data,
            error: None,
        }
    }

    /// A failure that still reports what it did (`PARTIAL_MOVE` with the files that moved).
    pub fn partial(error: WireError, data: Value) -> Done {
        Done {
            message: error.message.clone(),
            data: Some(data),
            error: Some(error),
        }
    }

    pub fn message(message: impl Into<String>) -> Done {
        Done {
            message: message.into(),
            data: None,
            error: None,
        }
    }
}

pub type Result<T> = std::result::Result<T, WireError>;

pub fn error(code: &str, message: impl Into<String>, category: &str) -> WireError {
    WireError::new(code, message, category)
}

pub fn tool_failure(code: &str, message: impl Into<String>) -> WireError {
    WireError::new(code, message, "tool_failure")
}

pub fn permission(code: &str, message: impl Into<String>) -> WireError {
    WireError::new(code, message, "permission")
}

/// `{"kind": ..., "value": ...}`: the payload of a command, read as the type its kind promises.
pub fn payload<T: DeserializeOwned>(value: &Value, kind: &str, what: &str) -> Result<T> {
    if value["kind"] != kind {
        return Err(error(
            "INVALID_PAYLOAD",
            format!("Geçersiz {what} isteği."),
            "validation",
        ));
    }
    serde_json::from_value(value["value"].clone()).map_err(|_| {
        error(
            "INVALID_PAYLOAD",
            format!("{what} bilgisi geçersiz."),
            "validation",
        )
    })
}

/// `{"kind": kind, "value": value}`: the data half of a result.
pub fn data(kind: &str, value: impl serde::Serialize) -> Value {
    serde_json::json!({ "kind": kind, "value": value })
}

pub fn outcome(result: Result<Done>) -> Outcome {
    match result {
        Ok(done) => Outcome {
            message: done.message,
            data: done.data,
            error: done.error,
        },
        Err(error) => Outcome::failed(error),
    }
}

/// An operation that is plain blocking work (files, the clipboard, a system call).
pub struct Blocking {
    pub name: &'static str,
    pub lane: Lane,
    pub run: fn(&Context, &Value) -> Result<Done>,
}

#[async_trait]
impl Operation for Blocking {
    fn name(&self) -> &'static str {
        self.name
    }

    fn lane(&self) -> Lane {
        self.lane
    }

    async fn run(&self, ctx: &Context, payload: &Value) -> Outcome {
        let (context, payload, run) = (ctx.clone(), payload.clone(), self.run);
        match tokio::task::spawn_blocking(move || run(&context, &payload)).await {
            Ok(result) => outcome(result),
            Err(_) => Outcome::failed(error(
                "OUTCOME_UNKNOWN",
                "İşlem beklenmedik şekilde durdu; sonucu bilinmiyor.",
                "tool_failure",
            )),
        }
    }
}
