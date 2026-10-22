//! The client core on the Rust side of the shell.
//!
//! `teamcraft-core` is async Rust with a SQLCipher store and a gateway WebSocket, none of which a
//! webview can host, and the access token must stay out of the webview. So the core runs here, on
//! a thread of its own with a single-threaded runtime (its backend types are not `Send`), and the
//! UI talks to it through the commands in [`commands`]: a snapshot of what the sidebar shows, windows
//! of a conversation's feed, and actions. Whenever the store changes the host emits
//! [`CORE_CHANGED_EVENT`](teamcraft_client_api::remote::CORE_CHANGED_EVENT) and the UI re-reads.

pub mod commands;
mod runtime;

pub use runtime::{CallPreparation, TokenCache};

#[cfg(all(test, feature = "integration"))]
mod dev_bridge;
#[cfg(all(test, feature = "integration"))]
mod fake_calls;
#[cfg(all(test, feature = "integration"))]
mod integration_tests;
