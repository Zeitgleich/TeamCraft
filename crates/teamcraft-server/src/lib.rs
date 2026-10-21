//! TeamCraft server: REST API, authentication and (later) the gateway.
//!
//! The binary in `main.rs` only wires configuration, logging and the listener together.

pub mod auth;
pub mod config;
pub mod error;
pub mod handlers;
pub mod listener;
pub mod logging;
pub mod router;
pub mod state;
