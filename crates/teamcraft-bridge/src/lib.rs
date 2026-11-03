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
//! - [`batch`], [`limiter`]: collecting lines into messages, rate limits.
//! - [`sources`]: the sources the endpoints accept: announced in the app, or in the file.
//! - [`http`]: the endpoints; [`idempotency`]: retries with an `Idempotency-Key` are answered once;
//!   [`ingest`]: log streams are read line by line as they arrive.
//! - [`multipart`], [`form`], [`spool`], [`file_hooks`], [`file_jobs`]: webhooks with files, from
//!   the form as it arrives to the encrypted uploads.
//! - [`worker`]: the task that owns the client (gateway, sending, uploads, index segments).
//! - [`app`]: wiring for the binary.

pub mod app;
pub mod batch;
pub mod config;
pub mod file_hooks;
pub mod file_jobs;
pub mod form;
pub mod http;
pub mod idempotency;
pub mod ingest;
pub mod limiter;
pub mod multipart;
pub mod render;
pub mod sources;
pub mod spool;
pub mod worker;
