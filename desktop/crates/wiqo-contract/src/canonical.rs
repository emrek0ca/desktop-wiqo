use serde_json::Value;

#[derive(Debug)]
pub struct CanonicalError(pub String);

impl std::fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "canonical JSON: {}", self.0)
    }
}

impl std::error::Error for CanonicalError {}

/// RFC 8785 (JSON Canonicalization Scheme): sorted keys, ECMAScript number and string forms. Every signature in the contract is made over this text.
pub fn canonical_json(value: &Value) -> Result<String, CanonicalError> {
    serde_json_canonicalizer::to_string(value).map_err(|error| CanonicalError(error.to_string()))
}
