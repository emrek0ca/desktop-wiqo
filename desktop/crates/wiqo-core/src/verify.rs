//! Who sent a command. A phone signs every command with the key it got when it paired (`wiqo-command:v2`); the desktop accepts it only from a phone it knows,
//! only once, and only within ten minutes of the moment it says it was made.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use wiqo_contract::{command_signature, verify_signature};

use crate::wire::Request;

const MAX_AGE_MS: i64 = 600_000;
const NONCE_TTL: Duration = Duration::from_secs(600);
const NONCE_CAPACITY: usize = 20_000;

/// Nonces already used, so a signed command cannot be replayed.
#[derive(Default)]
pub struct NonceCache {
    seen: Mutex<HashMap<String, Instant>>,
}

impl NonceCache {
    fn claim(&self, token: &str, nonce: &str) -> bool {
        let key = format!(
            "{}:{}",
            hex::encode(Sha256::digest(token.as_bytes())),
            hex::encode(Sha256::digest(nonce.as_bytes()))
        );
        let mut seen = self.seen.lock().expect("nonce cache lock");
        let now = Instant::now();
        seen.retain(|_, at| now.duration_since(*at) <= NONCE_TTL);
        if seen.contains_key(&key) {
            return false;
        }
        if seen.len() >= NONCE_CAPACITY {
            if let Some(oldest) = seen
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(key, _)| key.clone())
            {
                seen.remove(&oldest);
            }
        }
        seen.insert(key, now);
        true
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Rejection {
    /// The request does not use signature version 2.
    VersionRequired,
    /// No paired phone's key makes this signature, or the request is stale, malformed or replayed.
    Invalid,
}

/// Returns the pairing id of the phone whose key signed `request`. `raw_payload` is the payload exactly as it arrived: a field a newer sender added is part of
/// what it signed, and a re-encoded model would have dropped it.
pub fn signer_of<'a>(
    request: &Request,
    raw_payload: &Value,
    phones: impl Iterator<Item = (&'a str, &'a str)>,
    nonces: &NonceCache,
    now: DateTime<Utc>,
) -> Result<String, Rejection> {
    if request.signature_version != Some(2) {
        return Err(Rejection::VersionRequired);
    }
    let (Some(signature), Some(epoch_ms), Some(nonce)) = (
        request.signature.as_deref(),
        request.created_at_epoch_ms,
        request.nonce.as_deref(),
    ) else {
        return Err(Rejection::Invalid);
    };
    if signature.is_empty() || nonce.is_empty() || nonce.chars().count() > 200 {
        return Err(Rejection::Invalid);
    }
    if (now.timestamp_millis() - epoch_ms).abs() > MAX_AGE_MS {
        return Err(Rejection::Invalid);
    }
    let Ok(created_at) = DateTime::parse_from_rfc3339(&request.created_at) else {
        return Err(Rejection::Invalid);
    };
    if (created_at.timestamp_millis() - epoch_ms).abs() >= 1_000 {
        return Err(Rejection::Invalid);
    }
    for (pairing_id, token) in phones {
        let Ok(expected) = command_signature(
            token,
            &request.id.to_string(),
            &request.kind,
            epoch_ms as u64,
            nonce,
            raw_payload,
        ) else {
            continue;
        };
        if verify_signature(&expected, signature) {
            return if nonces.claim(token, nonce) {
                Ok(pairing_id.to_string())
            } else {
                Err(Rejection::Invalid)
            };
        }
    }
    Err(Rejection::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn signed(token: &str, nonce: &str, epoch_ms: i64) -> (Request, Value) {
        let payload = json!({"kind": "none"});
        let id = uuid::Uuid::new_v4();
        let signature = command_signature(
            token,
            &id.to_string(),
            "ping",
            epoch_ms as u64,
            nonce,
            &payload,
        )
        .unwrap();
        let created_at = crate::wire::timestamp(DateTime::from_timestamp_millis(epoch_ms).unwrap());
        (
            Request {
                id,
                kind: "ping".into(),
                payload: payload.clone(),
                created_at,
                signature_version: Some(2),
                created_at_epoch_ms: Some(epoch_ms),
                signature: Some(signature),
                nonce: Some(nonce.into()),
            },
            payload,
        )
    }

    #[test]
    fn a_known_phone_is_accepted_once_and_a_replay_is_refused() {
        let now = Utc::now();
        let nonces = NonceCache::default();
        let (request, payload) = signed("key-a", "n1", now.timestamp_millis());
        let phones = [("pair-1", "key-b"), ("pair-2", "key-a")];
        assert_eq!(
            signer_of(&request, &payload, phones.iter().copied(), &nonces, now),
            Ok("pair-2".into())
        );
        assert_eq!(
            signer_of(&request, &payload, phones.iter().copied(), &nonces, now),
            Err(Rejection::Invalid)
        );
    }

    #[test]
    fn unknown_keys_stale_commands_and_old_versions_are_refused() {
        let now = Utc::now();
        let nonces = NonceCache::default();
        let (request, payload) = signed("key-a", "n2", now.timestamp_millis());
        assert_eq!(
            signer_of(
                &request,
                &payload,
                [("p", "other")].into_iter(),
                &nonces,
                now
            ),
            Err(Rejection::Invalid)
        );
        let (stale, payload) = signed("key-a", "n3", now.timestamp_millis() - 700_000);
        assert_eq!(
            signer_of(&stale, &payload, [("p", "key-a")].into_iter(), &nonces, now),
            Err(Rejection::Invalid)
        );
        let (mut old, payload) = signed("key-a", "n4", now.timestamp_millis());
        old.signature_version = Some(1);
        assert_eq!(
            signer_of(&old, &payload, [("p", "key-a")].into_iter(), &nonces, now),
            Err(Rejection::VersionRequired)
        );
    }
}
