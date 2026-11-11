//! The pinned messages panel (issue #1735) on the store the desktop runs: SQLCipher behind a
//! tracked store with a message cache, so pins are found by one scan and followed through the
//! journal of writes afterwards.
#![cfg(all(feature = "ui", feature = "sqlcipher"))]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use crate::common::{PASSPHRASE, Peer, clock, options, settings};
use rand_core::OsRng;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::Message;
use teamcraft_core::api::fake::{FakeGateway, FakeServer, FakeServerApi};
use teamcraft_core::store::{
  DEFAULT_MESSAGE_CACHE_BYTES, LocalStore, MessageContent, MessageFilter, SqlCipherStore, StaticKeySource,
  TrackedStore,
};
use teamcraft_core::ui_backend::CoreChatBackend;
use teamcraft_core::{Client, PendingEnrollment};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::ConversationId;

type Store = TrackedStore<Arc<SqlCipherStore>>;

#[tokio::test]
async fn the_panel_lists_pins_made_after_it_was_read() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut carol = Desktop::enroll(&server, "pins-after-read").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "pins")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, carol.client.user_id())
    .await
    .unwrap();
  alice.client.send_text(channel, None, "the plan").await.unwrap();
  alice.client.send_text(channel, None, "the budget").await.unwrap();
  carol.settle(&mut alice).await;
  let shown = carol
    .backend
    .messages(channel, 0, carol.backend.message_count(channel));
  let plan = shown
    .iter()
    .find(|message| message.body == "the plan")
    .unwrap()
    .id;
  assert!(
    carol.pinned(channel).is_empty(),
    "the panel opens before anything is pinned"
  );

  carol.backend.set_pinned(channel, plan, true);
  assert_eq!(
    carol.pinned(channel),
    ["the plan"],
    "at once on the device that pinned"
  );
  carol.run_commands().await;
  carol.settle(&mut alice).await;
  assert_eq!(carol.pinned(channel), ["the plan"], "and once the server took it");

  let target = alice
    .client
    .store()
    .messages(channel, MessageFilter::All)
    .unwrap()
    .into_iter()
    .find(|stored| matches!(&stored.content, MessageContent::Text { body, .. } if body == "the budget"))
    .unwrap()
    .client_message_id;
  alice.client.set_pinned(channel, target, true).await.unwrap();
  carol.settle(&mut alice).await;
  assert_eq!(
    carol.pinned(channel),
    ["the plan", "the budget"],
    "a pin by another member"
  );

  carol.backend.set_pinned(channel, plan, false);
  carol.run_commands().await;
  carol.settle(&mut alice).await;
  assert_eq!(carol.pinned(channel), ["the budget"]);
}

/// A client as the desktop shell runs it.
struct Desktop {
  client: Client<FakeServerApi, Store>,
  gateway: FakeGateway,
  backend: CoreChatBackend<Store>,
  directory: PathBuf,
}

impl Desktop {
  async fn enroll(server: &FakeServer, name: &str) -> Self {
    let directory = std::env::temp_dir().join(format!("teamcraft-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let key = StaticKeySource::new(SymmetricKey::generate(&mut OsRng));
    let store = TrackedStore::with_message_cache(
      Arc::new(SqlCipherStore::open(&directory.join("store.db"), &key).unwrap()),
      DEFAULT_MESSAGE_CACHE_BYTES,
    );
    let api = server.add_user("Carol");
    let mut client = PendingEnrollment::new()
      .complete(api, store.clone(), clock(), settings(), options(PASSPHRASE))
      .await
      .unwrap();
    let (gateway, _) = client.connect_gateway().await.unwrap();
    let backend = CoreChatBackend::new(store, client.user_id(), clock());
    Self {
      client,
      gateway,
      backend,
      directory,
    }
  }

  fn pinned(&self, conversation: ConversationId) -> Vec<String> {
    let pinned: Vec<Message> = self.backend.pinned_messages(conversation);
    pinned.into_iter().map(|message| message.body).collect()
  }

  async fn run_commands(&mut self) {
    for command in self.backend.drain_commands() {
      self.client.run_ui_command(command).await.unwrap();
    }
    self.client.flush_outbox().await.unwrap();
  }

  async fn settle(&mut self, alice: &mut Peer) {
    for _ in 0..50 {
      let handled = alice.client.pump(&mut alice.gateway).await.unwrap()
        + self.client.pump(&mut self.gateway).await.unwrap();
      if handled == 0 {
        return;
      }
    }
    panic!("clients did not settle");
  }
}

impl Drop for Desktop {
  fn drop(&mut self) {
    // Best effort: a leftover directory in the temp dir is harmless.
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}
