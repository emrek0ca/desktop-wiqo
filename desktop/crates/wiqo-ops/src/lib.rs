//! The operations of a Wiqo desktop. One module per area; the platform differences stay inside the module, never in the caller.

pub mod activity;
mod applescript;
mod apps;
mod browser;
mod desktop_control;
mod files;
mod fsx;
mod ocr;
mod op;
mod personal;
mod process;
mod screen;
pub mod recorder;
pub mod screen_memory;
mod system;
pub mod terminal;

use std::sync::Arc;

use wiqo_core::engine::Engine;

/// Masks tokens and keys in text that leaves the computer (terminal output, a diagnostics bundle).
pub use desktop_control::{set_cursor_sink, stop_all as stop_desktop_control, CursorEvent};
/// Takes back a file change by its undo id (terminal sessions call this for the phone's "Geri al").
pub use files::undo::undo_file;
/// What a batch of file changes would do, for the question before the person's yes ("Önce prova").
pub use files::batch_preview;
/// A signed-in page read for a watcher (the server compares it).
pub use browser::read_watched_page;
pub use terminal::redact_secrets;

/// Every operation this build implements, one module per area. `sessions` are the terminal sessions the `terminal` step runs in.
fn all_operations(sessions: std::sync::Arc<terminal::Sessions>) -> Vec<Op> {
    [
        system::operations(),
        apps::operations(),
        files::operations(),
        screen::operations(),
        personal::operations(),
        terminal::code::operations(),
        vec![terminal::step::operation(sessions)],
        desktop_control::operations(),
        browser::operations(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Registers every operation this build implements. The engine only accepts names `operations.json` lists for the platform it runs on.
pub fn register_all(engine: &mut Engine) -> Result<(), String> {
    engine.set_application_targets(apps::targets);
    let sessions = terminal::Sessions::new();
    engine.set_terminal(sessions.clone());
    if engine.platform() == wiqo_contract::Platform::MacOs {
        system::start_clipboard_history();
    }
    // The full application list (Store apps too) is read once in the background, so the first "open Spotify" finds it.
    #[cfg(windows)]
    apps::warm();
    // Some operations are listed for macOS only in operations.json (the server never sends them elsewhere yet), so they are left out on other systems.
    let platform = engine.platform();
    all_operations(sessions)
        .into_iter()
        .filter(|operation| wiqo_contract::Manifest::get().supports(operation.name(), platform))
        .try_for_each(|operation| engine.register(operation))
}

/// Names of the operations this build implements (for reports and tests).
pub fn implemented() -> Vec<&'static str> {
    all_operations(std::sync::Arc::new(terminal::Sessions::default()))
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

    #[test]
    fn every_mac_operation_is_implemented() {
        let implemented: BTreeSet<&str> = implemented().into_iter().collect();
        let mac: BTreeSet<&str> = Manifest::get()
            .for_platform(Platform::MacOs)
            .map(|operation| operation.operation.as_str())
            .collect();
        let missing: Vec<&str> = mac.difference(&implemented).copied().collect();
        assert!(missing.is_empty(), "not implemented: {missing:?}");
    }

    #[test]
    fn every_shared_operation_is_implemented() {
        let implemented: BTreeSet<&str> = implemented().into_iter().collect();
        let shared: BTreeSet<&str> = Manifest::get()
            .for_platform(Platform::Windows)
            .map(|operation| operation.operation.as_str())
            .collect();
        let missing: Vec<&str> = shared.difference(&implemented).copied().collect();
        assert!(missing.is_empty(), "not implemented: {missing:?}");
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

    /// A destructive operation in the contract (one the phone asks about) must never run in the read-only lane, where several run at once and a
    /// stopped task does not wait for it; and every implemented operation is one the contract lists.
    #[test]
    fn every_lane_agrees_with_the_contract() {
        for operation in all_operations(std::sync::Arc::new(terminal::Sessions::default())) {
            let entry = Manifest::get()
                .operation(operation.name())
                .unwrap_or_else(|| panic!("{} is not in the contract", operation.name()));
            if entry.is_destructive {
                assert!(
                    matches!(operation.lane(), wiqo_core::engine::Lane::SideEffect),
                    "{} is destructive in the contract but runs in the read-only lane",
                    operation.name()
                );
            }
        }
    }
}
