//! The TeamCraft notetaker (e2ee.md §8.2, admin.md §3.6): a headless participant that people ask
//! into a call, which transcribes it through OpenRouter and posts a summary and the transcript into
//! the call's conversation.
//!
//! It is a bot account with a device of its own, like the integration bridge: a member of the
//! conversations it serves, an ordinary participant of their calls with its own DAVE keys. So the
//! call's audio leaves the end-to-end encryption to it, and from it to OpenRouter: that is the
//! feature, and why it is off by default, signed policy, and visible to everybody in the call.
//!
//! - [`config`]: the TOML file.
//! - [`capture`], [`wav`]: decrypted Opus frames to pieces of one speaker's speech.
//! - [`openrouter`]: transcription and chat completion requests, retries, usage.
//! - [`keys`]: the API key, checked against the hash the owners signed.
//! - [`transcript`], [`summary`], [`report`], [`texts`]: what gets posted.
//! - [`session`]: one call; [`worker`]: the client, the requests, the sessions, posting.
//! - [`app`]: wiring for the binary.

pub mod app;
pub mod capture;
pub mod config;
pub mod keys;
pub mod openrouter;
pub mod report;
pub mod session;
pub mod summary;
#[cfg(test)]
mod test_support;
pub mod texts;
pub mod transcript;
pub mod wav;
pub mod worker;
