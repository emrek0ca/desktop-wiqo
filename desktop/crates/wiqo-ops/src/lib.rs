//! The operations of a Wiqo desktop. One module per area; the platform differences stay inside the module, never in the caller.

mod agent;
mod apps;
mod chores;
mod convert;
mod device;
mod documents;
mod files;
mod fsx;
mod op;
mod personal;
mod plan;
mod process;
mod screen;
mod search;
mod system;
pub mod terminal;
mod transfer;
mod undo;
mod utility;

use std::sync::Arc;

use wiqo_core::engine::Engine;

/// Masks tokens and keys in text that leaves the computer (terminal output, a diagnostics bundle).
pub use terminal::redact_secrets;
/// Takes back a file change by its undo id (terminal sessions call this for the phone's "Geri al").
pub use undo::undo_file;

/// Every operation this build implements, one module per area.
fn all_operations() -> Vec<Op> {
    [
        system::operations(),
        apps::operations(),
        agent::operations(),
        device::operations(),
        files::operations(),
        documents::operations(),
        search::operations(),
        screen::operations(),
        transfer::operations(),
        utility::operations(),
        convert::operations(),
        plan::operations(),
        chores::operations(),
        personal::operations(),
        terminal::code::operations(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Registers every operation this build implements. The engine only accepts names `operations.json` lists for the platform it runs on.
pub fn register_all(engine: &mut Engine) -> Result<(), String> {
    engine.set_application_targets(apps::targets);
    engine.set_terminal(terminal::Sessions::new());
    if engine.platform() == wiqo_contract::Platform::MacOs {
        personal::start_clipboard_history();
    }
    // Some operations are listed for macOS only in operations.json (the server never sends them elsewhere yet), so they are left out on other systems.
    let platform = engine.platform();
    all_operations()
        .into_iter()
        .filter(|operation| wiqo_contract::Manifest::get().supports(operation.name(), platform))
        .try_for_each(|operation| engine.register(operation))
}

/// Names of the operations this build implements (for reports and tests).
pub fn implemented() -> Vec<&'static str> {
    all_operations()
        .iter()
        .map(|operation| operation.name())
        .collect()
}

pub(crate) type Op = Arc<dyn wiqo_core::engine::Operation>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use wiqo_contract::{Manifest, Platform};

    /// The operations R3 does not cover yet; the test below fails when this list and the code drift apart.
    const NOT_YET: [&str; 4] = [
        "computerDesktop",
        "computerBrowser",
        "endBrowserTask",
        "localAnalyze",
    ];

    /// Mac-only operations left out on purpose: driving another app's controls and clicking the screen stay with the old Mac app's careful
    /// accessibility layer until they are ported with the same checks.
    const MAC_NOT_YET: [&str; 2] = ["controlUIElement", "clickScreen"];

    #[test]
    fn every_mac_operation_is_implemented_except_the_listed_ones() {
        let implemented: BTreeSet<&str> = implemented().into_iter().collect();
        let mac: BTreeSet<&str> = Manifest::get()
            .for_platform(Platform::MacOs)
            .map(|operation| operation.operation.as_str())
            .collect();
        let missing: BTreeSet<&str> = mac.difference(&implemented).copied().collect();
        let expected: BTreeSet<&str> = NOT_YET.into_iter().chain(MAC_NOT_YET).collect();
        assert_eq!(missing, expected);
    }

    #[test]
    fn every_shared_operation_is_implemented_except_the_listed_ones() {
        let implemented: BTreeSet<&str> = implemented().into_iter().collect();
        let shared: BTreeSet<&str> = Manifest::get()
            .for_platform(Platform::Windows)
            .map(|operation| operation.operation.as_str())
            .collect();
        let missing: BTreeSet<&str> = shared.difference(&implemented).copied().collect();
        assert_eq!(missing, NOT_YET.into_iter().collect::<BTreeSet<_>>());
        assert!(
            implemented
                .difference(
                    &Manifest::get()
                        .for_platform(Platform::MacOs)
                        .map(|operation| operation.operation.as_str())
                        .collect()
                )
                .next()
                .is_none(),
            "an implemented operation the manifest does not list"
        );
    }
}
