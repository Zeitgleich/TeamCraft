//! The end-to-end suite (README.md). One test binary: every test file links the whole client stack,
//! which is large. Each test starts its own server (and SFU, bridge) with a fresh database.
#![cfg(feature = "e2e")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

mod acceptance_calls;
mod acceptance_files;
#[cfg(feature = "browser")]
mod acceptance_meetings_web;
mod acceptance_notifications;
mod acceptance_people;
mod acceptance_presence;
mod acceptance_search;
mod acceptance_spaces;
mod acceptance_threads;
mod accounts;
mod admin;
mod bridge;
mod calls;
mod conversion;
mod deployed;
mod devices;
mod display_names;
mod email_digest;
mod guest_domains;
mod guests;
mod meetings;
mod membership;
mod messaging;
mod mfa;
mod moderator;
mod retention_limits;
mod threads_policy;
#[cfg(feature = "browser")]
mod web;
