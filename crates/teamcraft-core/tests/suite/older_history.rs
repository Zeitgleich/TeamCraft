//! Scrolling above what a device holds, through the UI adapter and the fake server: a page per
//! request, a state for the top of the list, failures that show and recover.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common::{Peer, backend_of, run_commands, settings, settle};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::OlderHistory;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::ClientSettings;
use teamcraft_proto::{ConversationId, HistoryPolicy};

#[tokio::test]
async fn pages_arrive_one_per_request_until_the_beginning() {
  let (_server, _alice, mut bob, channel) = bob_after_five_messages().await;
  let ui = backend_of(&bob);
  assert_eq!(ui.older_history(channel), OlderHistory::Available);
  assert!(bob.texts(channel).is_empty(), "the messages predate Bob");

  ui.load_older_history(channel);
  assert_eq!(ui.older_history(channel), OlderHistory::Loading);
  run_commands(&mut bob, &ui).await;
  assert_eq!(ui.older_history(channel), OlderHistory::Available);
  assert_eq!(bob.texts(channel), vec!["four", "five"]);

  ui.load_older_history(channel);
  run_commands(&mut bob, &ui).await;
  assert_eq!(bob.texts(channel), vec!["two", "three", "four", "five"]);

  ui.load_older_history(channel);
  run_commands(&mut bob, &ui).await;
  assert_eq!(bob.texts(channel), vec!["one", "two", "three", "four", "five"]);
  assert_eq!(
    ui.older_history(channel),
    OlderHistory::Complete,
    "a page that says there is no more ends it"
  );

  ui.load_older_history(channel);
  assert!(
    ui.drain_commands().is_empty(),
    "nothing is asked for at the beginning"
  );
}

#[tokio::test]
async fn a_request_on_its_way_is_not_repeated() {
  let (_server, _alice, bob, channel) = bob_after_five_messages().await;
  let ui = backend_of(&bob);

  ui.load_older_history(channel);
  ui.load_older_history(channel);
  ui.load_older_history(channel);

  assert_eq!(ui.drain_commands().len(), 1);
}

#[tokio::test]
async fn a_failure_shows_keeps_the_place_and_can_be_retried() {
  let (server, _alice, mut bob, channel) = bob_after_five_messages().await;
  let ui = backend_of(&bob);
  ui.load_older_history(channel);
  run_commands(&mut bob, &ui).await;
  let held = bob.texts(channel);

  server.set_rest_failing(bob.client.user_id(), true);
  ui.load_older_history(channel);
  // The answer goes to the top of the list, not to a toast for every scroll.
  run_commands(&mut bob, &ui).await;
  assert_eq!(ui.older_history(channel), OlderHistory::Failed);
  assert_eq!(bob.texts(channel), held);

  ui.load_older_history(channel);
  run_commands(&mut bob, &ui).await;
  assert_eq!(ui.older_history(channel), OlderHistory::Failed, "still down");

  server.set_rest_failing(bob.client.user_id(), false);
  ui.load_older_history(channel);
  assert_eq!(ui.older_history(channel), OlderHistory::Loading);
  run_commands(&mut bob, &ui).await;
  assert_eq!(ui.older_history(channel), OlderHistory::Available);
  assert_eq!(bob.texts(channel), vec!["two", "three", "four", "five"]);
}

#[tokio::test]
async fn a_new_connection_ends_a_failure_so_the_reader_needs_no_retry() {
  let (server, _alice, mut bob, channel) = bob_after_five_messages().await;
  let ui = backend_of(&bob);
  server.set_rest_failing(bob.client.user_id(), true);
  ui.load_older_history(channel);
  run_commands(&mut bob, &ui).await;
  assert_eq!(ui.older_history(channel), OlderHistory::Failed);
  server.set_rest_failing(bob.client.user_id(), false);

  let (connection, _) = bob.client.connect_gateway().await.unwrap();
  bob.gateway = connection;

  assert_eq!(ui.older_history(channel), OlderHistory::Available);
}

#[tokio::test]
async fn a_restart_keeps_the_beginning_and_forgets_the_rest() {
  let (server, _alice, mut bob, channel) = bob_after_five_messages().await;
  let ui = backend_of(&bob);
  server.set_rest_failing(bob.client.user_id(), true);
  ui.load_older_history(channel);
  run_commands(&mut bob, &ui).await;
  server.set_rest_failing(bob.client.user_id(), false);

  // A new backend is what a restart leaves: no request of its own, so the old failure is history.
  assert_eq!(backend_of(&bob).older_history(channel), OlderHistory::Available);

  for _ in 0..3 {
    ui.load_older_history(channel);
    run_commands(&mut bob, &ui).await;
  }
  assert_eq!(ui.older_history(channel), OlderHistory::Complete);
  assert_eq!(
    backend_of(&bob).older_history(channel),
    OlderHistory::Complete,
    "the beginning costs no request after a restart"
  );
}

/// Alice wrote five messages; Bob, added with the full history, holds none of them. Pages are two
/// messages long.
async fn bob_after_five_messages() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let small_pages = ClientSettings {
    history_page_size: 2,
    ..settings()
  };
  let mut bob = Peer::enroll_with_settings(&server, "Bob", small_pages).await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "archive")
    .await
    .unwrap();
  for text in ["one", "two", "three", "four", "five"] {
    alice.client.send_text(channel, None, text).await.unwrap();
  }
  alice
    .client
    .add_member_with_history(channel, bob.client.user_id(), Some(HistoryPolicy::Full))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  (server, alice, bob, channel)
}
