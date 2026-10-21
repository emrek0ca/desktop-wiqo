//! The one wire contract of Wiqo.
//!
//! Phones, the server and every desktop speak the same language, written down once in `contracts/companion/`:
//! `operations.json` (what can be asked, per platform), `error-codes.json` (what every failure means) and `vectors.json`
//! (the exact bytes of canonical JSON and signatures). This crate reads those files and nothing else defines the wire.

mod canonical;
mod errors;
mod manifest;
pub mod names;
mod policy;
mod signature;

pub use canonical::{canonical_json, CanonicalError};
pub use errors::{Effect, ErrorClass, ErrorCodes, Recovery};
pub use manifest::{Manifest, Operation, Platform, TimeoutClass};
pub use names::AppAliases;
pub use policy::{is_valid_file_name, is_valid_local_path, FilePolicy};
pub use signature::{command_signature, terminal_grant_signature, verify_signature};

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
pub(crate) const APP_ALIASES_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../contracts/companion/app-aliases.json"
));
