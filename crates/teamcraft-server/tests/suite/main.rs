//! All integration tests of the crate in one test binary: one link step instead of one per file.
//! Each module keeps its own `cfg` gates (features, target), so a module that does not apply
//! here is empty.

#[path = "../common/mod.rs"]
mod common;

mod admin;
mod admin_extras;
mod admin_settings;
mod attachments;
mod authz;
mod backgrounds;
mod bots;
mod brand;
mod brand_shares;
mod caldav_calcraft;
mod call_e2e;
mod call_features;
mod call_roster;
mod calls;
mod conversations;
mod device_gaps;
mod first_sight;
mod gateway;
mod guests;
mod hardening;
mod health;
mod identity;
mod integration_channels;
mod key_bag;
mod key_packages;
mod last_seen;
mod local_accounts;
mod log_hygiene;
mod meeting_guests;
mod meeting_mail;
mod meetings;
mod messages;
mod mls;
mod notification_settings;
mod oidc;
mod org_backgrounds;
mod org_recovery;
mod organizations;
mod perf_gateway;
mod perf_history;
mod presence_derived;
mod profiles;
mod push;
mod push_gateway;
mod push_providers;
mod push_retry;
mod push_sending;
mod read_receipts;
mod rendezvous;
mod rest;
mod retention;
mod security;
mod session_policy;
mod shares;
mod space_features;
mod status;
mod threads;
mod unread_events;
mod user_groups;
mod web;
mod workspace;
