//! The end-to-end suite (README.md). One test binary: every test file links the whole client stack,
//! which is large. Each test starts its own server (and SFU, bridge) with a fresh database.
#![cfg(feature = "e2e")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

mod acceptance_activity;
mod acceptance_calls;
mod acceptance_files;
#[cfg(feature = "browser")]
mod acceptance_meetings_web;
mod acceptance_notifications;
mod acceptance_people;
mod acceptance_presence;
mod acceptance_search;
mod acceptance_space_invites;
mod acceptance_spaces;
mod acceptance_threads;
mod accounts;
mod admin;
mod bridge;
mod calls;
mod conversion;
mod dependency_outages;
mod deployed;
mod devices;
mod display_names;
mod email_digest;
mod guest_domains;
mod guests;
mod long_absence;
mod meetings;
mod membership;
mod messaging;
mod mfa;
mod moderator;
mod network_conditions;
#[cfg(feature = "browser")]
mod perf;
mod retention_limits;
mod server_robustness;
mod sync_perf;
mod threads_policy;
#[cfg(feature = "browser")]
mod web;
#[cfg(feature = "browser")]
mod web_calls;
#[cfg(feature = "browser")]
mod web_mobile;
#[cfg(feature = "browser")]
mod web_split;
