use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::canonical::{canonical_json, CanonicalError};

type HmacSha256 = Hmac<Sha256>;

fn hmac_hex(key: &str, message: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// `wiqo-command:v2`: the signature a phone puts on every command it sends to a desktop (and a desktop checks before running anything).
/// The payload enters as the SHA-256 of its canonical JSON, so the signature does not depend on key order or number spelling.
pub fn command_signature(
    token: &str,
    id: &str,
    command_type: &str,
    epoch_ms: u64,
    nonce: &str,
    payload: &Value,
) -> Result<String, CanonicalError> {
    let digest = hex::encode(Sha256::digest(canonical_json(payload)?.as_bytes()));
    Ok(hmac_hex(
        token,
        &format!(
            "wiqo-command:v2:{}:{}:{}:{}:{}",
            id.to_lowercase(),
            command_type,
            epoch_ms,
            nonce,
            digest
        ),
    ))
}

/// `wiqo-terminal-grant:v1`: the permission to run terminal work inside one folder, until a time, in `read` or `full` mode.
pub fn terminal_grant_signature(
    token: &str,
    grant_id: &str,
    root_directory: &str,
    mode: &str,
    expires_at: u64,
) -> String {
    hmac_hex(
        token,
        &format!(
            "wiqo-terminal-grant:v1:{}:{}:{}:{}",
            grant_id.to_lowercase(),
            root_directory,
            mode,
            expires_at
        ),
    )
}

/// Compares a received hex signature with the expected one without leaking where they differ.
pub fn verify_signature(expected_hex: &str, received_hex: &str) -> bool {
    let (Ok(expected), Ok(received)) = (hex::decode(expected_hex), hex::decode(received_hex))
    else {
        return false;
    };
    if expected.len() != received.len() {
        return false;
    }
    expected
        .iter()
        .zip(received.iter())
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}
