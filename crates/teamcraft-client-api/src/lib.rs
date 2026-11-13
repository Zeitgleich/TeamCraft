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
pub mod app_link;
pub mod backend;
pub mod brand;
pub mod bug_report;
pub mod call_view;
pub mod call_window;
pub mod calls;
pub mod canvas;
pub mod color_theme;
pub mod composer_format;
pub mod crash_reporting;
pub mod custom_emoji;
pub mod custom_status;
pub mod data_export;
pub mod debug_log;
pub mod drafts;
pub mod emoji;
pub mod emoji_catalog;
pub mod entry_rules;
pub mod failure;
pub mod files_query;
pub mod font;
pub mod join_request_alerts;
pub mod links;
pub mod list_load;
pub mod live;
pub mod log;
pub mod markdown;
pub mod media;
pub mod media_viewer;
pub mod meeting_edits;
pub mod meeting_hosts;
pub mod meeting_invitees;
pub mod message_sound;
pub mod mobile_settings;
pub mod model;
pub mod notetaker;
pub mod notify;
pub mod oidc_login;
pub mod os_notify;
pub mod page_calls;
pub mod picture_edit;
pub mod polls;
pub mod privacy;
pub mod qr;
pub mod receipts;
pub mod recent_activity;
pub mod recovery_kit;
pub mod remote;
pub mod scheduled;
/// Shared with the core, which verifies signed channel roles with the same rules (issue #380).
pub use teamcraft_proto::role_rules;
pub mod role_review;
pub mod search;
pub mod security;
pub mod server_entry;
pub mod settings_sync;
pub mod sign_in_look;
pub mod sign_in_page;
pub mod snapshot_changes;
pub mod space_scope;
pub mod tray_menu;
pub mod updates;
pub mod upload_pill;
pub mod video_playback;
pub mod video_stats;
pub mod voice;
pub mod window_chrome;
