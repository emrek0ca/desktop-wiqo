//! The failure codes this runtime reports. Every one is listed in `contracts/companion/error-codes.json` (the test below fails when one is not), because the
//! server decides what to do next from the code, never from the wording.

pub const INVALID_SIGNATURE: &str = "INVALID_SIGNATURE";
pub const SIGNATURE_VERSION_REQUIRED: &str = "SIGNATURE_VERSION_REQUIRED";
pub const COMMAND_TIMEOUT: &str = "COMMAND_TIMEOUT";
pub const UNSUPPORTED_OPERATION: &str = "UNSUPPORTED_OPERATION";
pub const INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
pub const OUTCOME_UNKNOWN: &str = "OUTCOME_UNKNOWN";
/// The person refused (or did not answer) a step that needed their yes.
pub const DENIED_ON_PC: &str = "DENIED_ON_PC";
/// Nobody answered the question before a step within the time given for it: the step did not run (not the same as a no).
pub const APPROVAL_TIMEOUT: &str = "APPROVAL_TIMEOUT";
/// A read that ran out of its time: it changed nothing, so it may be tried once more (unlike `COMMAND_TIMEOUT`, whose result is unknown).
pub const READ_TIMEOUT: &str = "READ_TIMEOUT";

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
            INVALID_PAYLOAD,
            OUTCOME_UNKNOWN,
            DENIED_ON_PC,
            APPROVAL_TIMEOUT,
            READ_TIMEOUT,
        ] {
            assert!(
                ErrorCodes::get().class_of(code).is_some(),
                "{code} is not in error-codes.json"
            );
        }
    }
}
