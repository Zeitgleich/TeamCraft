//! Dropping the handle waits until the engine's threads let go of the local store (#1980).
//!
//! SQLCipher frees its global state when the process exits; a thread still closing its connection
//! then frees memory into it, and the process aborts with "double free or corruption" after its
//! tests passed. SQLite removes the database's log when its last connection closes: once the
//! handle is gone, the log must be.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use teamcraft_ffi::{AccessToken, AppConfig, EventListener, LinkPlatform, TeamCraft, TokenProvider};

/// Nothing listens on the discard port, so every request fails fast as a network error.
const UNREACHABLE_SERVER: &str = "http://127.0.0.1:9";
/// Rounds of opening and dropping the engine.
const ROUNDS: usize = 20;
const ACCOUNT: &str = "tester";

#[tokio::test]
async fn the_store_is_closed_once_the_handle_is_dropped() {
  for _ in 0..ROUNDS {
    let directory = temp_dir();
    let teamcraft =
      TeamCraft::open_blocking(config(&directory), Arc::new(Tokens), Arc::new(Listener)).unwrap();
    // The read threads and the engine's jobs both ran, and a link wait runs beside the jobs.
    assert!(teamcraft.account_state().await.is_err());
    assert!(teamcraft.conversations().await.is_err());
    teamcraft
      .start_device_link("Test phone".into(), LinkPlatform::Android)
      .await
      .unwrap();
    let log = directory.join(format!("{ACCOUNT}.teamcraft.db-wal"));
    assert!(log.exists(), "the store is open in WAL mode");
    drop(teamcraft);
    assert!(!log.exists(), "every connection to the store is closed");
    std::fs::remove_dir_all(&directory).unwrap();
  }
}

struct Tokens;

impl TokenProvider for Tokens {
  fn access_token(&self) -> AccessToken {
    AccessToken::Token {
      value: "token".into(),
    }
  }
}

struct Listener;

impl EventListener for Listener {
  fn on_change(&self) {}
}

fn config(directory: &std::path::Path) -> AppConfig {
  AppConfig {
    server_url: UNREACHABLE_SERVER.into(),
    data_dir: directory.to_string_lossy().into_owned(),
    account: ACCOUNT.into(),
    database_key: [7; 32].to_vec(),
    push_gateway_url: None,
    is_memory_constrained: false,
  }
}

fn temp_dir() -> PathBuf {
  static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
  let directory = std::env::temp_dir().join(format!(
    "teamcraft-ffi-teardown-{}-{}",
    std::process::id(),
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
  ));
  std::fs::create_dir_all(&directory).unwrap();
  directory
}
