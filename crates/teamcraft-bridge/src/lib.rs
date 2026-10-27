//! The TeamCraft integration bridge (ARCHITECTURE.md §4.5): a headless client that turns webhooks
//! and log streams into end-to-end encrypted messages.
//!
//! It runs where the data comes from, never on the VPS. It is a bot device (see `POST
//! /api/v1/admin/bots`) and member of the channels it serves, so it holds their content keys and
//! encrypts like every other client; the server only sees ciphertext. Tools talk to a small local
//! HTTP endpoint (Slack/Mattermost-compatible webhooks, plain text and NDJSON logs); the bridge
//! batches, rate-limits, mentions people on alerts and builds the search index segments of its
//! channels, since it already has the plaintext.
//!
//! - [`config`]: the TOML file.
//! - [`render`]: webhook payloads and log lines to message text.
//! - [`batch`], [`limiter`]: collecting lines into messages, per-source rate limits.
//! - [`sources`]: the sources the endpoints accept: announced in the app, or in the file.
//! - [`http`]: the endpoints.
//! - [`worker`]: the task that owns the client (gateway, sending, index segments).
//! - [`app`]: wiring for the binary.

pub mod app;
pub mod batch;
pub mod config;
pub mod http;
pub mod limiter;
pub mod render;
pub mod sources;
pub mod worker;
