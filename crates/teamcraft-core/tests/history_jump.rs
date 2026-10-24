//! "Jump to date" through the UI adapter and the fake server: the history is requested by time,
//! merged into the local store, and the request is reported as answered.
#![cfg(all(feature = "ui", not(target_arch = "wasm32")))]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::HistoryPolicy;

#[tokio::test]
async fn a_date_request_loads_history_that_was_not_local() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "archive")
    .await
    .unwrap();
  for text in ["one", "two", "three"] {
    alice.client.send_text(channel, None, text).await.unwrap();
  }
  alice
    .client
    .add_member_with_history(channel, bob.client.user_id(), Some(HistoryPolicy::Full))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  let ui = backend_of(&bob);
  // Only the "added" notice is local; the three messages predate Bob.
  assert!(bob.texts(channel).is_empty());
  assert!(!ui.is_history_loaded(channel, 0));

  ui.request_history_at(channel, 0);
  run_commands(&mut bob, &ui).await;

  assert!(ui.is_history_loaded(channel, 0));
  assert!(
    !ui.is_history_loaded(channel, 1_000),
    "another date is a new request"
  );
  assert_eq!(bob.texts(channel), vec!["one", "two", "three"]);
}

#[tokio::test]
async fn a_date_after_the_newest_message_loads_nothing_but_is_answered() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "archive")
    .await
    .unwrap();
  alice.client.send_text(channel, None, "old").await.unwrap();
  let ui = backend_of(&alice);

  assert_eq!(
    alice
      .client
      .load_messages_from(channel, 4_000_000_000_000)
      .await
      .unwrap(),
    0
  );
  ui.request_history_at(channel, 4_000_000_000_000);
  run_commands(&mut alice, &ui).await;

  assert!(ui.is_history_loaded(channel, 4_000_000_000_000));
}
