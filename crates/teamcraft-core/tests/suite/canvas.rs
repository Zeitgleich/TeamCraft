//! A channel's shared page (issue #1221, e2ee.md §4.11) through the UI adapter and the fake
//! server: edits of several members converge, arrive live as hidden messages the server holds only
//! as ciphertext, are refused where the user may not post, and survive a restart.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settings, settle};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::canvas::{self, Block, BlockKind, CanvasDoc};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::Client;
use teamcraft_core::ui_backend::CoreChatBackend;
use teamcraft_proto::{ConversationId, GatewayEvent, Permission};

#[tokio::test]
async fn concurrent_edits_of_two_members_converge_on_both_devices() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  let rows_before = bob_ui.message_count(channel);
  let unread_before = bob_ui.conversation(channel).unwrap().unread_count;
  let mut alice_page = CanvasDoc::new();
  let (heading, update) = alice_page.insert_block(None, BlockKind::Heading, "Weekly");
  alice_ui.edit_canvas(channel, update, false);
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let mut bob_page = page_of(&bob_ui, channel);
  assert_eq!(texts(&bob_page.blocks()), ["Weekly"], "the edit arrived live");

  // Both edit the same heading and add a block, neither seeing the other's edits first.
  let alice_edits = [
    alice_page.set_text(&heading, "Weekly sync").unwrap(),
    alice_page
      .insert_block(Some(&heading), BlockKind::Check, "book a room")
      .1,
  ];
  let bob_edits = [
    bob_page.set_text(&heading, "Team Weekly").unwrap(),
    bob_page
      .insert_block(Some(&heading), BlockKind::Bullet, "agenda")
      .1,
  ];
  for update in alice_edits {
    alice_ui.edit_canvas(channel, update, false);
  }
  for update in bob_edits {
    bob_ui.edit_canvas(channel, update, false);
  }
  run_commands(&mut alice, &alice_ui).await;
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let seen_by_alice = page_of(&alice_ui, channel).blocks();
  let seen_by_bob = page_of(&bob_ui, channel).blocks();
  assert_eq!(seen_by_alice, seen_by_bob);
  assert_eq!(seen_by_alice[0].text, "Team Weekly sync");
  let mut added = texts(&seen_by_alice[1..]);
  added.sort();
  assert_eq!(added, ["agenda", "book a room"]);

  // Page edits are no messages anybody reads in the conversation.
  assert_eq!(bob_ui.message_count(channel), rows_before);
  assert_eq!(bob_ui.conversation(channel).unwrap().unread_count, unread_before);
}

#[tokio::test]
async fn the_server_holds_page_edits_only_as_ciphertext() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let alice_ui = backend_of(&alice);
  let secret = "Merger with Project A closes Friday";
  let mut page = CanvasDoc::new();
  let (_, update) = page.insert_block(None, BlockKind::Paragraph, secret);
  alice_ui.edit_canvas(channel, update.clone(), false);
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let stored = server.stored_ciphertexts();
  assert!(!stored.is_empty());
  for ciphertext in &stored {
    assert!(
      !contains(ciphertext, secret.as_bytes()),
      "the page text is not in plain text"
    );
    assert!(!contains(ciphertext, &update), "nor is the update");
  }
  // Bob's device decrypted it all the same.
  assert_eq!(texts(&page_of(&backend_of(&bob), channel).blocks()), [secret]);
}

#[tokio::test]
async fn members_who_may_not_post_read_the_page_but_cannot_edit_it() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  let mut page = CanvasDoc::new();
  let (id, update) = page.insert_block(None, BlockKind::Paragraph, "Read me");
  alice_ui.edit_canvas(channel, update, false);
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  // An admin made the channel read-only for Bob.
  server.set_my_permissions(channel, Some(vec![Permission::AddReaction]));
  server.publish_event(
    &[bob.client.user_id()],
    &GatewayEvent::ConversationUpdate {
      conversation_id: channel,
    },
  );
  settle(&mut [&mut bob]).await;
  assert!(!bob_ui.conversation(channel).unwrap().can_post);

  let before = bob_ui.canvas(channel);
  let mut bob_page = page_of(&bob_ui, channel);
  let edit = bob_page.set_text(&id, "Changed by Bob").unwrap();
  bob_ui.edit_canvas(channel, edit, false);
  assert!(bob_ui.drain_commands().is_empty(), "nothing goes out");
  assert_eq!(bob_ui.canvas(channel), before, "nor is it kept");
  assert_eq!(texts(&page_of(&bob_ui, channel).blocks()), ["Read me"]);
}

#[tokio::test]
async fn the_page_survives_a_restart() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  let alice_ui = backend_of(&alice);
  let mut page = CanvasDoc::new();
  let (first, update) = page.insert_block(None, BlockKind::Check, "Ship it");
  alice_ui.edit_canvas(channel, update, false);
  alice_ui.edit_canvas(channel, page.set_checked(&first, true).unwrap(), false);
  // Kept at once, before it is sent.
  assert_eq!(page_of(&alice_ui, channel).blocks(), page.blocks());
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  for peer in [alice, bob] {
    let reopened = Client::open(peer.api.clone(), peer.store.clone(), common::clock(), settings())
      .unwrap()
      .unwrap();
    let ui = CoreChatBackend::new(peer.store.clone(), reopened.user_id(), common::clock());
    assert_eq!(page_of(&ui, channel).blocks(), page.blocks(), "{}", peer.name);
  }
}

#[tokio::test]
async fn a_compaction_keeps_the_page_and_waits_for_new_updates() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  let mut page = CanvasDoc::new();
  let (id, update) = page.insert_block(None, BlockKind::Paragraph, "a");
  alice_ui.edit_canvas(channel, update, false);
  for text in ["ab", "abc", "abcd"] {
    alice_ui.edit_canvas(channel, page.set_text(&id, text).unwrap(), false);
  }
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let state = bob_ui.canvas(channel);
  assert_eq!(state.updates.len(), 4);
  assert_eq!(state.updates_since_snapshot, 4);
  let versions_before = canvas::versions(&state).unwrap();
  let (merged, history) = canvas::compaction(&state).unwrap();
  // A stale revision changes nothing; the one read replaces the list.
  bob_ui.compact_canvas(channel, state.revision - 1, merged.clone(), history.clone());
  assert_eq!(bob_ui.canvas(channel).updates.len(), 4);
  bob_ui.compact_canvas(channel, state.revision, merged, history);
  let compacted = bob_ui.canvas(channel);
  assert_eq!(compacted.updates.len(), 1);
  assert!(compacted.is_compacted);
  assert_eq!(page_of(&bob_ui, channel).blocks(), page.blocks());
  assert_eq!(
    canvas::versions(&compacted).unwrap(),
    versions_before,
    "the history stays"
  );

  // A snapshot restarts the count.
  let whole = teamcraft_client_api::canvas::snapshot(&alice_ui.canvas(channel).updates)
    .unwrap()
    .unwrap();
  alice_ui.edit_canvas(channel, whole, true);
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(bob_ui.canvas(channel).updates_since_snapshot, 0);
}

#[tokio::test]
async fn every_device_lists_the_versions_with_who_and_when_and_restores_one() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  let mut alice_page = CanvasDoc::new();
  let (id, update) = alice_page.insert_block(None, BlockKind::Paragraph, "Budget: 10k");
  alice_ui.edit_canvas(channel, update, false);
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  // Bob overwrites Alice's line well after: a version of his own.
  server.advance_clock(std::time::Duration::from_secs(3600));
  let mut bob_page = page_of(&bob_ui, channel);
  bob_ui.edit_canvas(channel, bob_page.set_text(&id, "Budget: 50k").unwrap(), false);
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  for ui in [&alice_ui, &bob_ui] {
    let state = ui.canvas(channel);
    let versions = canvas::versions(&state).unwrap();
    assert_eq!(
      versions.iter().map(|version| version.by).collect::<Vec<_>>(),
      [bob.client.user_id(), alice.client.user_id()],
      "newest first, by who sent them"
    );
    assert!(versions[0].from_ms > versions[1].until_ms);
    assert_eq!(
      texts(&canvas::page_at(&state, &versions[1]).unwrap()),
      ["Budget: 10k"]
    );
  }

  // Alice restores her version: an edit like any other, which Bob's device takes.
  let state = alice_ui.canvas(channel);
  let old = canvas::page_at(&state, &canvas::versions(&state).unwrap()[1]).unwrap();
  let mut page = page_of(&alice_ui, channel);
  alice_ui.edit_canvas(channel, page.restore(&old).unwrap(), false);
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(texts(&page_of(&bob_ui, channel).blocks()), ["Budget: 10k"]);
  assert_eq!(canvas::versions(&bob_ui.canvas(channel)).unwrap().len(), 3);
}

/// The page as `ui` holds it.
fn page_of(ui: &impl ChatBackend, channel: ConversationId) -> CanvasDoc {
  CanvasDoc::from_updates(&ui.canvas(channel).updates).unwrap()
}

fn texts(blocks: &[Block]) -> Vec<String> {
  blocks.iter().map(|block| block.text.clone()).collect()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
  haystack.windows(needle.len()).any(|window| window == needle)
}

async fn channel_with_two() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "notes")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  (server, alice, bob, channel)
}
