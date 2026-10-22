//! The contract between the TeamCraft UI (`teamcraft-ui`) and the client core (`teamcraft-core`).
//!
//! Backend traits ([`backend`]) and the view models they exchange ([`model`], [`admin`], [`calls`],
//! [`security`], [`search`]) live here so neither side depends on the other. The traits model the
//! local, already decrypted store of the client: reads are synchronous, anything that needs the
//! network or decryption happens below them and shows up as a store update. No UI framework, no
//! I/O; builds for the browser.

pub mod admin;
pub mod backend;
pub mod calls;
pub mod model;
pub mod remote;
pub mod search;
pub mod security;
