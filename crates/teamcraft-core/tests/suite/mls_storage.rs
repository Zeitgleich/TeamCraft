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
use teamcraft_core::client::is_catching_up;
use teamcraft_core::store::{
  Attached, AttachedRange, LocalStore, MemoryStore, MessageFilter, StoreError, StoredMessage, Table,
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

/// The first save after another process committed something other than MLS state (the iOS
/// notification extension's push bookkeeping, on every app resume).
#[cfg(feature = "sealed-sqlite")]
mod resume {
  use openmls_rust_crypto::OpenMlsRustCrypto;
  use openmls_traits::OpenMlsProvider as _;
  use rand_core::OsRng;
  use teamcraft_core::mls_storage::{SavedMlsStorage, load_mls_storage, save_mls_storage};
  use teamcraft_core::store::{SealedSqliteStore, StaticKeySource};
  use teamcraft_crypto::secret::SymmetricKey;

  use super::*;

  const GROUPS: usize = 300;
  /// About 9 KB per group in a dozen entries, the tree the largest (as measured).
  const ENTRY_SIZES: [usize; 12] = [3_000, 2_000, 1_000, 800, 600, 500, 400, 300, 200, 100, 50, 50];

  /// The numbers of docs/performance.md:
  /// `cargo test -p teamcraft-core --all-features --test suite -- --ignored --nocapture measure_save_after`
  #[test]
  #[ignore = "measurement; prints what the first save after another process's commit writes"]
  fn measure_save_after_another_process_committed() {
    let directory = std::env::temp_dir().join(format!("teamcraft-mls-resume-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    let app_store = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
    let provider = OpenMlsRustCrypto::default();
    for group in 0..GROUPS {
      for (entry, size) in ENTRY_SIZES.into_iter().enumerate() {
        set_entry(&provider, format!("{entry}/{group}").as_bytes(), &vec![7; size]);
      }
    }
    save_mls_storage(&app_store, &provider, &SavedMlsStorage::default()).unwrap();
    drop(provider);
    let (provider, saved) = load_mls_storage(&app_store).unwrap();

    let extension = SealedSqliteStore::open(&path, &StaticKeySource::new(key)).unwrap();
    extension.put(Table::Users, "pushed", b"bookkeeping").unwrap();
    drop(extension);

    set_entry(&provider, b"0/0", b"one message later");
    let started = std::time::Instant::now();
    save_mls_storage(&app_store, &provider, &saved).unwrap();
    println!(
      "first save after another process's commit: {} bytes written in {:?}",
      saved.written_bytes(),
      started.elapsed()
    );
    drop(app_store);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  fn set_entry(provider: &OpenMlsRustCrypto, key: &[u8], value: &[u8]) {
    provider.storage().set_value(key.to_vec(), value.to_vec());
  }
}

mod groups {
  use super::*;

  /// The provider storage notes the keys it changes (crates/vendor/openmls_memory_storage), and a
  /// save writes those. Through everything real MLS operations do to it, what a start would load is
  /// what the running client holds: a key the storage failed to note would differ here.
  #[tokio::test]
  async fn what_was_saved_is_what_the_provider_holds_after_real_operations() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let dave = Peer::enroll(&server, "Dave").await;
    let (mut carol, store) = enroll_on_counting_store(&server, "Carol").await;
    assert_saved(&carol, &store, "enrolled");

    let space = alice.space().await;
    let joined = alice.client.create_channel(space, "joined").await.unwrap();
    alice.client.add_member(joined, carol.user_id()).await.unwrap();
    let left = alice.client.create_channel(space, "left").await.unwrap();
    alice.client.add_member(left, carol.user_id()).await.unwrap();
    carol.full_sync().await.unwrap();
    assert_saved(&carol, &store, "joined by welcome");

    alice.client.send_text(joined, None, "hello").await.unwrap();
    carol.full_sync().await.unwrap();
    assert_saved(&carol, &store, "received a message");

    carol.send_text(joined, None, "hello back").await.unwrap();
    assert_saved(&carol, &store, "sent a message");

    alice
      .client
      .add_member(joined, dave.client.user_id())
      .await
      .unwrap();
    carol.full_sync().await.unwrap();
    assert_saved(&carol, &store, "processed another's commit");

    let own = carol.create_channel(space, "own").await.unwrap();
    assert_saved(&carol, &store, "founded a group");
    carol.add_member(own, dave.client.user_id()).await.unwrap();
    assert_saved(&carol, &store, "made a commit");

    alice.client.remove_member(left, carol.user_id()).await.unwrap();
    carol.full_sync().await.unwrap();
    assert_saved(&carol, &store, "removed from a group");
  }

  #[track_caller]
  fn assert_saved(client: &Client<FakeServerApi, Arc<CountingStore>>, store: &CountingStore, step: &str) {
    use openmls_traits::OpenMlsProvider as _;

    let (loaded, _) = teamcraft_core::mls_storage::load_mls_storage(store).unwrap();
    let saved = loaded.storage().values.read().unwrap();
    let held = client.mls_provider().storage().values.read().unwrap();
    assert_eq!(saved.len(), held.len(), "number of entries, {step}");
    for (key, value) in held.iter() {
      assert_eq!(
        saved.get(key),
        Some(value),
        "{step}: {}",
        String::from_utf8_lossy(key)
      );
    }
  }

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

mod opening {
  use super::*;

  /// Issue #840: the iOS notification extension opens the client on every push. Opening commits
  /// nothing when nothing changed, so the commit of its push bookkeeping is the only one.
  #[tokio::test]
  async fn opening_the_client_again_writes_nothing() {
    let server = FakeServer::new();
    let api = server.add_user("Carol");
    let store = Arc::new(CountingStore::default());
    let carol = PendingEnrollment::new()
      .complete(
        api.clone(),
        store.clone(),
        common::clock(),
        common::settings(),
        common::options(PASSPHRASE),
      )
      .await
      .unwrap();
    drop(carol);
    drop(Client::open(api.clone(), store.clone(), common::clock(), common::settings()).unwrap());

    store.reset_counts();
    drop(Client::open(api, store.clone(), common::clock(), common::settings()).unwrap());

    assert_eq!(store.writes(), 0, "{:?}", store.written_tables());
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
    let entries = entry_records(&bob.store);
    let (store, api) = (bob.store.clone(), bob.api.clone());
    drop(bob);
    store_as_before_v1_0_9(&store);

    let mut client = Client::open(api.clone(), store.clone(), common::clock(), common::settings())
      .unwrap()
      .unwrap();
    assert_eq!(entry_records(&store), entries, "moved back to entries");
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
  /// Issue #840: moving the record takes several times the state in memory, which the iOS
  /// notification extension doesn't have, and it would keep the app out of the store meanwhile. The
  /// process short of memory doesn't open and leaves everything as it found it; the app moves it.
  #[tokio::test]
  async fn a_process_short_of_memory_leaves_the_move_to_the_app() {
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
    let entries = entry_records(&bob.store);
    let (store, api) = (bob.store.clone(), bob.api.clone());
    drop(bob);
    store_as_before_v1_0_9(&store);
    let unmoved = store.list(Table::MlsStorage).unwrap();

    let short_of_memory = teamcraft_core::ClientSettings {
      leaves_mls_upkeep_to_app: true,
      ..common::settings()
    };
    let refused = Client::open(api.clone(), store.clone(), common::clock(), short_of_memory);
    assert!(matches!(
      refused,
      Err(teamcraft_core::CoreError::MlsStateNotMoved)
    ));
    assert_eq!(store.list(Table::MlsStorage).unwrap(), unmoved, "not touched");

    Client::open(api, store.clone(), common::clock(), common::settings())
      .unwrap()
      .unwrap();
    assert_eq!(entry_records(&store), entries, "the app moved it");
  }

  /// Issue #1198: the app may be catching up next to the extension: opening writes nothing there,
  /// and the marks of its catch-up stay. The app's own open still drops them.
  #[tokio::test]
  async fn a_process_short_of_memory_writes_nothing_when_the_client_opens() {
    let server = FakeServer::new();
    let bob = Peer::enroll(&server, "Bob").await;
    let (store, api) = (bob.store.clone(), bob.api.clone());
    drop(bob);
    let conversation = ConversationId::from_snowflake(teamcraft_proto::Snowflake::new(7).unwrap());
    store
      .put(Table::SyncState, &format!("catching_up/{conversation}"), b"true")
      .unwrap();
    teamcraft_core::records::merge_removed_sender(
      &store,
      "sender-1",
      &teamcraft_core::account_data::SenderAbsence {
        left_at_ms: 1_000,
        maybe_back_at_ms: None,
      },
    )
    .unwrap();

    let counted = CountingStore::on(Box::new(store.clone()));
    let short_of_memory = teamcraft_core::ClientSettings {
      leaves_mls_upkeep_to_app: true,
      // A limit the store doesn't hold yet: the app notes it.
      max_upload_bytes: Some(1),
      ..common::settings()
    };
    let client = Client::open(api.clone(), &counted, common::clock(), short_of_memory)
      .unwrap()
      .unwrap();
    drop(client);
    assert_eq!(counted.written_tables(), Vec::<String>::new());
    assert!(is_catching_up(&store, conversation).unwrap(), "the marks stay");

    Client::open(api, store.clone(), common::clock(), common::settings())
      .unwrap()
      .unwrap();
    assert!(
      !is_catching_up(&store, conversation).unwrap(),
      "the app drops them"
    );
  }

  /// The entry records of the MLS table, without what a save writes besides them.
  fn entry_records(store: &MemoryStore) -> Vec<(String, Vec<u8>)> {
    let mut records = store.list(Table::MlsStorage).unwrap();
    records.retain(|(key, _)| key.starts_with("entry/"));
    records
  }

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
  /// Keys of the MLS entries written or deleted.
  mls_keys: Mutex<Vec<String>>,
  /// Tables and keys of every write.
  writes: Mutex<Vec<String>>,
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
      writes: Mutex::default(),
    }
  }

  fn writes(&self) -> usize {
    self.writes.lock().unwrap().len()
  }

  fn written_tables(&self) -> Vec<String> {
    self.writes.lock().unwrap().clone()
  }

  fn note_write(&self, table: Table, key: &str) {
    self
      .writes
      .lock()
      .unwrap()
      .push(format!("{}/{key}", table.name()));
    if table == Table::MlsStorage && key.starts_with("entry/") {
      self.mls_keys.lock().unwrap().push(key.to_string());
    }
  }

  fn reset_counts(&self) {
    self.writes.lock().unwrap().clear();
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
    }
    self.note_write(table, key);
    self.inner.put(table, key, value)
  }

  fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
    self.note_write(table, key);
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

  fn attached_messages(
    &self,
    conversation: ConversationId,
    kind: Attached,
    range: AttachedRange,
    limit: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.inner.attached_messages(conversation, kind, range, limit)
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

  fn wait_for_purge(&self, max_wait: std::time::Duration) -> bool {
    self.inner.wait_for_purge(max_wait)
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    self.inner.begin_writes()
  }

  fn end_writes(&self) -> Result<(), StoreError> {
    self.inner.end_writes()
  }
}
