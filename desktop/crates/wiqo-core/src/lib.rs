//! The Wiqo desktop runtime, without any operation in it.
//!
//! What a phone or the server sends arrives through [`relay`], is checked ([`verify`]), is run at most once ([`ledger`]) inside the right lane and time
//! budget ([`engine`]), and the answer goes back in the shape the contract defines ([`wire`]). The operations themselves live in `wiqo-ops`.

pub mod api;
pub mod codes;
pub mod engine;
pub mod grants;
pub mod ledger;
pub mod relay;
pub mod storage;
pub mod terminal;
pub mod vault;
pub mod verify;
pub mod wire;
