//! `teamcraft-import`: brings the history of another chat system (Synology Chat) into TeamCraft as
//! a client, end-to-end encrypted (e2ee.md §4.10, admin.md §3.8, docs/import-synology.md).

pub mod app;
pub mod args;
pub mod auth;
pub mod convert;
pub mod export;
pub mod files;
pub mod mapping;
pub mod plan;
pub mod progress;
pub mod report;
pub mod run;
