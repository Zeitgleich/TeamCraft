//! Client logic shared by desktop, web and mobile (ARCHITECTURE.md §7–§8): the encrypted local
//! store, the server API, the sync engine and first-run enrollment. UIs read the local store through
//! adapters; network and decryption happen below them.

pub mod account_data;
pub mod anchoring;
pub mod api;
pub mod asset_ledger;
pub mod attestation;
pub mod call_answer;
pub mod call_group_wait;
pub mod canvas;
pub mod canvas_presence;
pub mod channel_roles;
#[cfg(feature = "ui")]
pub mod chat_list_pass;
pub mod client;
pub mod clock;
// The browser can't read other sites' pages (e2ee.md §12): no composer preview there.
#[cfg(all(feature = "ui", not(target_arch = "wasm32")))]
pub mod composer_preview;
pub mod connection;
pub mod custom_emoji;
pub mod device_activity;
pub mod downloads;
#[cfg(feature = "ui")]
pub(crate) mod entry_tally;
pub mod error;
#[cfg(feature = "ui")]
pub mod export;
pub mod image_header;
pub mod imported;
pub mod integration_sources;
pub mod link_preview;
pub mod meeting_invitees;
pub mod meeting_lobby;
pub mod meeting_standing;
pub mod signed_deletion;
pub mod space_dms;
pub mod text_preview;
// The keys come from the UI contract (client-api), which the hosts of a UI build in; the service
// worker reads pictures for its notifications.
/// Anonymous polls' ballots and boxes (issue #2121).
pub mod anonymous_poll;
pub mod group_mentions;
#[cfg(any(feature = "ui", feature = "service-worker"))]
pub mod media_cache;
pub mod media_stream;
pub mod mls_storage;
pub mod motion_photo;
pub mod notetaker;
pub mod notified;
pub mod notify_rules;
pub mod pending_work;
pub mod people_names;
pub mod picked_files;
pub mod platform;
/// Counting polls into the UI's view (issue #205).
#[cfg(feature = "ui")]
pub mod polls;
pub mod prefetch;
pub mod provider_account_names;
pub mod push_guard;
#[cfg(any(feature = "ui", feature = "service-worker"))]
pub mod push_picture;
pub mod push_preview;
pub mod push_probe;
pub mod push_subscription;
pub mod push_text;
pub mod read_state;
pub mod readable_text;
pub mod records;
pub mod retry;
pub mod role_signing;
pub mod row_badge;
pub mod runner;
#[cfg(not(target_arch = "wasm32"))]
pub mod search;
#[cfg(not(target_arch = "wasm32"))]
pub mod search_index;
pub mod search_scope;
pub mod stickers;
pub mod store;
#[cfg(not(target_arch = "wasm32"))]
pub mod svg_thumbnail;
pub mod thumbnail;
pub mod timing;
pub mod transfers;
pub mod typing;
#[cfg(feature = "ui")]
mod ui_actions;
#[cfg(feature = "ui")]
pub mod ui_activity;
#[cfg(feature = "ui")]
pub mod ui_backend;
#[cfg(feature = "ui")]
mod ui_draft_files;
#[cfg(feature = "ui")]
mod ui_drafts;
#[cfg(feature = "ui")]
mod ui_files;
#[cfg(feature = "ui")]
mod ui_files_page;
#[cfg(feature = "ui")]
pub mod ui_join_requests;
#[cfg(feature = "ui")]
mod ui_links;
#[cfg(feature = "ui")]
mod ui_people;
#[cfg(feature = "ui")]
mod ui_search;
#[cfg(feature = "ui")]
pub mod ui_snapshot;
#[cfg(feature = "ui")]
mod ui_strict;
#[cfg(feature = "ui")]
mod ui_workspace;
pub mod unseen_reactions;
pub mod upkeep;
#[cfg(target_arch = "wasm32")]
pub mod web_push;
pub mod workspace;

mod convert;
mod trust;

pub use canvas_presence::CanvasPresenceState;
pub use client::{
  AttestationSettings, CallEventSink, CallKeySigner, CallPreparation, Client, ClientSettings,
  DirectMediaInputs, EnrollmentOptions, EnrollmentSecret, FlushReport, GatewayOutcome, GroupAccess,
  HistoryReading, HistoryScan, JwksSource, LinkAnnouncement, LinkCodeKind, LinkOffer, LinkRequestInfo,
  LinkRequestList, MAX_VIEWER_BYTES, NewBackupSecret, OrgRecoveryUnlock, PendingEnrollment, SyncPhase,
  parse_recovery_key,
};
pub use error::{CoreError, JoinRequestRefusal, UnreadableStore};
pub use typing::TypingState;
