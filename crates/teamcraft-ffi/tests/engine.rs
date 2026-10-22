//! The FFI surface without a server: opening, input validation and error mapping.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use teamcraft_ffi::{AppConfig, EventListener, TeamCraft, TeamCraftError, TokenProvider};

/// Nothing listens on the discard port, so every request fails fast as a network error.
const UNREACHABLE_SERVER: &str = "http://127.0.0.1:9";

#[tokio::test]
async fn a_fresh_device_needs_enrollment_and_reports_the_network() {
  let directory = temp_dir();
  let teamcraft = TeamCraft::new(config(&directory, [7; 32]), tokens(Some("token")), listener()).unwrap();
  assert!(matches!(
    teamcraft.account_state().await,
    Err(TeamCraftError::Network(_))
  ));
  assert!(matches!(
    teamcraft.conversations().await,
    Err(TeamCraftError::NotEnrolled)
  ));
  assert!(matches!(
    teamcraft.start().await,
    Err(TeamCraftError::NotEnrolled)
  ));
  assert!(matches!(
    teamcraft.wait_for_link_peer().await,
    Err(TeamCraftError::NoLinkInProgress)
  ));
  let code = teamcraft.start_device_link().await.unwrap();
  assert!(code.starts_with("tclink1:"));
  std::fs::remove_dir_all(&directory).unwrap();
}

#[tokio::test]
async fn without_a_token_calls_report_signed_out() {
  let directory = temp_dir();
  let teamcraft = TeamCraft::new(config(&directory, [7; 32]), tokens(None), listener()).unwrap();
  assert!(matches!(
    teamcraft.account_state().await,
    Err(TeamCraftError::SignedOut)
  ));
  std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn bad_configuration_is_rejected() {
  let directory = temp_dir();
  let mut short_key = config(&directory, [7; 32]);
  short_key.database_key.truncate(16);
  assert!(matches!(
    TeamCraft::new(short_key, tokens(None), listener()),
    Err(TeamCraftError::InvalidInput(_))
  ));
  let mut escaping = config(&directory, [7; 32]);
  escaping.account = "../elsewhere".into();
  assert!(matches!(
    TeamCraft::new(escaping, tokens(None), listener()),
    Err(TeamCraftError::InvalidInput(_))
  ));
  std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn reopening_with_the_same_key_works() {
  let directory = temp_dir();
  drop(TeamCraft::new(config(&directory, [7; 32]), tokens(None), listener()).unwrap());
  // An empty store has no record to fail on; this only checks reopening works with the same key.
  assert!(TeamCraft::new(config(&directory, [7; 32]), tokens(None), listener()).is_ok());
  std::fs::remove_dir_all(&directory).unwrap();
}

struct Tokens(Option<String>);

impl TokenProvider for Tokens {
  fn access_token(&self) -> Option<String> {
    self.0.clone()
  }
}

struct Listener;

impl EventListener for Listener {
  fn on_change(&self) {}
}

fn tokens(maybe_token: Option<&str>) -> Arc<dyn TokenProvider> {
  Arc::new(Tokens(maybe_token.map(str::to_string)))
}

fn listener() -> Arc<dyn EventListener> {
  Arc::new(Listener)
}

fn config(directory: &std::path::Path, key: [u8; 32]) -> AppConfig {
  AppConfig {
    server_url: UNREACHABLE_SERVER.into(),
    data_dir: directory.to_string_lossy().into_owned(),
    account: "tester".into(),
    database_key: key.to_vec(),
  }
}

fn temp_dir() -> PathBuf {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_nanos();
  let directory =
    std::env::temp_dir().join(format!("teamcraft-ffi-{nanos}-{:?}", std::thread::current().id()));
  std::fs::create_dir_all(&directory).unwrap();
  directory
}
