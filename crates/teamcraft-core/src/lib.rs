//! Client logic shared by desktop, web and mobile (ARCHITECTURE.md §7–§8): the encrypted local
//! store, the server API, the sync engine and first-run enrollment. UIs read the local store through
//! adapters; network and decryption happen below them.

pub mod account_data;
pub mod api;
pub mod attestation;
pub mod channel_roles;
pub mod client;
pub mod clock;
pub mod connection;
pub mod device_activity;
pub mod downloads;
pub mod error;
pub mod integration_sources;
pub mod link_preview;
// The keys come from the UI contract (client-api), which the hosts of a UI build in; the service
// worker reads pictures for its notifications.
#[cfg(any(feature = "ui", feature = "service-worker"))]
pub mod media_cache;
pub mod mls_storage;
pub mod notify_rules;
pub mod picked_files;
pub mod platform;
pub mod prefetch;
pub mod push_guard;
#[cfg(any(feature = "ui", feature = "service-worker"))]
pub mod push_picture;
pub mod push_preview;
pub mod push_subscription;
pub mod push_text;
pub mod read_state;
pub mod records;
pub mod retry;
pub mod runner;
#[cfg(not(target_arch = "wasm32"))]
pub mod search;
pub mod store;
#[cfg(not(target_arch = "wasm32"))]
pub mod svg_thumbnail;
pub mod thumbnail;
pub mod transfers;
pub mod typing;
#[cfg(feature = "ui")]
mod ui_actions;
#[cfg(feature = "ui")]
pub mod ui_activity;
#[cfg(feature = "ui")]
pub mod ui_backend;
#[cfg(feature = "ui")]
mod ui_files;
#[cfg(feature = "ui")]
pub mod ui_join_requests;
#[cfg(feature = "ui")]
mod ui_people;
#[cfg(feature = "ui")]
mod ui_search;
#[cfg(feature = "ui")]
pub mod ui_snapshot;
#[cfg(feature = "ui")]
mod ui_workspace;
pub mod upkeep;
#[cfg(target_arch = "wasm32")]
pub mod web_push;
pub mod workspace;

mod convert;
mod trust;

pub use client::{
  AttestationSettings, CallEventSink, CallKeySigner, CallPreparation, Client, ClientSettings,
  EnrollmentOptions, EnrollmentSecret, FlushReport, GatewayOutcome, GroupAccess, HistoryScan, JwksSource,
  LinkAnnouncement, LinkOffer, LinkRequestInfo, LinkRequestList, MAX_VIEWER_BYTES, OrgRecoveryUnlock,
  PendingEnrollment, SyncPhase, parse_recovery_key,
};
pub use error::{CoreError, JoinRequestRefusal, UnreadableStore};
pub use typing::TypingState;
