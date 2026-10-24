//! The UI's snapshot kept per conversation (issue #246): what a reader puts together from the
//! changes is what a full read shows, and a change in one conversation sends only that one.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use crate::common::{PASSPHRASE, Peer, clock, options, settings};
use teamcraft_client_api::remote::{ChatSnapshot, SecuritySnapshot};
use teamcraft_client_api::snapshot_changes::SnapshotCursor;
use teamcraft_core::api::fake::{FakeGateway, FakeServer, FakeServerApi};
use teamcraft_core::store::{MemoryStore, TrackedStore};
use teamcraft_core::ui_backend::CoreChatBackend;
use teamcraft_core::ui_snapshot::SnapshotCache;
use teamcraft_core::{Client, PendingEnrollment};
use teamcraft_proto::ConversationId;

type Store = TrackedStore<Arc<MemoryStore>>;

/// Carol's client as the desktop and web shells run it: a tracked store, a cache and the UI's copy.
struct Reader {
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
    let mut client = PendingEnrollment::new()
      .complete(
        server.add_user("Carol"),
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
