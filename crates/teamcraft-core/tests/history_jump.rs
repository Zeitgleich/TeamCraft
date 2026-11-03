//! "Jump to date" through the UI adapter and the fake server: the history is requested by time,
//! merged into the local store, and the request is reported as answered. A page older than what is
//! held leaves a hole, which fills a page per request from the reader's side (#1156).
#![cfg(all(feature = "ui", not(target_arch = "wasm32")))]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Peer, backend_of, run_commands, settings, settle};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{GapEdge, OlderHistory};
use teamcraft_client_api::remote::{ChatAction, ChatAnswer, ChatQuery};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::ClientSettings;
use teamcraft_proto::{ConversationId, HistoryPolicy};

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

#[tokio::test]
async fn the_wire_forwards_a_date_request_and_its_answer() {
  // Web and desktop reach the core through these messages (#1156): the request has to arrive.
  let (_server, _alice, mut bob, channel) = bob_holding_the_newest().await;
  let ui = backend_of(&bob);
  let asked = ChatQuery::IsHistoryLoaded {
    conversation: channel,
    at: 0,
  };
  assert_eq!(asked.clone().answer(&ui), ChatAnswer::Flag { value: false });

  let action = ChatAction::RequestHistoryAt {
    conversation: channel,
    at: 0,
  };
  assert!(action.apply_to(&ui).is_none());
  run_commands(&mut bob, &ui).await;

  assert_eq!(asked.answer(&ui), ChatAnswer::Flag { value: true });
  assert_eq!(bob.texts(channel)[..2], ["one", "two"]);
}

#[tokio::test]
async fn the_wire_reads_and_fills_holes() {
  let (_server, _alice, mut bob, channel) = bob_holding_the_newest().await;
  let ui = backend_of(&bob);
  ui.request_history_at(channel, 0);
  run_commands(&mut bob, &ui).await;
  let gap = ui.history_gaps(channel)[0];
  let query = ChatQuery::HistoryGaps {
    conversation: channel,
  };
  assert_eq!(
    query.clone().answer(&ui),
    ChatAnswer::HistoryGaps {
      gaps: vec![gap],
      total: ui.message_count(channel),
    }
  );

  let action = ChatAction::FillHistoryGap {
    conversation: channel,
    after: gap.after,
    edge: GapEdge::Older,
  };
  assert!(action.apply_to(&ui).is_none());
  run_commands(&mut bob, &ui).await;

  assert_eq!(
    bob.texts(channel),
    ["one", "two", "three", "four", "seven", "eight"]
  );
}

#[tokio::test]
async fn a_jump_before_what_is_held_leaves_a_hole_that_fills_reading_down() {
  let (_server, _alice, mut bob, channel) = bob_holding_the_newest().await;
  let ui = backend_of(&bob);
  assert!(ui.history_gaps(channel).is_empty());

  ui.request_history_at(channel, 0);
  run_commands(&mut bob, &ui).await;
  assert_eq!(bob.texts(channel), ["one", "two", "seven", "eight"]);
  let gaps = ui.history_gaps(channel);
  assert_eq!(gaps.len(), 1, "between two and what was held");
  assert_eq!(gaps[0].index, row_of(&ui, channel, "two") + 1);
  assert_eq!(gaps[0].status, OlderHistory::Available);

  ui.fill_history_gap(channel, gaps[0], GapEdge::Older);
  assert_eq!(ui.history_gaps(channel)[0].status, OlderHistory::Loading);
  run_commands(&mut bob, &ui).await;
  assert_eq!(
    bob.texts(channel),
    ["one", "two", "three", "four", "seven", "eight"]
  );
  let gaps = ui.history_gaps(channel);
  assert_eq!(
    gaps[0].index,
    row_of(&ui, channel, "four") + 1,
    "the hole moved down"
  );

  fill_until_closed(&mut bob, &ui, channel, GapEdge::Older).await;
  assert_eq!(
    bob.texts(channel),
    ["one", "two", "three", "four", "five", "six", "seven", "eight"]
  );
  assert_eq!(
    ui.older_history(channel),
    OlderHistory::Available,
    "the history above the page is still to read"
  );
}

#[tokio::test]
async fn a_hole_fills_from_the_newer_side_for_a_reader_coming_up() {
  let (_server, _alice, mut bob, channel) = bob_holding_the_newest().await;
  let ui = backend_of(&bob);
  ui.request_history_at(channel, 0);
  run_commands(&mut bob, &ui).await;

  let gap = ui.history_gaps(channel)[0];
  ui.fill_history_gap(channel, gap, GapEdge::Newer);
  run_commands(&mut bob, &ui).await;
  assert_eq!(
    bob.texts(channel),
    ["one", "two", "five", "six", "seven", "eight"]
  );
  assert_eq!(
    ui.history_gaps(channel)[0].index,
    row_of(&ui, channel, "two") + 1,
    "the hole stays after the jumped page"
  );

  fill_until_closed(&mut bob, &ui, channel, GapEdge::Newer).await;
  assert_eq!(
    bob.texts(channel),
    ["one", "two", "three", "four", "five", "six", "seven", "eight"]
  );
}

#[tokio::test]
async fn a_failed_page_shows_at_the_hole_and_can_be_retried() {
  let (server, _alice, mut bob, channel) = bob_holding_the_newest().await;
  let ui = backend_of(&bob);
  ui.request_history_at(channel, 0);
  run_commands(&mut bob, &ui).await;

  server.set_rest_failing(bob.client.user_id(), true);
  ui.fill_history_gap(channel, ui.history_gaps(channel)[0], GapEdge::Older);
  run_commands(&mut bob, &ui).await;
  let gaps = ui.history_gaps(channel);
  assert_eq!(gaps.len(), 1, "the hole stays");
  assert_eq!(gaps[0].status, OlderHistory::Failed);

  server.set_rest_failing(bob.client.user_id(), false);
  ui.fill_history_gap(channel, gaps[0], GapEdge::Older);
  ui.fill_history_gap(channel, gaps[0], GapEdge::Older);
  assert_eq!(
    ui.drain_commands().len(),
    1,
    "a request on its way is not repeated"
  );
}

#[tokio::test]
async fn a_jump_into_what_is_held_leaves_no_hole() {
  let (_server, _alice, mut bob, channel) = bob_holding_the_newest().await;
  let ui = backend_of(&bob);
  for _ in 0..4 {
    ui.load_older_history(channel);
    run_commands(&mut bob, &ui).await;
  }
  assert_eq!(bob.texts(channel).len(), 8);

  ui.request_history_at(channel, 0);
  run_commands(&mut bob, &ui).await;

  assert!(ui.history_gaps(channel).is_empty());
}

/// Asks for the first hole's history from `edge` until there is no hole; fails after a few pages.
async fn fill_until_closed(
  bob: &mut Peer,
  ui: &teamcraft_core::ui_backend::CoreChatBackend<std::sync::Arc<teamcraft_core::store::MemoryStore>>,
  channel: ConversationId,
  edge: GapEdge,
) {
  for _ in 0..4 {
    let Some(gap) = ui.history_gaps(channel).first().copied() else {
      return;
    };
    ui.fill_history_gap(channel, gap, edge);
    run_commands(bob, ui).await;
  }
  assert!(ui.history_gaps(channel).is_empty(), "it closes at the rows held");
}

/// Where the row with `body` is in the feed.
fn row_of(ui: &impl ChatBackend, conversation: ConversationId, body: &str) -> usize {
  let rows = ui.messages(conversation, 0, ui.message_count(conversation));
  rows.iter().position(|row| row.body == body).unwrap()
}

/// Alice wrote six messages before Bob joined with the full history and two after: Bob holds those
/// two. Pages are two messages long.
async fn bob_holding_the_newest() -> (FakeServer, Peer, Peer, ConversationId) {
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
  for text in ["one", "two", "three", "four", "five", "six"] {
    alice.client.send_text(channel, None, text).await.unwrap();
  }
  alice
    .client
    .add_member_with_history(channel, bob.client.user_id(), Some(HistoryPolicy::Full))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  for text in ["seven", "eight"] {
    alice.client.send_text(channel, None, text).await.unwrap();
  }
  settle(&mut [&mut alice, &mut bob]).await;
  (server, alice, bob, channel)
}
