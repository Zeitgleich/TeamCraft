//! TeamCraft server: REST API, authentication and the realtime gateway.
//!
//! The binary in `main.rs` only wires configuration, logging and the listener together.

pub mod auth;
pub mod config;
pub mod error;
pub mod extract;
pub mod gateway;
pub mod handlers;
pub mod listener;
pub mod logging;
pub mod meetings;
pub mod openapi;
pub mod push;
pub mod rendezvous;
pub mod router;
pub mod services;
pub mod state;
pub mod storage;
#[cfg(feature = "integration")]
pub mod testing;
