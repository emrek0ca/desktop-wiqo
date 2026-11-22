//! The one wire contract of Wiqo.
//!
//! Phones, the server and every desktop speak the same language, written down once in `contracts/companion/`:
//! `operations.json` (what can be asked, per platform), `error-codes.json` (what every failure means), `frames.json` (what the phone and the
//! desktop say to each other through the server's pipe) and `vectors.json` (the exact bytes of canonical JSON and signatures). This crate reads those files and nothing else defines the wire.

mod canonical;
mod errors;
pub mod frames;
mod manifest;
pub mod names;
mod policy;
pub mod schema_generated;
mod signature;

pub use canonical::{canonical_json, CanonicalError};
pub use errors::{Effect, ErrorClass, ErrorCodes, Recovery};
pub use manifest::{Manifest, Operation, Platform, TimeoutClass};
pub use names::AppAliases;
pub use policy::{is_valid_file_name, is_valid_local_path, FilePolicy};
pub use signature::{command_signature, remote_input_signature, remote_session_key, verify_signature};

/// The contract files, embedded at build time so a desktop build can never run against a different contract than it was tested with.
pub(crate) const OPERATIONS_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../contracts/companion/operations.json"
));
pub(crate) const ERROR_CODES_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../contracts/companion/error-codes.json"
));
pub(crate) const FILE_POLICY_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../contracts/companion/file-policy.json"
));
pub(crate) const FRAMES_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../contracts/companion/frames.json"
));
pub(crate) const APP_ALIASES_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../contracts/companion/app-aliases.json"
));

#[cfg(test)]
mod schema_tests {
    use super::schema_generated::{decodes, RunSyncEvent, StreamEvent};
    use std::path::PathBuf;

    fn contracts() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../contracts")
    }

    /// Every example the schema ships (contracts/schema/examples.json) decodes as its type with the code generated from the schema.
    #[test]
    fn every_schema_example_decodes_with_the_generated_types() {
        let manifest: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(contracts().join("schema/examples.json")).expect("manifest")).expect("manifest is JSON");
        assert!(manifest.len() >= 15, "the manifest lists the examples");
        for item in manifest {
            let (file, kind) = (item["file"].as_str().unwrap_or_default(), item["type"].as_str().unwrap_or_default());
            let text = std::fs::read_to_string(contracts().join(file)).unwrap_or_else(|_| panic!("{file} is missing"));
            assert_eq!(decodes(kind, &text).unwrap_or_else(|error| panic!("{file} as {kind}: {error}")), true, "{kind} is a type of the schema");
        }
    }

    /// A newer server's event of a kind this build does not know is read as unknown, not as an error.
    #[test]
    fn an_event_of_an_unknown_kind_is_ignored_and_a_known_one_keeps_its_fields() {
        let unknown: StreamEvent = serde_json::from_str(r#"{"type":"something_new","x":1}"#).expect("unknown kind");
        assert!(matches!(unknown, StreamEvent::Unknown));
        let step: RunSyncEvent = serde_json::from_str(include_str!("../../../../contracts/schema/examples/sync_step.json")).expect("step envelope");
        match step {
            RunSyncEvent::Copilot(update) => {
                assert_eq!(update.seq, Some(5));
                match update.event {
                    StreamEvent::StepUpdate(event) => {
                        assert_eq!(event.step.exit_code, Some(1));
                        assert_eq!(event.step.command.as_deref(), Some("pytest -q"));
                    }
                    other => panic!("not a step: {other:?}"),
                }
            }
            other => panic!("not a copilot update: {other:?}"),
        }
    }
}
