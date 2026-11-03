//! The MLS state on disk (issue #424): one record per storage entry, so coming back to many new
//! conversations writes what each join changed, not the state of every group again after each
//! join; a group's records go with the group; a store of a version before 1.0.9 moves over.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::common::{self, PASSPHRASE, Peer, settle};
use teamcraft_core::api::fake::{FakeServer, FakeServerApi};
use teamcraft_core::store::{
  LocalStore, MediaRange, MemoryStore, MessageFilter, StoreError, StoredMessage, Table,
};
use teamcraft_core::{Client, PendingEnrollment};
use teamcraft_proto::{ClientMessageId, ConversationId, Id as _, MessageId};

/// Conversations of the larger catch-up; the smaller one has a quarter of them.
const CHANNELS: usize = 24;

mod catch_up {
  use super::*;

  /// The bytes of MLS state a catch-up writes per joined group stay the same however many groups
  /// the device is in; with one record for all groups they grew with every group.
  #[tokio::test]
  async fn writes_per_joined_group_do_not_grow_with_the_number_of_groups() {
    let few = catch_up_writes(CHANNELS / 4).await;
    let many = catch_up_writes(CHANNELS).await;
    let (few_per_join, many_per_join) = (few.bytes / (CHANNELS / 4), many.bytes / CHANNELS);
    assert!(
      many_per_join < few_per_join * 3 / 2,
      "{few_per_join} bytes per join with {} groups, {many_per_join} with {CHANNELS}",
      CHANNELS / 4
    );
  }

  /// The numbers of docs/performance.md, on a workspace of any size, in memory or with
  /// `MLS_STORAGE_SQLCIPHER=1` on a SQLCipher file:
  /// `MLS_STORAGE_CHANNELS=300 cargo test -p teamcraft-core --test suite -- --ignored --nocapture mls_storage`
  #[tokio::test]
  #[ignore = "measurement; prints the bytes a catch-up writes"]
  async fn measure_catch_up_writes() {
    let channels = std::env::var("MLS_STORAGE_CHANNELS")
      .ok()
      .and_then(|value| value.parse().ok())
      .unwrap_or(100);
    let (store, kind) = measured_store();
    let writes = catch_up_writes_on(channels, store).await;
    println!(
      "catch-up of {channels} groups ({kind}): {} MLS state writes, {} bytes ({} per group), \
       state at the end {} bytes, catch-up {:.2} s",
      writes.puts,
      writes.bytes,
      writes.bytes / channels.max(1),
      writes.final_bytes,
      writes.catch_up.as_secs_f64(),
    );
  }
}

mod groups {
  use super::*;

  /// A commit in one channel writes records of that channel's group, none of another's.
  #[tokio::test]
  async fn a_change_to_one_group_writes_only_its_records() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let dave = Peer::enroll(&server, "Dave").await;
    let (mut carol, store) = enroll_on_counting_store(&server, "Carol").await;
    let space = alice.space().await;
    let mut channels = Vec::new();
    for name in ["one", "two"] {
      let channel = alice.client.create_channel(space, name).await.unwrap();
      alice.client.add_member(channel, carol.user_id()).await.unwrap();
      channels.push(channel);
    }
    carol.full_sync().await.unwrap();
    assert!(group_records(&*store, channels[0]) >= 5);
    assert!(group_records(&*store, channels[1]) >= 5);

    store.reset_counts();
    alice
      .client
      .add_member(channels[0], dave.client.user_id())
      .await
      .unwrap();
    carol.full_sync().await.unwrap();

    let written = store.written_keys();
    assert!(
      !written.is_empty(),
      "the commit moved the group to the next epoch"
    );
    for key in &written {
      assert!(
        is_of_group(key, channels[0]),
        "a record not of the changed group was written: {}",
        String::from_utf8_lossy(&storage_key(key))
      );
    }
  }

  /// A device removed from a channel forgets the channel's MLS state, on disk too.
  #[tokio::test]
  async fn a_removed_device_keeps_no_record_of_the_group() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let channel = alice
      .client
      .create_channel(alice.space().await, "general")
      .await
      .unwrap();
    alice
      .client
      .add_member(channel, bob.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    assert!(group_records(&*bob.store, channel) >= 5);

    alice
      .client
      .remove_member(channel, bob.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    // The previous epoch's leaf keys too, which openmls leaves behind after a removal.
    assert_eq!(group_records(&*bob.store, channel), 0);
    assert!(group_records(&*alice.store, channel) >= 5, "Alice is still in it");
  }
}

mod migration {
  use super::*;

  /// A device whose MLS state a version before 1.0.9 stored as one record opens the store, moves
  /// the state to entries and goes on in its groups.
  #[tokio::test]
  async fn a_store_of_an_older_version_opens_and_its_groups_go_on() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let channel = alice
      .client
      .create_channel(alice.space().await, "general")
      .await
      .unwrap();
    alice
      .client
      .add_member(channel, bob.client.user_id())
      .await
      .unwrap();
    alice.client.send_text(channel, None, "before").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let entries = bob.store.list(Table::MlsStorage).unwrap();
    let (store, api) = (bob.store.clone(), bob.api.clone());
    drop(bob);
    store_as_before_v1_0_9(&store);

    let mut client = Client::open(api.clone(), store.clone(), common::clock(), common::settings())
      .unwrap()
      .unwrap();
    assert_eq!(
      store.list(Table::MlsStorage).unwrap(),
      entries,
      "moved back to entries"
    );
    let (gateway, _) = client.connect_gateway().await.unwrap();
    let mut bob = Peer {
      name: "Bob".into(),
      api,
      store,
      client,
      gateway,
    };
    alice.client.send_text(channel, None, "after").await.unwrap();
    bob.client.send_text(channel, None, "bob is here").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    assert_eq!(bob.texts(channel), ["before", "after", "bob is here"]);
    assert!(alice.texts(channel).contains(&"bob is here".to_string()));
  }

  /// Rewrites the MLS records as versions before 1.0.9 kept them: one record of length-prefixed
  /// storage keys and values.
  fn store_as_before_v1_0_9(store: &MemoryStore) {
    let mut legacy = Vec::new();
    for (key, value) in store.list(Table::MlsStorage).unwrap() {
      // The marker that replaces the old record goes too.
      if key.starts_with("entry/") {
        for chunk in [storage_key(&key), value] {
          legacy.extend_from_slice(&u32::try_from(chunk.len()).unwrap().to_be_bytes());
          legacy.extend_from_slice(&chunk);
        }
      }
      store.delete(Table::MlsStorage, &key).unwrap();
    }
    store.put(Table::MlsStorage, "provider", &legacy).unwrap();
  }
}

/// What a catch-up wrote of the MLS state.
struct MlsWrites {
  puts: usize,
  bytes: usize,
  /// The size of the MLS state in the store after it.
  final_bytes: usize,
  /// How long Carol's sync took (the fake server answers in memory).
  catch_up: std::time::Duration,
}

/// Carol comes back to `channels` channels Alice made and added her to while she was away, and
/// catches up: what she wrote of the MLS state meanwhile.
async fn catch_up_writes(channels: usize) -> MlsWrites {
  catch_up_writes_on(channels, CountingStore::default()).await
}

async fn catch_up_writes_on(channels: usize, store: CountingStore) -> MlsWrites {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let store = Arc::new(store);
  let mut carol = enroll_on(&server, "Carol", store.clone()).await;
  let space = alice.space().await;
  for index in 0..channels {
    let channel = alice
      .client
      .create_channel(space, &format!("channel {index}"))
      .await
      .unwrap();
    alice.client.add_member(channel, carol.user_id()).await.unwrap();
  }
  settle(&mut [&mut alice]).await;

  store.reset_counts();
  let started = std::time::Instant::now();
  carol.full_sync().await.unwrap();
  let catch_up = started.elapsed();

  let final_bytes = store
    .list(Table::MlsStorage)
    .unwrap()
    .iter()
    .map(|(key, value)| key.len() + value.len())
    .sum();
  MlsWrites {
    puts: store.mls_puts.load(Ordering::Relaxed),
    bytes: store.mls_bytes.load(Ordering::Relaxed),
    final_bytes,
    catch_up,
  }
}

/// How many MLS records of `conversation`'s group the store holds.
fn group_records(store: &impl LocalStore, conversation: ConversationId) -> usize {
  store
    .list(Table::MlsStorage)
    .unwrap()
    .iter()
    .filter(|(key, _)| is_of_group(key, conversation))
    .count()
}

/// Whether an MLS record is an entry of `conversation`'s group. openmls keys a group's entries with
/// the group ID as JSON (`openmls_memory_storage`), and the group ID ends with the conversation ID.
fn is_of_group(record_key: &str, conversation: ConversationId) -> bool {
  if !record_key.starts_with("entry/") {
    // The marker in the old record's place.
    return false;
  }
  let id = u64::try_from(conversation.snowflake().get()).unwrap();
  let id_bytes: Vec<String> = id.to_be_bytes().iter().map(u8::to_string).collect();
  let marker = format!(",{}]", id_bytes.join(","));
  storage_key(record_key)
    .windows(marker.len())
    .any(|window| window == marker.as_bytes())
}

/// The openmls storage key of an MLS record (`entry/` and the key in base64url).
fn storage_key(record_key: &str) -> Vec<u8> {
  URL_SAFE_NO_PAD
    .decode(record_key.strip_prefix("entry/").unwrap())
    .unwrap()
}

async fn enroll_on_counting_store(
  server: &FakeServer,
  name: &str,
) -> (Client<FakeServerApi, Arc<CountingStore>>, Arc<CountingStore>) {
  let store = Arc::new(CountingStore::default());
  (enroll_on(server, name, store.clone()).await, store)
}

async fn enroll_on(
  server: &FakeServer,
  name: &str,
  store: Arc<CountingStore>,
) -> Client<FakeServerApi, Arc<CountingStore>> {
  PendingEnrollment::new()
    .complete(
      server.add_user(name),
      store,
      common::clock(),
      common::settings(),
      common::options(PASSPHRASE),
    )
    .await
    .unwrap()
}

/// The store of the measurement: SQLCipher on a file with `MLS_STORAGE_SQLCIPHER=1`, else memory.
fn measured_store() -> (CountingStore, &'static str) {
  #[cfg(feature = "sqlcipher")]
  if std::env::var_os("MLS_STORAGE_SQLCIPHER").is_some() {
    use teamcraft_core::store::{SqlCipherStore, StaticKeySource};
    use teamcraft_crypto::secret::SymmetricKey;
    let directory = std::env::temp_dir().join(format!(
      "teamcraft-mls-storage-{}",
      rand_core::RngCore::next_u64(&mut rand_core::OsRng)
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let key = StaticKeySource::new(SymmetricKey::generate(&mut rand_core::OsRng));
    let store = SqlCipherStore::open(&directory.join("store.db"), &key).unwrap();
    return (CountingStore::on(Box::new(store)), "SQLCipher file");
  }
  (CountingStore::default(), "memory")
}

/// A store that counts what is written of the MLS state; in memory unless made [`Self::on`]
/// another.
struct CountingStore {
  inner: Box<dyn LocalStore + Send + Sync>,
  mls_puts: AtomicUsize,
  mls_bytes: AtomicUsize,
  /// Keys of the MLS records written or deleted.
  mls_keys: Mutex<Vec<String>>,
}

impl Default for CountingStore {
  fn default() -> Self {
    Self::on(Box::new(MemoryStore::new()))
  }
}

impl CountingStore {
  fn on(inner: Box<dyn LocalStore + Send + Sync>) -> Self {
    Self {
      inner,
      mls_puts: AtomicUsize::default(),
      mls_bytes: AtomicUsize::default(),
      mls_keys: Mutex::default(),
    }
  }

  fn reset_counts(&self) {
    self.mls_puts.store(0, Ordering::Relaxed);
    self.mls_bytes.store(0, Ordering::Relaxed);
    self.mls_keys.lock().unwrap().clear();
  }

  fn written_keys(&self) -> Vec<String> {
    self.mls_keys.lock().unwrap().clone()
  }
}

impl LocalStore for CountingStore {
  fn get(&self, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    self.inner.get(table, key)
  }

  fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError> {
    if table == Table::MlsStorage {
      self.mls_puts.fetch_add(1, Ordering::Relaxed);
      self
        .mls_bytes
        .fetch_add(key.len() + value.len(), Ordering::Relaxed);
      self.mls_keys.lock().unwrap().push(key.to_string());
    }
    self.inner.put(table, key, value)
  }

  fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
    if table == Table::MlsStorage {
      self.mls_keys.lock().unwrap().push(key.to_string());
    }
    self.inner.delete(table, key)
  }

  fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    self.inner.list(table)
  }

  fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
    self.inner.upsert_message(message)
  }

  fn message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    self.inner.message(conversation, client_message_id)
  }

  fn messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.inner.messages(conversation, filter)
  }

  fn message_by_server_id(
    &self,
    conversation: ConversationId,
    server_id: MessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    self.inner.message_by_server_id(conversation, server_id)
  }

  fn media_messages(
    &self,
    conversation: ConversationId,
    range: MediaRange,
    limit: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.inner.media_messages(conversation, range, limit)
  }

  fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
    self.inner.delete_messages(conversation)
  }

  fn delete_message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<(), StoreError> {
    self.inner.delete_message(conversation, client_message_id)
  }

  fn purge_freed(&self) -> Result<(), StoreError> {
    self.inner.purge_freed()
  }

  fn purge_freed_soon(&self) {
    self.inner.purge_freed_soon();
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    self.inner.begin_writes()
  }

  fn end_writes(&self) -> Result<(), StoreError> {
    self.inner.end_writes()
  }
}
