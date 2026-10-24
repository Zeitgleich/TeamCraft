//! All integration tests of the crate in one test binary: one link step instead of one per file.
//! Each module keeps its own `cfg` gates (features, target), so a module that does not apply
//! here is empty.
// Every module runs on tokio against the fake server; the wasm tests are web.rs and
// web_service_worker.rs.
#![cfg(not(target_arch = "wasm32"))]

mod account_data;
mod activity;
mod backgrounds;
mod calls;
mod catch_up;
mod chat_actions;
#[path = "../common/mod.rs"]
mod common;
mod connection;
mod dev_stack;
mod devices;
mod dm_setup;
mod enrollment;
mod files;
mod key_packages;
mod link_previews;
mod linking;
mod member_devices;
mod member_removal;
mod messages;
mod mls_recovery;
mod offline_actions;
mod org_backgrounds;
mod org_recovery;
mod people_directory;
mod push;
mod push_guard;
mod queries;
mod removal_followups;
mod runner;
mod search;
mod snapshot_cache;
mod sync_engine;
mod upkeep;
mod workspace;
mod workspace_admin;
