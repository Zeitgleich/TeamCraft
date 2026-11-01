//! All integration tests of the crate in one test binary: one link step instead of one per file.
//! Each module keeps its own `cfg` gates (features, target), so a module that does not apply
//! here is empty.
// Every module runs on tokio against the fake server; the wasm tests are web.rs and
// web_service_worker.rs.
#![cfg(not(target_arch = "wasm32"))]

mod abandoned_setup;
mod account_data;
mod activity;
mod admin_add_policy;
mod admin_joins;
mod backgrounds;
mod calls;
mod catch_up;
mod channel_roles;
mod chat_actions;
#[path = "../common/mod.rs"]
mod common;
mod connection;
mod custom_status;
mod dev_stack;
mod devices;
mod direct_uploads;
mod dm_setup;
mod enrollment;
mod files;
mod first_add_back;
mod guest_meetings;
mod import;
mod key_packages;
mod link_previews;
mod linking;
mod live_permissions;
mod meeting_chats;
mod member_devices;
mod member_removal;
mod messages;
mod mls_recovery;
mod mls_storage;
mod notes_refound;
mod notetaker;
mod offline_actions;
mod org_backgrounds;
mod org_recovery;
mod people_directory;
mod push;
mod push_guard;
mod queries;
mod recovery_kit;
mod removal_followups;
mod runner;
mod search;
mod sending;
mod server_changes;
mod snapshot_cache;
mod store_integrity;
mod sync_engine;
mod sync_heads;
mod unseen_reactions;
mod upkeep;
mod upload_parts;
mod workspace;
mod workspace_admin;
