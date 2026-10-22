//! Client logic shared by desktop, web and mobile (ARCHITECTURE.md §7–§8): the encrypted local
//! store, the server API, the sync engine and first-run enrollment. UIs read the local store through
//! adapters; network and decryption happen below them.

pub mod api;
pub mod attestation;
pub mod client;
pub mod clock;
pub mod error;
pub mod platform;
pub mod push_preview;
pub mod read_state;
pub mod records;
pub mod runner;
#[cfg(not(target_arch = "wasm32"))]
pub mod search;
pub mod store;
#[cfg(feature = "ui")]
pub mod ui_backend;
#[cfg(feature = "ui")]
mod ui_search;
#[cfg(target_arch = "wasm32")]
pub mod web_push;
pub mod workspace;

mod convert;
mod trust;

pub use client::{
  CallEventSink, CallKeySigner, Client, ClientSettings, EnrollmentOptions, FlushReport, GatewayOutcome,
  LinkOffer, OrgRecoveryUnlock, PendingEnrollment,
};
pub use error::CoreError;
