//! The failure codes this runtime reports. Every one is listed in `contracts/companion/error-codes.json` (the test below fails when one is not), because the
//! server decides what to do next from the code, never from the wording.

pub const INVALID_SIGNATURE: &str = "INVALID_SIGNATURE";
pub const SIGNATURE_VERSION_REQUIRED: &str = "SIGNATURE_VERSION_REQUIRED";
pub const COMMAND_TIMEOUT: &str = "COMMAND_TIMEOUT";
pub const UNSUPPORTED_OPERATION: &str = "UNSUPPORTED_OPERATION";
pub const FULL_ACCESS_DISABLED: &str = "FULL_ACCESS_DISABLED";
pub const CAPABILITY_DISABLED: &str = "CAPABILITY_DISABLED";
pub const INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
pub const OUTCOME_UNKNOWN: &str = "OUTCOME_UNKNOWN";
pub const AGENT_SCOPE: &str = "AGENT_SCOPE";
/// The person at the computer refused (or did not answer) a step of the server's agent.
pub const DENIED_ON_PC: &str = "DENIED_ON_PC";

#[cfg(test)]
mod tests {
    use super::*;
    use wiqo_contract::ErrorCodes;

    #[test]
    fn every_code_this_runtime_reports_is_classified_in_the_contract() {
        for code in [
            INVALID_SIGNATURE,
            SIGNATURE_VERSION_REQUIRED,
            COMMAND_TIMEOUT,
            UNSUPPORTED_OPERATION,
            FULL_ACCESS_DISABLED,
            CAPABILITY_DISABLED,
            INVALID_PAYLOAD,
            OUTCOME_UNKNOWN,
            AGENT_SCOPE,
            DENIED_ON_PC,
        ] {
            assert!(
                ErrorCodes::get().class_of(code).is_some(),
                "{code} is not in error-codes.json"
            );
        }
    }
}
