#![cfg(feature = "ui")]
//! Messages a device can't read yet stand as placeholders at their place (issue #888): waiting
//! while a key may come, "not available" once the wait ran out, filled in place when the key
//! arrives.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{Peer, settings, settle};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::Placeholder;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::store::{LocalStore, MessageFilter, Unreadable};
use teamcraft_proto::ConversationId;

const START_MS: i64 = 1_800_000_000_000;
const HOUR_MS: i64 = 60 * 60 * 1000;

#[tokio::test]
async fn a_message_waits_in_its_place_and_fills_in_when_its_key_arrives() {
  let server = FakeServer::new();
  let (mut phone, mut bob, mut laptop, channel, _clock) = laptop_missing_a_key(&server).await;
  let backend = common::backend_of(&laptop);

  let (count, page) = backend.message_page(channel, 0, 50);
  let waiting = page.last().unwrap();
  assert_eq!(
    waiting.maybe_placeholder,
    Some(Placeholder::Waiting),
    "at its place, the newest"
  );
  assert_eq!(waiting.author, bob.client.user_id());
  assert!(waiting.body.is_empty(), "no content before the key");
  let texts: Vec<&str> = page
    .iter()
    .filter(|message| message.maybe_placeholder.is_none())
    .map(|message| message.body.as_str())
    .filter(|body| !body.is_empty())
    .collect();
  assert_eq!(
    texts,
    ["one", "two", "three"],
    "the history the key bag opens is readable"
  );
  let placeholder_id = waiting.id;
  let progress = backend.key_catch_up().unwrap();
  assert_eq!((progress.caught_up, progress.total), (0, 1));

  // The user's other device comes back: it lets the laptop in, and the key reaches it.
  phone.client.full_sync().await.unwrap();
  phone.client.apply_pending_changes().await.unwrap();
  phone.client.upload_key_bag().await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  laptop.client.sync_account_data().await.unwrap();
  laptop.client.full_sync().await.unwrap();

  let (count_after, page) = backend.message_page(channel, 0, 50);
  assert_eq!(count_after, count, "filled in place: no row added or lost");
  let filled = page.last().unwrap();
  assert_eq!(filled.id, placeholder_id, "the row keeps its place");
  assert_eq!(filled.maybe_placeholder, None);
  assert_eq!(filled.body, "after the switch");
  assert!(page.iter().all(|message| message.maybe_placeholder.is_none()));
  assert_eq!(backend.key_catch_up(), None, "caught up");
}

#[tokio::test]
async fn a_key_from_the_user_s_other_device_fills_in_at_the_key_bag_merge() {
  let server = FakeServer::new();
  let (mut phone, mut bob, mut laptop, channel, _clock) = laptop_missing_a_key(&server).await;
  // The phone comes back and catches up; its key bag now holds the key.
  phone.client.full_sync().await.unwrap();
  settle(&mut [&mut phone, &mut bob]).await;
  phone.client.upload_key_bag().await.unwrap();

  // The laptop only merges the key bag: no catch-up of the conversation.
  laptop.client.sync_account_data().await.unwrap();
  assert!(laptop.texts(channel).contains(&"after the switch".to_string()));
}

#[tokio::test]
async fn a_key_that_never_comes_ends_as_not_available_not_as_an_endless_wait() {
  let server = FakeServer::new();
  let (_phone, _bob, mut laptop, channel, clock) = laptop_missing_a_key(&server).await;
  let backend = common::backend_of(&laptop);

  // Within the longest wait the laptop, not let into the group yet, keeps waiting.
  clock.advance(24 * HOUR_MS);
  assert!(!laptop.client.settle_missing_keys().unwrap());
  assert!(
    placeholders(&backend, channel)
      .iter()
      .all(|placeholder| *placeholder == Placeholder::Waiting)
  );

  clock.advance(3 * 24 * HOUR_MS);
  assert!(laptop.client.settle_missing_keys().unwrap());
  let shown = placeholders(&backend, channel);
  assert!(!shown.is_empty());
  assert!(
    shown
      .iter()
      .all(|placeholder| *placeholder == Placeholder::NotAvailable),
    "\"not available\" is no wait: {shown:?}"
  );
  assert_eq!(backend.key_catch_up(), None, "nothing waits any more");
  // Still parked: a key that turns up after all fills it in.
  assert_eq!(laptop.client.undecrypted_count().unwrap(), shown.len());
}

#[tokio::test]
async fn placeholders_count_as_neither_unread_nor_newest_activity() {
  let server = FakeServer::new();
  let (_phone, _bob, laptop, channel, _clock) = laptop_missing_a_key(&server).await;
  let backend = common::backend_of(&laptop);

  let conversation = backend.conversation(channel).unwrap();
  let readable_unread = laptop_readable_others(&laptop, channel);
  assert_eq!(
    conversation.unread_count as usize, readable_unread,
    "the placeholder may be a reaction or an edit: no phantom unread"
  );
  let newest_readable = laptop
    .store
    .messages(channel, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .filter(|message| message.maybe_unreadable.is_none())
    .map(|message| message.sent_at_ms)
    .max();
  assert_eq!(backend.last_activity(channel), newest_readable);
}

#[tokio::test]
async fn a_key_arriving_while_older_rows_are_shown_changes_only_its_own_row() {
  let server = FakeServer::new();
  let (mut phone, mut bob, mut laptop, channel, _clock) = laptop_missing_a_key(&server).await;
  let backend = common::backend_of(&laptop);
  // The list is scrolled up: it shows the oldest rows only.
  let before = backend.message_page(channel, 0, 2).1;

  phone.client.full_sync().await.unwrap();
  phone.client.apply_pending_changes().await.unwrap();
  phone.client.upload_key_bag().await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  laptop.client.sync_account_data().await.unwrap();
  laptop.client.full_sync().await.unwrap();

  assert_eq!(
    backend.message_page(channel, 0, 2).1,
    before,
    "the rows in view don't move"
  );
}

/// Alice's phone and Bob share a channel with history; Alice recovers a laptop (key bag from the
/// server, not in the group yet) while her phone is switched off, and Bob switches the channel's key and
/// writes. The laptop can't read that message: no device of Alice's that is online has its key.
async fn laptop_missing_a_key(server: &FakeServer) -> (Peer, Peer, Peer, ConversationId, Arc<ManualClock>) {
  let mut phone = Peer::enroll(server, "Alice").await;
  let mut bob = Peer::enroll(server, "Bob").await;
  let mut carol = Peer::enroll(server, "Carol").await;
  let space = phone.space().await;
  let channel = phone.client.create_channel(space, "general").await.unwrap();
  for user in [bob.client.user_id(), carol.client.user_id()] {
    phone.client.add_member(channel, user).await.unwrap();
  }
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  for text in ["one", "two"] {
    phone.client.send_text(channel, None, text).await.unwrap();
  }
  bob.client.send_text(channel, None, "three").await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  phone.client.upload_key_bag().await.unwrap();

  // The phone is switched off from here: it does nothing until a test syncs it again.
  // Removing Carol switches the key; Alice's devices learn it only from the phone or from Bob
  // letting the laptop in, and neither happens until the test syncs them again.
  bob
    .client
    .remove_member(channel, carol.client.user_id())
    .await
    .unwrap();
  bob
    .client
    .send_text(channel, None, "after the switch")
    .await
    .unwrap();
  let clock = Arc::new(ManualClock::new(START_MS));
  let mut laptop = Peer::enroll_on_clock(
    phone.api.clone(),
    "Alice laptop",
    settings(),
    Box::new(clock.clone()),
  )
  .await;
  laptop.client.full_sync().await.unwrap();
  assert!(
    !laptop.texts(channel).contains(&"after the switch".to_string()),
    "the laptop has no key for it yet"
  );
  assert!(
    laptop
      .store
      .messages(channel, MessageFilter::TopLevel)
      .unwrap()
      .iter()
      .any(|message| matches!(message.maybe_unreadable, Some(Unreadable::Waiting { .. })))
  );
  (phone, bob, laptop, channel, clock)
}

fn placeholders(
  backend: &teamcraft_core::ui_backend::CoreChatBackend<Arc<teamcraft_core::store::MemoryStore>>,
  channel: ConversationId,
) -> Vec<Placeholder> {
  backend
    .message_page(channel, 0, 50)
    .1
    .into_iter()
    .filter_map(|message| message.maybe_placeholder)
    .collect()
}

/// Readable messages of others: what alone may count as unread on a device that read nothing.
fn laptop_readable_others(laptop: &Peer, channel: ConversationId) -> usize {
  laptop
    .store
    .messages(channel, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .filter(|message| {
      message.maybe_unreadable.is_none()
        && message.sender_user_id != laptop.client.user_id()
        && message.maybe_server_id.is_some()
        && !message.is_hidden()
    })
    .count()
}
