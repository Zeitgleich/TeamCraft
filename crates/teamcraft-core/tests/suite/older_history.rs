//! Scrolling above what a device holds, through the UI adapter and the fake server: a page per
//! request, a state for the top of the list, failures that show and recover.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common::{Peer, backend_of, run_commands, settings, settle};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{Bookmark, BookmarkKind, OlderHistory};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::ClientSettings;
use teamcraft_core::store::LocalStore;
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

/// Alice pins a message and unpins it right away; Bob reads the history back in pages of two: the
/// unpin first, kept until the message is there, then the message with its pin. The pin is older,
/// so the message ends up unpinned, not pinned by the control applied last (issue #1438).
#[tokio::test]
async fn an_older_pin_read_later_doesnt_undo_a_newer_unpin() {
  let (_server, mut alice, mut bob, channel) = bob_and_an_archive().await;
  let target = alice.client.send_text(channel, None, "pin me").await.unwrap();
  alice.client.set_pinned(channel, target, true).await.unwrap();
  alice.client.set_pinned(channel, target, false).await.unwrap();
  alice.client.send_text(channel, None, "after").await.unwrap();
  add_bob(&mut alice, &mut bob, channel).await;
  let ui = backend_of(&bob);

  for _ in 0..2 {
    ui.load_older_history(channel);
    run_commands(&mut bob, &ui).await;
  }

  assert_eq!(bob.texts(channel), vec!["pin me", "after"]);
  let stored = bob.store.message(channel, target).unwrap().unwrap();
  assert!(!stored.is_pinned, "the older pin won");
}

/// Alice sets the bookmarks twice; Bob reads the newer list first and the older one a page later.
/// The bar shows the newer list (issue #1438).
#[tokio::test]
async fn older_bookmarks_read_later_dont_replace_newer_ones() {
  let (_server, mut alice, mut bob, channel) = bob_and_an_archive().await;
  let alice_ui = backend_of(&alice);
  for (title, text) in [("Old runbook", "one"), ("New runbook", "two")] {
    alice_ui.set_bookmarks(channel, vec![link(title)]);
    run_commands(&mut alice, &alice_ui).await;
    alice.client.send_text(channel, None, text).await.unwrap();
  }
  add_bob(&mut alice, &mut bob, channel).await;
  let ui = backend_of(&bob);

  ui.load_older_history(channel);
  run_commands(&mut bob, &ui).await;
  assert_eq!(bookmark_titles(&ui, channel), ["New runbook"]);
  ui.load_older_history(channel);
  run_commands(&mut bob, &ui).await;

  assert_eq!(bob.texts(channel), vec!["one", "two"]);
  assert_eq!(
    bookmark_titles(&ui, channel),
    ["New runbook"],
    "the older list won"
  );
}

/// Alice wrote five messages; Bob, added with the full history, holds none of them. Pages are two
/// messages long.
async fn bob_after_five_messages() -> (FakeServer, Peer, Peer, ConversationId) {
  let (server, mut alice, mut bob, channel) = bob_and_an_archive().await;
  for text in ["one", "two", "three", "four", "five"] {
    alice.client.send_text(channel, None, text).await.unwrap();
  }
  add_bob(&mut alice, &mut bob, channel).await;
  (server, alice, bob, channel)
}

/// Alice's empty channel and Bob, not in it yet, who reads pages of two messages.
async fn bob_and_an_archive() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let small_pages = ClientSettings {
    history_page_size: 2,
    ..settings()
  };
  let bob = Peer::enroll_with_settings(&server, "Bob", small_pages).await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "archive")
    .await
    .unwrap();
  (server, alice, bob, channel)
}

/// Adds Bob with the full history, which he holds none of yet.
async fn add_bob(alice: &mut Peer, bob: &mut Peer, channel: ConversationId) {
  alice
    .client
    .add_member_with_history(channel, bob.client.user_id(), Some(HistoryPolicy::Full))
    .await
    .unwrap();
  settle(&mut [alice, bob]).await;
}

fn link(title: &str) -> Bookmark {
  Bookmark {
    emoji: "📘".into(),
    title: title.into(),
    kind: BookmarkKind::Link,
    url: "https://example.com/runbook".into(),
    maybe_message: None,
  }
}

fn bookmark_titles(
  ui: &teamcraft_core::ui_backend::CoreChatBackend<std::sync::Arc<teamcraft_core::store::MemoryStore>>,
  channel: ConversationId,
) -> Vec<String> {
  let conversation = ui.conversation(channel).unwrap();
  conversation
    .bookmarks
    .into_iter()
    .map(|bookmark| bookmark.title)
    .collect()
}
