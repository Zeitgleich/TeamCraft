//! Device-linking relay: forwards opaque frames between two devices of one user (e2ee.md §5).

pub mod connection;
mod hub;

pub use hub::{JoinError, Joined, RelayError, RendezvousHub, Signal};
