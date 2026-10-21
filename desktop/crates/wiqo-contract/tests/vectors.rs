//! The reference vectors every companion must reproduce byte for byte (contracts/companion/vectors.json).

use serde_json::Value;
use wiqo_contract::{
    canonical_json, command_signature, terminal_grant_signature, verify_signature, ErrorCodes,
    Manifest, Platform,
};

fn vectors() -> Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../contracts/companion/vectors.json"
    )))
    .expect("vectors.json is valid")
}

#[test]
fn canonical_json_matches_every_reference_vector() {
    let vectors = vectors();
    let cases = vectors["canonicalJson"]
        .as_array()
        .expect("canonicalJson list");
    assert!(!cases.is_empty());
    for case in cases {
        let input: Value = serde_json::from_str(case["input"].as_str().unwrap()).unwrap();
        assert_eq!(
            canonical_json(&input).unwrap(),
            case["canonical"].as_str().unwrap(),
            "input {}",
            case["input"]
        );
    }
}

#[test]
fn terminal_grant_signatures_match() {
    let vectors = vectors();
    for grant in vectors["terminalGrants"].as_array().unwrap() {
        let signature = terminal_grant_signature(
            grant["token"].as_str().unwrap(),
            grant["grantId"].as_str().unwrap(),
            grant["rootDirectory"].as_str().unwrap(),
            grant["mode"].as_str().unwrap(),
            grant["expiresAt"].as_u64().unwrap(),
        );
        assert_eq!(
            signature,
            grant["signature"].as_str().unwrap(),
            "grant {}",
            grant["grantId"]
        );
        assert!(verify_signature(
            &signature,
            grant["signature"].as_str().unwrap()
        ));
    }
}

#[test]
fn command_signatures_match() {
    let vectors = vectors();
    for command in vectors["commandSignatures"].as_array().unwrap() {
        let payload = &command["payload"];
        assert_eq!(
            canonical_json(payload).unwrap(),
            command["payloadCanonical"].as_str().unwrap()
        );
        let signature = command_signature(
            command["token"].as_str().unwrap(),
            command["id"].as_str().unwrap(),
            command["type"].as_str().unwrap(),
            command["epochMs"].as_u64().unwrap(),
            command["nonce"].as_str().unwrap(),
            payload,
        )
        .unwrap();
        assert_eq!(
            signature,
            command["signature"].as_str().unwrap(),
            "command {}",
            command["id"]
        );
    }
}

#[test]
fn a_wrong_or_malformed_signature_is_refused() {
    assert!(!verify_signature("abcd", "abce"));
    assert!(!verify_signature("abcd", "abcdef"));
    assert!(!verify_signature("abcd", "not hex"));
}

#[test]
fn the_manifest_and_error_codes_load_and_agree() {
    let manifest = Manifest::get();
    assert_eq!(manifest.operations().len(), 56);
    assert_eq!(manifest.for_platform(Platform::MacOs).count(), 56);
    assert_eq!(manifest.for_platform(Platform::Windows).count(), 40);
    let open = manifest.operation("openURL").unwrap();
    assert!(!open.is_destructive);
    assert!(manifest.supports("getSystemStatus", Platform::Windows));
    assert!(!manifest.supports("sendMessage", Platform::Windows));
    assert!(manifest.operation("doesNotExist").is_none());

    let codes = ErrorCodes::get();
    assert!(codes.len() > 50);
    assert!(codes.class_of("ACCESS_DENIED").unwrap().blocked);
    assert!(codes.class_of("NOT_A_CODE").is_none());
}
