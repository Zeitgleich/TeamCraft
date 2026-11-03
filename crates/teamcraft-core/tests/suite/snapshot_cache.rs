//! The UI's snapshot kept per conversation (issue #246): what a reader puts together from the
//! changes is what a full read shows, and a change in one conversation sends only that one.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use crate::common::{PASSPHRASE, Peer, clock, options, settings};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{ThreadId, thread_id_of};
use teamcraft_client_api::remote::{ChatSnapshot, SecuritySnapshot};
use teamcraft_client_api::snapshot_changes::SnapshotCursor;
use teamcraft_core::api::fake::{FakeGateway, FakeServer, FakeServerApi};
use teamcraft_core::client::{LinkAnnouncement, NewDeviceLink};
use teamcraft_core::store::{MemoryStore, TrackedStore};
use teamcraft_core::ui_backend::CoreChatBackend;
use teamcraft_core::ui_snapshot::SnapshotCache;
use teamcraft_core::{Client, LinkOffer, PendingEnrollment};
use teamcraft_proto::{ClientMessageId, ConversationId, DeviceLinkPlatform};

type Store = TrackedStore<Arc<MemoryStore>>;

/// Carol's client as the desktop and web shells run it: a tracked store, a cache and the UI's copy.
struct Reader {
  api: FakeServerApi,
  client: Client<FakeServerApi, Store>,
  gateway: FakeGateway,
  store: Store,
  backend: CoreChatBackend<Store>,
  cache: SnapshotCache,
  held: ChatSnapshot,
  cursor: SnapshotCursor,
}

#[tokio::test]
async fn a_new_message_sends_only_its_conversation() {
  let (mut alice, mut carol, [first, second]) = workspace().await;

  alice.client.send_text(first, None, "hello").await.unwrap();
  carol.catch_up(&mut alice).await;
  let changes = carol.changes();

  let changed: Vec<ConversationId> = changes
    .conversations
    .iter()
    .map(|change| change.conversation.id)
    .collect();
  assert_eq!(changed, vec![first]);
  assert!(changes.maybe_order.is_none() && changes.maybe_spaces.is_none());
  let applied = carol.held.apply(changes);
  assert!(applied.touches(first) && !applied.touches(second));
  carol.assert_up_to_date();
}

/// A reply that stays in its thread leaves the conversation's entry as it was; the reader is still
/// told to re-read the feed, or the thread panel never shows it.
#[tokio::test]
async fn a_thread_reply_from_another_member_touches_its_conversation() {
  let (mut alice, mut carol, [first, second]) = workspace().await;
  let thread = carol.thread_of(&mut alice, first).await;

  alice
    .client
    .send_text(first, Some(thread), "in the thread")
    .await
    .unwrap();
  carol.catch_up(&mut alice).await;
  let changes = carol.changes();

  assert!(changes.feeds.contains(&first) || changes.conversations.iter().any(|c| c.conversation.id == first));
  let applied = carol.held.apply(changes);
  assert!(applied.touches(first) && !applied.touches(second));
  carol.assert_up_to_date();
  assert!(
    carol
      .thread_bodies(first, thread)
      .contains(&"in the thread".to_owned())
  );
  assert_eq!(carol.root_reply_count(first), 1);
}

/// The sender's own reply shows up while it is still pending, before the server has it.
#[tokio::test]
async fn an_own_thread_reply_touches_its_conversation_while_pending() {
  let (mut alice, mut carol, [first, second]) = workspace().await;
  let thread = carol.thread_of(&mut alice, first).await;

  carol.backend.post_message(first, Some(thread), "my reply");
  let changes = carol.changes();

  let applied = carol.held.apply(changes);
  assert!(applied.touches(first) && !applied.touches(second));
  assert!(
    carol
      .thread_bodies(first, thread)
      .contains(&"my reply".to_owned())
  );
}

/// Edits, reactions and deletes add no message; the reader still re-reads the feed right away,
/// not only after the next new message (issue #481).
mod changes_without_a_new_message {
  use super::*;

  #[tokio::test]
  async fn an_edit_touches_its_conversation() {
    let (mut alice, mut carol, [first, second], target) = with_message().await;

    alice.client.edit_message(first, target, "edited").await.unwrap();

    carol.assert_feed_follows(&mut alice, first, second).await;
    let feed = carol.backend.messages(first, 0, 10);
    assert!(
      feed
        .iter()
        .any(|message| message.body == "edited" && message.is_edited)
    );
  }

  #[tokio::test]
  async fn a_reaction_touches_its_conversation() {
    let (mut alice, mut carol, [first, second], target) = with_message().await;

    alice.client.react(first, target, "👍", true).await.unwrap();

    carol.assert_feed_follows(&mut alice, first, second).await;
    let feed = carol.backend.messages(first, 0, 10);
    let reacted = feed.iter().find(|message| message.body == "hello").unwrap();
    assert_eq!(reacted.reactions.len(), 1);
  }

  #[tokio::test]
  async fn a_delete_touches_its_conversation() {
    let (mut alice, mut carol, [first, second], target) = with_message().await;

    alice.client.delete_message(first, target).await.unwrap();

    carol.assert_feed_follows(&mut alice, first, second).await;
    let feed = carol.backend.messages(first, 0, 10);
    assert!(feed.iter().all(|message| message.body != "hello"));
  }

  /// Alice wrote "hello" and then "later" in the first channel; Carol has read both.
  async fn with_message() -> (Peer, Reader, [ConversationId; 2], ClientMessageId) {
    let (mut alice, mut carol, channels) = workspace().await;
    let target = alice.client.send_text(channels[0], None, "hello").await.unwrap();
    alice.client.send_text(channels[0], None, "later").await.unwrap();
    carol.catch_up(&mut alice).await;
    let changes = carol.changes();
    carol.held.apply(changes);
    (alice, carol, channels, target)
  }
}

#[tokio::test]
async fn nothing_written_is_nothing_to_send() {
  let (_alice, mut carol, _) = workspace().await;
  let changes = carol.changes();
  assert!(changes.is_empty());
  assert_eq!(changes.cursor, carol.cursor);
}

#[tokio::test]
async fn a_new_conversation_comes_with_the_new_order() {
  let (mut alice, mut carol, _) = workspace().await;
  let third = alice
    .client
    .create_channel(alice.space().await, "third")
    .await
    .unwrap();
  alice
    .client
    .add_member(third, carol.client.user_id())
    .await
    .unwrap();
  carol.catch_up(&mut alice).await;

  let changes = carol.changes();
  assert!(
    changes
      .maybe_order
      .as_ref()
      .is_some_and(|order| order.contains(&third))
  );
  carol.held.apply(changes);
  carol.assert_up_to_date();
}

#[tokio::test]
async fn another_cache_answers_with_everything() {
  let (_alice, mut carol, _) = workspace().await;
  carol.cache = SnapshotCache::new(2);
  let changes = carol.changes();
  assert!(changes.maybe_full.is_some());
  carol.held.apply(changes);
  carol.assert_up_to_date();
}

/// A new device of Carol's asks to be linked: the mark the approve prompt watches moves in the
/// changes the UI reads, and moves again once this device took the request up.
#[tokio::test]
async fn a_link_request_moves_the_mark_the_ui_reads() {
  let (_alice, mut carol, _) = workspace().await;
  let before = link_request_mark(&carol.held);
  let api = carol.api.clone();
  let offer = LinkOffer::generate();
  let announcement = LinkAnnouncement {
    device_name: "Tablet".into(),
    platform: DeviceLinkPlatform::Android,
  };
  let waiting = tokio::time::timeout(Duration::from_millis(200), async {
    let (link, announced) = tokio::join!(
      NewDeviceLink::connect(&api, &offer),
      offer.announce(&api, &announcement)
    );
    announced.unwrap();
    link
  });

  let (_, ()) = tokio::join!(waiting, async {
    while carol.client.link_requests().is_empty() {
      carol.client.pump(&mut carol.gateway).await.unwrap();
      tokio::task::yield_now().await;
    }
    let changes = carol.changes();
    carol.held.apply(changes);
    let announced = link_request_mark(&carol.held);
    assert!(announced > before, "the UI never hears of the request");

    let id = carol.client.link_requests()[0].id.clone();
    carol.client.reject_link_request(&id).await.unwrap();
    let changes = carol.changes();
    carol.held.apply(changes);
    assert!(link_request_mark(&carol.held) > announced);
  });
}

fn link_request_mark(snapshot: &ChatSnapshot) -> u64 {
  snapshot
    .maybe_live
    .as_ref()
    .map_or(0, |live| live.marks.link_requests)
}

/// Alice's space with two channels Carol is in; Carol has read her first snapshot.
async fn workspace() -> (Peer, Reader, [ConversationId; 2]) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut carol = Reader::enroll(&server).await;
  let space = alice.space().await;
  let mut channels = Vec::new();
  for name in ["first", "second"] {
    let channel = alice.client.create_channel(space, name).await.unwrap();
    alice
      .client
      .add_member(channel, carol.client.user_id())
      .await
      .unwrap();
    channels.push(channel);
  }
  carol.catch_up(&mut alice).await;
  let first = carol
    .cache
    .changes_since(&carol.backend, carol.store.take_changes(), security, None);
  carol.cursor = first.cursor;
  carol.held = *first.maybe_full.unwrap();
  carol.assert_up_to_date();
  (alice, carol, [channels[0], channels[1]])
}

impl Reader {
  async fn enroll(server: &FakeServer) -> Self {
    let store = TrackedStore::new(Arc::new(MemoryStore::new()));
    let api = server.add_user("Carol");
    let mut client = PendingEnrollment::new()
      .complete(
        api.clone(),
        store.clone(),
        clock(),
        settings(),
        options(PASSPHRASE),
      )
      .await
      .unwrap();
    let (gateway, _) = client.connect_gateway().await.unwrap();
    let backend = CoreChatBackend::new(store.clone(), client.user_id(), clock());
    let held = ChatSnapshot::read(&backend, SecuritySnapshot::default());
    Self {
      api,
      client,
      gateway,
      store,
      backend,
      cache: SnapshotCache::new(1),
      held,
      cursor: SnapshotCursor::default(),
    }
  }

  /// Lets both clients take their gateway frames until nothing new arrives.
  async fn catch_up(&mut self, alice: &mut Peer) {
    for _ in 0..50 {
      let handled = alice.client.pump(&mut alice.gateway).await.unwrap()
        + self.client.pump(&mut self.gateway).await.unwrap();
      if handled == 0 {
        return;
      }
    }
    panic!("clients did not settle");
  }

  /// Alice posts a message in `conversation`; returns its thread once Carol has read it.
  async fn thread_of(&mut self, alice: &mut Peer, conversation: ConversationId) -> ThreadId {
    alice.client.send_text(conversation, None, "root").await.unwrap();
    self.catch_up(alice).await;
    let changes = self.changes();
    self.held.apply(changes);
    let feed = self.backend.messages(conversation, 0, 10);
    thread_id_of(feed.iter().find(|message| message.body == "root").unwrap().id)
  }

  /// Takes Alice's change: only `changed` has to be re-read, and the held snapshot stays current.
  async fn assert_feed_follows(&mut self, alice: &mut Peer, changed: ConversationId, other: ConversationId) {
    self.catch_up(alice).await;
    let changes = self.changes();
    let applied = self.held.apply(changes);
    assert!(applied.touches(changed) && !applied.touches(other));
    self.assert_up_to_date();
  }

  fn thread_bodies(&self, conversation: ConversationId, thread: ThreadId) -> Vec<String> {
    self
      .backend
      .thread(conversation, thread)
      .into_iter()
      .map(|message| message.body)
      .collect()
  }

  fn root_reply_count(&self, conversation: ConversationId) -> usize {
    let feed = self.backend.messages(conversation, 0, 10);
    feed
      .iter()
      .find(|message| message.body == "root")
      .unwrap()
      .reply_count as usize
  }

  fn changes(&mut self) -> teamcraft_client_api::snapshot_changes::SnapshotChanges {
    let changes = self.cache.changes_since(
      &self.backend,
      self.store.take_changes(),
      security,
      Some(self.cursor),
    );
    self.cursor = changes.cursor;
    changes
  }

  /// What the UI put together equals a full read of the store.
  fn assert_up_to_date(&self) {
    let fresh = ChatSnapshot::read(&self.backend, SecuritySnapshot::default());
    assert_eq!(self.held, fresh);
  }
}

fn security() -> Option<SecuritySnapshot> {
  Some(SecuritySnapshot::default())
}
