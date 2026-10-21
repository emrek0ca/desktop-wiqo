//! Every finished command and its answer, so the same command id is never run twice: a retry, a reconnect or a second delivery gets the stored answer.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::storage::{write_json, DataDir};
use crate::wire::CommandResult;

const CAPACITY: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub command_id: Uuid,
    /// What was asked (the request without its delivery details): a known id with a different digest is a different command and is refused.
    pub digest: String,
    pub result: CommandResult,
}

pub struct Ledger {
    path: Option<PathBuf>,
    entries: Mutex<Vec<Entry>>,
}

impl Ledger {
    pub fn open(dir: &DataDir) -> Ledger {
        let path = dir.file("ledger.json");
        let entries = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Ledger {
            path: Some(path),
            entries: Mutex::new(entries),
        }
    }

    /// A ledger that is not written to disk (tests).
    pub fn in_memory() -> Ledger {
        Ledger {
            path: None,
            entries: Mutex::new(Vec::new()),
        }
    }

    pub fn lookup(&self, command_id: Uuid) -> Option<Entry> {
        self.entries
            .lock()
            .expect("ledger lock")
            .iter()
            .find(|entry| entry.command_id == command_id)
            .cloned()
    }

    pub fn record(&self, command_id: Uuid, digest: String, result: CommandResult) {
        let mut entries = self.entries.lock().expect("ledger lock");
        entries.retain(|entry| entry.command_id != command_id);
        entries.push(Entry {
            command_id,
            digest,
            result,
        });
        if entries.len() > CAPACITY {
            let excess = entries.len() - CAPACITY;
            entries.drain(..excess);
        }
        if let Some(path) = &self.path {
            if let Err(error) = write_json(path, &*entries) {
                tracing::warn!("ledger could not be saved: {error}");
            }
        }
    }
}
