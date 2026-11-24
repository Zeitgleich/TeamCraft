//! What TeamCraft's headless call participants share: the notetaker (e2ee.md §8.2) and the recorder
//! bot (e2ee.md §8.3, #2273). Each is a bot account with a device of its own, a member of the
//! conversations it serves and an ordinary participant of their calls with its own DAVE keys.
//!
//! - [`config`]: the sections every bot's TOML file has (server, store, identity, health).
//! - [`client`]: opening the encrypted client, enrolling the bot's device on the first run.
//! - [`tickets`]: join tickets from the server, for rejoining a call after a lost connection.
//! - [`gateway`]: keeping the gateway connection (backoff, pump, heartbeat).
//! - [`call`]: a call as a muted bot participant whose DAVE key the device key signs.
//! - [`health`]: the liveness file of the container health check.
//! - `test_call` (feature `test-support`): an in-process SFU and call keys for the bots' tests.
//! - [`texts`]: posted texts in German and English (Fluent).

pub mod call;
pub mod client;
pub mod config;
pub mod gateway;
pub mod health;
#[cfg(feature = "test-support")]
pub mod test_call;
pub mod texts;
pub mod tickets;

/// Now as Unix milliseconds; 0 before 1970, the maximum past `i64`.
pub fn unix_ms() -> i64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}
