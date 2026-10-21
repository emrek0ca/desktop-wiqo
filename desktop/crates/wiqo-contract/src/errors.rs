use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

/// What a failure did to the world: nothing, part of the work, or it is not known (then the same action is never repeated).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    None,
    Partial,
    Uncertain,
}

/// What may be done next without asking: look again, try once more, or stop. No recovery means the model may choose another route or ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recovery {
    Observe,
    RetryOnce,
    Stop,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct ErrorClass {
    #[serde(default)]
    pub blocked: bool,
    pub effect: Effect,
    pub recovery: Option<Recovery>,
}

#[derive(Debug, Deserialize)]
struct File {
    version: u32,
    codes: HashMap<String, ErrorClass>,
}

/// `contracts/companion/error-codes.json`: the meaning of every failure code the desktops, workers and server report.
#[derive(Debug)]
pub struct ErrorCodes {
    pub version: u32,
    codes: HashMap<String, ErrorClass>,
}

impl ErrorCodes {
    pub fn get() -> &'static ErrorCodes {
        static CODES: OnceLock<ErrorCodes> = OnceLock::new();
        CODES.get_or_init(|| {
            let file: File =
                serde_json::from_str(crate::ERROR_CODES_JSON).expect("error-codes.json is valid");
            ErrorCodes {
                version: file.version,
                codes: file.codes,
            }
        })
    }

    pub fn class_of(&self, code: &str) -> Option<&ErrorClass> {
        self.codes.get(code)
    }

    pub fn len(&self) -> usize {
        self.codes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }
}
