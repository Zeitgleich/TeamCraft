//! All integration tests of the crate in one test binary: one link step instead of one per file.
//! Each module keeps its own `cfg` gates (features, target), so a module that does not apply
//! here is empty.

mod account_data;
mod activity;
mod backgrounds;
mod calls;
mod chat_actions;
#[path = "../common/mod.rs"]
mod common;
mod connection;
mod dev_stack;
mod devices;
mod enrollment;
mod files;
mod key_packages;
mod link_previews;
mod linking;
mod member_devices;
mod messages;
mod mls_recovery;
mod org_backgrounds;
mod org_recovery;
mod push;
mod push_guard;
mod queries;
mod runner;
mod search;
mod sync_engine;
mod workspace;
mod workspace_admin;
