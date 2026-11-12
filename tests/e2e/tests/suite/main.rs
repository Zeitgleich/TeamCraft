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
#[cfg(feature = "browser")]
mod admin_console_requests;
mod bridge;
#[cfg(feature = "browser")]
mod cached_start;
mod calls;
mod client_outages;
mod conversion;
mod demo_signin;
mod dependency_outages;
mod deployed;
#[cfg(feature = "desktop")]
mod desktop_perf;
mod devices;
mod display_names;
#[cfg(feature = "browser")]
mod dpop_web;
mod email_digest;
mod guest_domains;
mod guests;
mod long_absence;
mod meetings;
#[cfg(feature = "browser")]
mod meetings_firefox;
mod membership;
mod messaging;
mod mfa;
mod mobile_workspace;
mod moderator;
mod network_conditions;
#[cfg(feature = "browser")]
mod pending_echo;
#[cfg(feature = "browser")]
mod perf;
mod retention_limits;
#[cfg(feature = "browser")]
mod role_signing_web;
mod server_robustness;
#[cfg(feature = "browser")]
mod sw_update;
mod sync_perf;
mod threads_policy;
#[cfg(feature = "browser")]
mod web;
#[cfg(feature = "browser")]
mod web_calls;
#[cfg(feature = "browser")]
mod web_directory;
#[cfg(feature = "browser")]
mod web_feed_stability;
#[cfg(feature = "browser")]
mod web_mobile;
#[cfg(feature = "browser")]
mod web_new_device;
#[cfg(feature = "browser")]
mod web_scroll_older;
#[cfg(feature = "browser")]
mod web_single_image;
#[cfg(feature = "browser")]
mod web_split;
