//! Browser tests of the store on OPFS (issue #2266), in a dedicated worker: the only place with
//! sync access handles. Run like `tests/web.rs` (crate README); the two `bench_open_*` tests are
//! ignored and measure the open of a large store (docs/performance.md, "Web store on OPFS"): run
//! them with `--include-ignored --nocapture`, in a release build.
#![cfg(target_arch = "wasm32")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use rand_core::{OsRng, RngCore};
use teamcraft_core::store::{
  IndexedDbStore, LocalStore, MessageContent, MessageFilter, OpfsOpening, OpfsStore, StoreError,
  StoredMessage, Table,
};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{ClientMessageId, Id, Snowflake, UserId};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{console_log, wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

/// The store of docs/performance.md, "Web start on phones": 30 channels, 21,150 messages.
const BENCH_CHANNELS: i64 = 30;
const BENCH_MESSAGES_PER_CHANNEL: u32 = 705;

#[wasm_bindgen_test]
async fn store_survives_reopening() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  let (store, opening) = OpfsStore::open(&name, &key).await.unwrap();
  assert_eq!(opening, OpfsOpening::Migrated { records: 0 });
  store.put(Table::Users, "1", b"one").unwrap();
  store.put(Table::Users, "2", b"two").unwrap();
  store.delete(Table::Users, "2").unwrap();
  store.upsert_message(&message(7, 1, "hello")).unwrap();
  store.upsert_message(&message(7, 2, "world")).unwrap();
  store.upsert_message(&message(8, 3, "elsewhere")).unwrap();
  store.delete_messages(id(8)).unwrap();
  store.flush().unwrap();
  drop(store);

  let (reopened, opening) = OpfsStore::open(&name, &key).await.unwrap();
  assert_eq!(opening, OpfsOpening::Opened);
  assert_eq!(
    reopened.list(Table::Users).unwrap(),
    vec![("1".to_string(), b"one".to_vec())]
  );
  assert_eq!(bodies(&reopened, 7), ["hello", "world"]);
  assert!(reopened.messages(id(8), MessageFilter::All).unwrap().is_empty());
  drop(reopened);
  OpfsStore::delete(&name).await.unwrap();
}

/// Without a checkpoint the next open scans the log; with one it reads the index.
#[wasm_bindgen_test]
async fn opens_from_the_checkpoint_and_from_the_log_alike() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  let (store, _) = OpfsStore::open(&name, &key).await.unwrap();
  for client in 0..50 {
    store
      .upsert_message(&message(7, client, &format!("message {client}")))
      .unwrap();
  }
  drop(store);
  let (store, _) = OpfsStore::open(&name, &key).await.unwrap();
  assert_eq!(bodies(&store, 7).len(), 50);
  store.checkpoint().unwrap();
  drop(store);
  let (store, _) = OpfsStore::open(&name, &key).await.unwrap();
  assert_eq!(bodies(&store, 7)[49], "message 49");
  drop(store);
  OpfsStore::delete(&name).await.unwrap();
}

#[wasm_bindgen_test]
async fn another_key_cannot_open_the_store() {
  let name = database_name();
  let (store, _) = OpfsStore::open(&name, &SymmetricKey::generate(&mut OsRng))
    .await
    .unwrap();
  store.put(Table::Account, "account", b"secret").unwrap();
  drop(store);
  let result = OpfsStore::open(&name, &SymmetricKey::generate(&mut OsRng)).await;
  assert!(matches!(result, Err(StoreError::WrongKey)));
  OpfsStore::delete(&name).await.unwrap();
}

/// e2ee.md §9.4: one core per store. The files' locks keep a second open out while the first holds
/// the store, and let it in once the first closed.
#[wasm_bindgen_test]
async fn a_second_open_is_refused_while_the_store_is_open() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  let (store, _) = OpfsStore::open(&name, &key).await.unwrap();
  assert!(matches!(
    OpfsStore::open(&name, &key).await,
    Err(StoreError::InUse)
  ));
  drop(store);
  let (store, _) = OpfsStore::open(&name, &key).await.unwrap();
  drop(store);
  OpfsStore::delete(&name).await.unwrap();
}

mod from_indexed_db {
  use super::*;

  #[wasm_bindgen_test]
  async fn takes_the_records_once_and_empties_indexed_db() {
    let name = database_name();
    let key = SymmetricKey::generate(&mut OsRng);
    let old = IndexedDbStore::open(&name, &key).await.unwrap();
    old.put(Table::Users, "1", b"one").unwrap();
    for client in 0..20 {
      old
        .upsert_message(&message(7, client, &format!("old {client}")))
        .unwrap();
    }
    old.flush().await.unwrap();
    drop(old);

    let (store, opening) = OpfsStore::open(&name, &key).await.unwrap();
    assert!(matches!(opening, OpfsOpening::Migrated { records } if records > 20));
    assert_eq!(store.get(Table::Users, "1").unwrap(), Some(b"one".to_vec()));
    assert_eq!(bodies(&store, 7)[19], "old 19");
    store.put(Table::Users, "2", b"two").unwrap();
    drop(store);

    let emptied = IndexedDbStore::open(&name, &key).await.unwrap();
    assert_eq!(emptied.get(Table::Users, "1").unwrap(), None);
    drop(emptied);
    let (store, opening) = OpfsStore::open(&name, &key).await.unwrap();
    assert_eq!(opening, OpfsOpening::Opened);
    assert_eq!(store.get(Table::Users, "2").unwrap(), Some(b"two".to_vec()));
    drop(store);
    OpfsStore::delete(&name).await.unwrap();
  }

  /// Interrupted after the log was complete but before IndexedDB was emptied: the next open keeps
  /// the log (with what was written since) and empties IndexedDB then.
  #[wasm_bindgen_test]
  async fn a_stale_copy_left_in_indexed_db_is_ignored_and_removed() {
    let name = database_name();
    let key = SymmetricKey::generate(&mut OsRng);
    let (store, _) = OpfsStore::open(&name, &key).await.unwrap();
    store.put(Table::Users, "1", b"new").unwrap();
    drop(store);
    let stale = IndexedDbStore::open(&name, &key).await.unwrap();
    stale.put(Table::Users, "1", b"stale").unwrap();
    stale.flush().await.unwrap();
    drop(stale);

    let (store, opening) = OpfsStore::open(&name, &key).await.unwrap();
    assert_eq!(opening, OpfsOpening::Opened);
    assert_eq!(store.get(Table::Users, "1").unwrap(), Some(b"new".to_vec()));
    drop(store);
    let emptied = IndexedDbStore::open(&name, &key).await.unwrap();
    assert_eq!(emptied.get(Table::Users, "1").unwrap(), None);
    drop(emptied);
    OpfsStore::delete(&name).await.unwrap();
  }
}

/// docs/performance.md, "Web store on OPFS": the open of the IndexedDB store at the size the docs
/// use, what it holds in memory (every record's ciphertext), and one channel's messages read.
#[wasm_bindgen_test]
#[ignore = "benchmark; run alone with --include-ignored --nocapture"]
async fn bench_open_indexeddb() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  seed(&IndexedDbStore::open(&name, &key).await.unwrap()).await;
  let (records, sealed_bytes) = indexed_db_records(&name).await;
  let started = now_ms();
  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  let open_ms = now_ms() - started;
  let started = now_ms();
  let read = store.messages(id(1), MessageFilter::All).unwrap().len();
  let read_ms = now_ms() - started;
  console_log!(
    "bench indexeddb: open {open_ms:.0} ms; holds {records} records' ciphertext, {:.1} MiB; \
     {read} messages of one channel read in {read_ms:.0} ms",
    mib(sealed_bytes)
  );
}

/// [`bench_open_indexeddb`] for the store on OPFS: the move from IndexedDB once, then the open
/// measured; it holds the index (an ID and a place per record) instead of the ciphertext.
#[wasm_bindgen_test]
#[ignore = "benchmark; run alone with --include-ignored --nocapture"]
async fn bench_open_opfs() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  seed(&IndexedDbStore::open(&name, &key).await.unwrap()).await;
  let (records, _) = indexed_db_records(&name).await;
  let started = now_ms();
  let (store, _) = OpfsStore::open(&name, &key).await.unwrap();
  let migrate_ms = now_ms() - started;
  drop(store);
  let started = now_ms();
  let (store, _) = OpfsStore::open(&name, &key).await.unwrap();
  let open_ms = now_ms() - started;
  let started = now_ms();
  let read = store.messages(id(1), MessageFilter::All).unwrap().len();
  let read_ms = now_ms() - started;
  // 32 bytes of ID and 12 of place per entry; the B-tree's own nodes come on top.
  console_log!(
    "bench opfs: moved in {migrate_ms:.0} ms; open {open_ms:.0} ms; holds an index of {records} \
     entries, {:.1} MiB; {read} messages of one channel read in {read_ms:.0} ms",
    mib(records * 44)
  );
  drop(store);
  OpfsStore::delete(&name).await.unwrap();
}

/// How many records IndexedDB's store `name` holds, and their bytes, read by the page's script.
async fn indexed_db_records(name: &str) -> (usize, usize) {
  let count = js_sys::Function::new_with_args(
    "name",
    "return new Promise((resolve, reject) => { const open = indexedDB.open(name); \
     open.onerror = () => reject(open.error); open.onsuccess = () => { const db = open.result; \
     const all = db.transaction('records').objectStore('records').getAll(); \
     all.onerror = () => reject(all.error); all.onsuccess = () => { db.close(); \
     resolve([all.result.length, all.result.reduce((sum, value) => sum + value.byteLength, 0)]); }; }; });",
  );
  let promise: js_sys::Promise = count.call1(&JsValue::NULL, &name.into()).unwrap().into();
  let counted: js_sys::Array = wasm_bindgen_futures::JsFuture::from(promise)
    .await
    .unwrap()
    .into();
  (
    counted.get(0).as_f64().unwrap() as usize,
    counted.get(1).as_f64().unwrap() as usize,
  )
}

fn mib(bytes: usize) -> f64 {
  bytes as f64 / (1024.0 * 1024.0)
}

async fn seed(store: &IndexedDbStore) {
  for channel in 1..=BENCH_CHANNELS {
    store.begin_writes().unwrap();
    for number in 0..BENCH_MESSAGES_PER_CHANNEL {
      store.upsert_message(&bench_message(channel, number)).unwrap();
    }
    store.end_writes().unwrap();
    store.flush().await.unwrap();
  }
}

/// A message like `examples/web_start_seed.rs` writes.
fn bench_message(channel: i64, number: u32) -> StoredMessage {
  let unique = channel << 32 | i64::from(number);
  let mut client_id = [0_u8; 16];
  client_id[..8].copy_from_slice(&unique.to_le_bytes());
  StoredMessage {
    client_message_id: ClientMessageId::from_bytes(client_id),
    maybe_server_id: Some(id(1 + unique)),
    sent_at_ms: 1_700_000_000_000 + i64::from(number),
    content: MessageContent::Text {
      body: format!("An older message, number {number}, with a few words in it to read."),
      mentions: Vec::new(),
      meta: Default::default(),
    },
    ..message(channel, 0, "")
  }
}

fn now_ms() -> f64 {
  js_sys::Date::now()
}

fn bodies(store: &OpfsStore, conversation: i64) -> Vec<String> {
  store
    .messages(id(conversation), MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .map(|message| match message.content {
      MessageContent::Text { body, .. } => body,
      other => format!("{other:?}"),
    })
    .collect()
}

fn message(conversation: i64, client: u8, text: &str) -> StoredMessage {
  StoredMessage {
    conversation_id: id(conversation),
    client_message_id: ClientMessageId::from_bytes([client; 16]),
    maybe_server_id: Some(id(100 + i64::from(client))),
    sender_user_id: id::<UserId>(1),
    maybe_thread_id: None,
    sent_at_ms: 1,
    content: MessageContent::Text {
      body: text.into(),
      mentions: Vec::new(),
      meta: Default::default(),
    },
    is_edited: false,
    edits: Vec::new(),
    maybe_edit_id: None,
    is_pinned: false,
    maybe_verified_commit: None,
    maybe_server_change: None,
    maybe_unreadable: None,
  }
}

fn id<T: Id>(raw: i64) -> T {
  T::from_snowflake(Snowflake::new(raw).unwrap())
}

fn database_name() -> String {
  format!("teamcraft-test-{}", OsRng.next_u64())
}
