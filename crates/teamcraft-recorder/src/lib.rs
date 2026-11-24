//! The TeamCraft recorder bot (e2ee.md §8.3, admin.md §3.13, #2273): a headless participant that
//! records the calls of the conversations it is a member of, unattended, and posts each recording,
//! encrypted, into the call's conversation.
//!
//! It is a bot account with a device of its own, like the notetaker: a member of the conversations
//! whose calls must be recorded, an ordinary participant of their calls with its own DAVE keys. It
//! records with the pipeline every app uses and announces it with the same signed notice, so
//! everybody in the call sees "Recording" with the bot's name. It records only while the
//! owner-signed policy names it; under `recorder_bot_only` nobody else may.
//!
//! - [`config`]: the TOML file.
//! - [`session`]: one call: join, record, hand over takes, leave.
//! - [`worker`]: the client, the policy, the calls, posting.
//! - [`disk`]: the recordings' free space and what a crash left.
//! - [`texts`]: what gets posted.
//! - [`app`]: wiring for the binary.

pub mod app;
pub mod config;
pub mod disk;
pub mod session;
pub mod texts;
pub mod worker;
