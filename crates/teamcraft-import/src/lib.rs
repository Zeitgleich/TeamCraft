//! `teamcraft-import`: brings the history of another chat system (Synology Chat, Slack, Mattermost)
//! into TeamCraft as a client, end-to-end encrypted (e2ee.md §4.10, admin.md §3.8,
//! docs/import-synology.md, docs/import-slack.md, docs/import-mattermost.md).

pub mod app;
pub mod args;
pub mod auth;
pub mod convert;
pub mod export;
pub mod files;
pub mod mapping;
pub mod plan;
pub mod progress;
pub mod proof_key;
pub mod report;
pub mod run;
pub mod sources;
