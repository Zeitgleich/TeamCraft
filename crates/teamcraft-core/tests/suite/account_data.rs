//! Saved items, drafts and saved searches follow the user across devices inside the encrypted
//! key bag (e2ee.md §6.4).
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, link_device, settle};
use teamcraft_client_api::backend::{ChatBackend, SearchBackend};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::store::{LocalStore, MessageFilter};
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};

#[tokio::test]
async fn saved_items_drafts_and_searches_reach_the_other_device() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;
  let channel = phone
    .client
    .create_channel(phone.space().await, "notes")
    .await
    .unwrap();
  phone
    .client
    .send_text(channel, None, "remember this")
    .await
    .unwrap();
  let message = phone.store.messages(channel, MessageFilter::TopLevel).unwrap()[0].clone();

  for command in [
    CoreCommand::SetSaved {
      conversation: channel,
      client_message_id: message.client_message_id,
      is_saved: true,
    },
    CoreCommand::SetDraft {
      conversation: channel,
      maybe_thread: None,
      body: "half a sentence".into(),
    },
    CoreCommand::SaveSearch {
      id: 7,
      name: "Mine".into(),
      query: "from:@me".into(),
    },
  ] {
    phone.client.run_ui_command(command).await.unwrap();
  }
  phone.client.sync_account_data().await.unwrap();
  laptop.sync_account_data().await.unwrap();

  let data = laptop.account_data();
  assert!(data.is_saved(channel, message.client_message_id));
  assert_eq!(data.draft(channel, None), Some("half a sentence"));
  assert_eq!(data.saved_searches()[0].1.name, "Mine");

  // The UI reads them from the laptop's store.
  phone.client.add_missing_devices(channel).await.unwrap();
  laptop.sync_welcomes().await.unwrap();
  laptop.load_older_messages(channel).await.unwrap();
  let backend = CoreChatBackend::new(
    laptop.store().clone(),
    laptop.user_id(),
    Box::new(ManualClock::new(1_800_000_000_000)),
  );
  assert_eq!(backend.saved_items()[0].message.body, "remember this");
  assert_eq!(backend.draft(channel, None).as_deref(), Some("half a sentence"));
  assert_eq!(backend.saved_searches()[0].name, "Mine");
  settle(&mut [&mut phone]).await;
}

#[tokio::test]
async fn concurrent_changes_on_two_devices_merge() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;
  let channel = phone
    .client
    .create_channel(phone.space().await, "drafts")
    .await
    .unwrap();

  // Both change different things before either syncs; the second upload hits a version conflict
  // and merges instead of overwriting.
  phone
    .client
    .run_ui_command(CoreCommand::SaveSearch {
      id: 1,
      name: "from phone".into(),
      query: "a".into(),
    })
    .await
    .unwrap();
  laptop
    .run_ui_command(CoreCommand::SetDraft {
      conversation: channel,
      maybe_thread: None,
      body: "from laptop".into(),
    })
    .await
    .unwrap();
  phone.client.sync_account_data().await.unwrap();
  laptop.sync_account_data().await.unwrap();
  phone.client.sync_account_data().await.unwrap();

  for data in [phone.client.account_data(), laptop.account_data()] {
    assert_eq!(data.saved_searches().len(), 1);
    assert_eq!(data.draft(channel, None), Some("from laptop"));
  }
  assert!(!phone.client.is_key_bag_dirty() && !laptop.is_key_bag_dirty());
}
