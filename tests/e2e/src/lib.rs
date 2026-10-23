//! Harness of the end-to-end tests (README.md): starts the real `teamcraft-server`, `teamcraft-sfu`
//! and `teamcraft-bridge` binaries against the dev stack and drives them with headless
//! `teamcraft-core` clients. Empty without the `e2e` feature.
#![cfg(feature = "e2e")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves, and the
// harness reports skips on stderr.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

pub mod bridge;
#[cfg(feature = "browser")]
pub mod browser;
pub mod call;
pub mod mail;
pub mod peer;
pub mod process;
pub mod resources;
pub mod rest;
pub mod scenario;
pub mod stack;
pub mod tokens;
pub mod totp;

pub use bridge::BridgeProcess;
pub use mail::{Mailpit, ReceivedMail};
pub use peer::{DevClient, Peer};
pub use rest::Reply;
pub use stack::{Stack, StackBuilder};
pub use tokens::UserTokens;
