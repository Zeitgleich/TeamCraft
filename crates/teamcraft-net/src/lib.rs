//! Network helpers shared by the clients and the server.
//!
//! - [`retry`]: the one retry policy (capped exponential backoff with jitter, honouring
//!   `Retry-After`) every retry loop uses, on every target including the browser.
//! - Happy Eyeballs (RFC 8305, [`race`]) for the native clients.
//! - [`traffic`]: marking a voice connection's packets for the queues on the way (DSCP).

pub mod retry;

#[cfg(not(target_arch = "wasm32"))]
mod race;

#[cfg(not(target_arch = "wasm32"))]
pub mod traffic;

#[cfg(not(target_arch = "wasm32"))]
pub use race::{ATTEMPT_DELAY, RaceError, interleave, race};
