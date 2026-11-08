//! The computer itself: status and ping (`status`), power, volume, brightness, clipboard and its history, media and opening an address (`controls`), Wiqo's Shortcuts (`shortcuts`), and the one-sentence system chores (`utility`).

mod controls;
mod shortcuts;
mod status;
mod utility;

use crate::Op;

pub(crate) fn operations() -> Vec<Op> {
    [
        status::operations(),
        controls::operations(),
        shortcuts::operations(),
        utility::operations(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

pub use controls::start_clipboard_history;
