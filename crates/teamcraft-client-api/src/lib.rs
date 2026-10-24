//! The contract between the TeamCraft UI (`teamcraft-ui`) and the client core (`teamcraft-core`).
//!
//! Backend traits ([`backend`]) and the view models they exchange ([`model`], [`admin`], [`calls`],
//! [`security`], [`search`]) live here so neither side depends on the other. The traits model the
//! local, already decrypted store of the client: reads are synchronous, anything that needs the
//! network or decryption happens below them and shows up as a store update. No UI framework, no
//! I/O; builds for the browser.

pub mod actions;
pub mod activity;
pub mod admin;
pub mod backend;
pub mod brand;
pub mod calls;
pub mod emoji;
pub mod emoji_catalog;
pub mod failure;
pub mod legacy_session;
pub mod links;
pub mod live;
pub mod log;
pub mod model;
pub mod notify;
pub mod os_notify;
pub mod privacy;
pub mod remote;
pub mod search;
pub mod security;
pub mod server_entry;
pub mod sign_in_look;
pub mod sign_in_page;
pub mod updates;
pub mod window_chrome;
