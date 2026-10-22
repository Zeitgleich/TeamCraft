//! The end-to-end suite (README.md). One test binary: every test file links the whole client stack,
//! which is large. Each test starts its own server (and SFU, bridge) with a fresh database.
#![cfg(feature = "e2e")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

mod accounts;
mod admin;
mod bridge;
mod calls;
mod conversion;
mod devices;
mod guest_domains;
mod guests;
mod meetings;
mod membership;
mod messaging;
mod moderator;
mod threads_policy;
#[cfg(feature = "browser")]
mod web;
