use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::canonical::{canonical_json, CanonicalError};

type HmacSha256 = Hmac<Sha256>;

/// The live remote screen's key (frames.json remoteScreen): HMAC of the session and its nonce with the phone's pairing token, so the phone and this
/// desktop hold it and the server that joins them does not. Every input of the session is signed with it (`remote_input_signature`).
pub fn remote_session_key(token: &str, session: &str, nonce: &str) -> String {
    hmac_hex(token, &format!("wiqo-remote:v1:{session}:{nonce}"))
}

/// The signature of one input message of a live remote screen: HMAC of the message text with the session key.
pub fn remote_input_signature(session_key: &str, message: &str) -> String {
    hmac_hex(session_key, message)
}

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
