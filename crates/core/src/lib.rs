//! The fragment cell's pure logic. Everything here is deterministic given
//! its inputs (randomness and the clock come from the caller), so the cell
//! runs it as wasm and the host tests it.

pub mod access;
pub mod codestorage;
pub mod manifest;
pub mod npub;
pub mod ratelimit;
pub mod schema;
pub mod secrets;
pub mod site;
pub mod webhook;
