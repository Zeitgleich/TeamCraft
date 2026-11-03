//! Settings follow the user's devices inside the encrypted key bag (e2ee.md §6.4, issue #945): over
//! the fake server, with whole key bag uploads and conflicts as in production.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, link_device, settle};
use serde_json::{Value, json};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::error::CoreError;
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};
use teamcraft_proto::synced_settings::{
  MAX_SYNCED_SETTINGS_BYTES, SEED_VERSION, SettingWrite, SettingWriteKind,
};

#[tokio::test]
async fn a_change_reaches_the_other_device() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;

  phone
    .client
    .write_synced_settings(&[change("theme", json!("dark"))])
    .unwrap();
  phone.client.sync_account_data().await.unwrap();
  laptop.sync_account_data().await.unwrap();

  assert_eq!(laptop.account_data().settings["theme"].value, json!("dark"));
}

#[tokio::test]
async fn the_gateway_event_brings_a_change_without_polling() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;
  settle(&mut [&mut phone]).await;
  let backend = backend_of(&phone);
  let mark_before = backend.settings_mark();

  laptop
    .write_synced_settings(&[change("theme", json!("dark"))])
    .unwrap();
  laptop.sync_account_data().await.unwrap();
  settle(&mut [&mut phone]).await;

  assert_eq!(backend.account_settings()["theme"].value, json!("dark"));
  assert!(
    backend.settings_mark() > mark_before,
    "the UI is told to read again"
  );
}

#[tokio::test]
async fn the_ui_command_writes_and_the_ui_reads_it_back() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let backend = backend_of(&phone);
  phone
    .client
    .run_ui_command(CoreCommand::WriteSettings {
      writes: vec![change("theme", json!("dark"))],
    })
    .await
    .unwrap();

  let entry = &backend.account_settings()["theme"];
  assert_eq!(entry.value, json!("dark"));
  assert_eq!(entry.writer, phone.client.device_id());
}

#[tokio::test]
async fn the_ui_waits_until_the_device_has_merged_the_account() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;
  let backend = CoreChatBackend::new(
    laptop.store().clone(),
    laptop.user_id(),
    Box::new(ManualClock::new(1_800_000_000_000)),
  );
  assert_eq!(backend.settings_mark(), 0);
  laptop.sync_account_data().await.unwrap();
  assert!(backend.settings_mark() > 0);
}

#[tokio::test]
async fn devices_changing_different_settings_keep_both() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;

  phone
    .client
    .write_synced_settings(&[change("theme", json!("dark"))])
    .unwrap();
  laptop
    .write_synced_settings(&[change("density", json!("compact"))])
    .unwrap();
  phone.client.sync_account_data().await.unwrap();
  laptop.sync_account_data().await.unwrap();
  phone.client.sync_account_data().await.unwrap();

  for data in [phone.client.account_data(), laptop.account_data()] {
    assert_eq!(data.settings["theme"].value, json!("dark"));
    assert_eq!(data.settings["density"].value, json!("compact"));
  }
}

#[tokio::test]
async fn devices_changing_the_same_setting_converge_whoever_uploads_first() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;

  phone
    .client
    .write_synced_settings(&[change("theme", json!("dark"))])
    .unwrap();
  laptop
    .write_synced_settings(&[change("theme", json!("light"))])
    .unwrap();
  // The second upload hits a version conflict and merges.
  laptop.sync_account_data().await.unwrap();
  phone.client.sync_account_data().await.unwrap();
  laptop.sync_account_data().await.unwrap();

  let on_phone = phone.client.account_data().settings;
  assert_eq!(on_phone, laptop.account_data().settings);
  let winner = phone.client.device_id().max(laptop.device_id());
  assert_eq!(on_phone["theme"].writer, winner, "not whoever's clock is ahead");
}

#[tokio::test]
async fn the_device_clock_does_not_decide() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;

  phone
    .client
    .write_synced_settings(&[change("theme", json!("dark"))])
    .unwrap();
  phone
    .client
    .write_synced_settings(&[change("theme", json!("light"))])
    .unwrap();
  phone.client.sync_account_data().await.unwrap();
  laptop.sync_account_data().await.unwrap();
  // Written second, with a version after what it saw, whatever time this device believes it is.
  laptop
    .write_synced_settings(&[change("theme", json!("sepia"))])
    .unwrap();
  laptop.sync_account_data().await.unwrap();
  phone.client.sync_account_data().await.unwrap();

  let entry = &phone.client.account_data().settings["theme"];
  assert_eq!(entry.value, json!("sepia"));
  assert_eq!(entry.version, 3);
}

#[tokio::test]
async fn an_offline_change_waits_and_goes_out_later() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;
  laptop.sync_account_data().await.unwrap();

  // Nothing talks to the server here: the change is only in the store and marked for upload.
  laptop
    .write_synced_settings(&[change("theme", json!("dark"))])
    .unwrap();
  assert!(laptop.is_key_bag_dirty());
  phone.client.sync_account_data().await.unwrap();
  assert!(!phone.client.account_data().settings.contains_key("theme"));

  laptop.sync_account_data().await.unwrap();
  assert!(!laptop.is_key_bag_dirty());
  phone.client.sync_account_data().await.unwrap();
  assert_eq!(phone.client.account_data().settings["theme"].value, json!("dark"));
}

#[tokio::test]
async fn a_key_of_a_newer_client_survives_a_device_that_does_not_know_it() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;
  phone
    .client
    .write_synced_settings(&[change("from.a.newer.client", json!({"any": ["shape"]}))])
    .unwrap();
  phone.client.sync_account_data().await.unwrap();
  laptop.sync_account_data().await.unwrap();

  laptop
    .write_synced_settings(&[change("theme", json!("dark"))])
    .unwrap();
  laptop.sync_account_data().await.unwrap();
  phone.client.sync_account_data().await.unwrap();

  let settings = phone.client.account_data().settings;
  assert_eq!(settings["from.a.newer.client"].value, json!({"any": ["shape"]}));
  assert_eq!(settings["theme"].value, json!("dark"));
}

#[tokio::test]
async fn the_first_seed_stays_when_another_device_offers_its_own() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;
  phone
    .client
    .write_synced_settings(&[seed("theme", json!("dark"))])
    .unwrap();
  phone.client.sync_account_data().await.unwrap();
  laptop.sync_account_data().await.unwrap();

  laptop
    .write_synced_settings(&[seed("theme", json!("light"))])
    .unwrap();

  let entry = &laptop.account_data().settings["theme"];
  assert_eq!(entry.value, json!("dark"));
  assert_eq!(entry.version, SEED_VERSION);
}

#[tokio::test]
async fn too_large_a_value_is_refused_visibly_and_stays_out_of_the_key_bag() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let huge = "x".repeat(MAX_SYNCED_SETTINGS_BYTES);

  let outcome = phone.client.write_synced_settings(&[
    change("notifications.keywords", json!(huge)),
    change("theme", json!("dark")),
  ]);

  assert!(matches!(outcome, Err(CoreError::SettingsTooLarge { keys }) if keys == ["notifications.keywords"]));
  let settings = phone.client.account_data().settings;
  assert!(!settings.contains_key("notifications.keywords"));
  assert_eq!(settings["theme"].value, json!("dark"), "what fits is kept");
}

#[tokio::test]
async fn writing_the_same_value_again_does_not_upload() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  phone
    .client
    .write_synced_settings(&[change("theme", json!("dark"))])
    .unwrap();
  phone.client.sync_account_data().await.unwrap();
  assert!(!phone.client.is_key_bag_dirty());

  phone
    .client
    .write_synced_settings(&[change("theme", json!("dark"))])
    .unwrap();

  assert!(
    !phone.client.is_key_bag_dirty(),
    "a repeated write costs no key bag"
  );
}

fn change(key: &str, value: Value) -> SettingWrite {
  SettingWrite {
    key: key.to_owned(),
    value,
    kind: SettingWriteKind::Change,
  }
}

fn seed(key: &str, value: Value) -> SettingWrite {
  SettingWrite {
    key: key.to_owned(),
    value,
    kind: SettingWriteKind::Seed,
  }
}

fn backend_of(peer: &Peer) -> CoreChatBackend<impl teamcraft_core::store::LocalStore + Clone + use<>> {
  CoreChatBackend::new(
    peer.client.store().clone(),
    peer.client.user_id(),
    Box::new(ManualClock::new(1_800_000_000_000)),
  )
}
