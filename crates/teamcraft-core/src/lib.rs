//! Client logic shared by desktop, web and mobile (ARCHITECTURE.md §7–§8): the encrypted local
//! store, the server API, the sync engine and first-run enrollment. UIs read the local store through
//! adapters; network and decryption happen below them.

pub mod api;
pub mod attestation;
pub mod client;
pub mod clock;
pub mod error;
pub mod records;
pub mod store;
#[cfg(feature = "ui")]
pub mod ui_backend;

mod convert;
mod trust;

pub use client::{Client, ClientSettings, EnrollmentOptions, FlushReport, GatewayOutcome, PendingEnrollment};
pub use error::CoreError;
