//! Browser tests of the store on OPFS (issue #2266), in a dedicated worker: the only place with
//! sync access handles. Run like `tests/web.rs` (crate README); the two `bench_open_*` tests are
//! ignored and measure the open of a large store (docs/performance.md, "Web store on OPFS"): run
//! them with `--include-ignored --nocapture`, in a release build.
#![cfg(target_arch = "wasm32")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::ops::ControlFlow;

use rand_core::{OsRng, RngCore};
use teamcraft_core::store::{
  BrowserStore, IndexedDbStore, LocalStore, MessageContent, MessageFilter, OpfsOpening, OpfsStore,
  ServiceWorkerStore, StoreError, StoredMessage, Table,
};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{ClientMessageId, Id, Snowflake, UserId};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{console_log, wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{
  FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions, FileSystemGetFileOptions,
  FileSystemSyncAccessHandle, WorkerGlobalScope,
};

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

/// The service worker's view of the store (e2ee.md §9.3): a copy of the log read through
/// `getFile()`, which works in any worker; the tests run it in this dedicated worker, where the
/// store can be written first. `web_service_worker.rs` opens it in a real service worker.
mod in_the_service_worker {
  use super::*;

  #[wasm_bindgen_test]
  async fn reads_the_log_and_refuses_writes() {
    let name = database_name();
    let key = SymmetricKey::generate(&mut OsRng);
    let (store, _) = OpfsStore::open(&name, &key).await.unwrap();
    store.put(Table::Users, "1", b"one").unwrap();
    for client in 0..30 {
      store
        .upsert_message(&message(7, client, &format!("message {client}")))
        .unwrap();
    }
    store.flush().unwrap();
    drop(store);

    let copy = ServiceWorkerStore::open_for_service_worker(&name, &key)
      .await
      .unwrap();
    assert!(copy.is_on_opfs());
    assert_eq!(copy.get(Table::Users, "1").unwrap(), Some(b"one".to_vec()));
    assert_eq!(bodies(&copy, 7)[29], "message 29");
    assert!(matches!(
      copy.put(Table::Users, "2", b"two"),
      Err(StoreError::ReadOnly)
    ));
    drop(copy);

    let (store, _) = OpfsStore::open(&name, &key).await.unwrap();
    assert_eq!(store.get(Table::Users, "2").unwrap(), None);
    drop(store);
    OpfsStore::delete(&name).await.unwrap();
  }

  /// The owner's worker holds the files' sync access handles while the service worker copies them.
  #[wasm_bindgen_test]
  async fn reads_while_the_owner_holds_the_store() {
    let name = database_name();
    let key = SymmetricKey::generate(&mut OsRng);
    let (store, _) = OpfsStore::open(&name, &key).await.unwrap();
    store.put(Table::Users, "1", b"one").unwrap();
    store.flush().unwrap();
    store.checkpoint().unwrap();
    store.put(Table::Users, "2", b"two").unwrap();
    store.flush().unwrap();

    let copy = ServiceWorkerStore::open_for_service_worker(&name, &key)
      .await
      .unwrap();
    assert_eq!(copy.get(Table::Users, "1").unwrap(), Some(b"one".to_vec()));
    assert_eq!(copy.get(Table::Users, "2").unwrap(), Some(b"two".to_vec()));
    drop(copy);
    drop(store);
    OpfsStore::delete(&name).await.unwrap();
  }

  /// Before the move the records are in IndexedDB, as before; reading creates nothing on OPFS.
  #[wasm_bindgen_test]
  async fn reads_indexed_db_before_the_move() {
    let name = database_name();
    let key = SymmetricKey::generate(&mut OsRng);
    let old = IndexedDbStore::open(&name, &key).await.unwrap();
    old.put(Table::Users, "1", b"one").unwrap();
    old.flush().await.unwrap();
    drop(old);

    let store = ServiceWorkerStore::open_for_service_worker(&name, &key)
      .await
      .unwrap();
    assert!(!store.is_on_opfs());
    assert_eq!(store.get(Table::Users, "1").unwrap(), Some(b"one".to_vec()));
    drop(store);
    assert!(!has_opfs_directory(&name).await);
    OpfsStore::delete(&name).await.unwrap();
  }

  /// Once the log exists, IndexedDB's records are stale (a move interrupted before it emptied
  /// them): the copy of the log wins.
  #[wasm_bindgen_test]
  async fn ignores_a_stale_indexed_db_copy() {
    let name = database_name();
    let key = SymmetricKey::generate(&mut OsRng);
    let (store, _) = OpfsStore::open(&name, &key).await.unwrap();
    store.put(Table::Users, "1", b"new").unwrap();
    store.flush().unwrap();
    drop(store);
    let stale = IndexedDbStore::open(&name, &key).await.unwrap();
    stale.put(Table::Users, "1", b"stale").unwrap();
    stale.flush().await.unwrap();
    drop(stale);

    let copy = ServiceWorkerStore::open_for_service_worker(&name, &key)
      .await
      .unwrap();
    assert!(copy.is_on_opfs());
    assert_eq!(copy.get(Table::Users, "1").unwrap(), Some(b"new".to_vec()));
    drop(copy);
    OpfsStore::delete(&name).await.unwrap();
  }

  /// A move interrupted before the log's header was written left files but no log: the records
  /// are still in IndexedDB.
  #[wasm_bindgen_test]
  async fn files_without_a_log_mean_indexed_db() {
    let name = database_name();
    let key = SymmetricKey::generate(&mut OsRng);
    let old = IndexedDbStore::open(&name, &key).await.unwrap();
    old.put(Table::Users, "1", b"one").unwrap();
    old.flush().await.unwrap();
    drop(old);
    for file in ["records-0.log", "records-1.log"] {
      write_opfs_file(&name, file, &[0x42; 64]).await;
    }

    let store = ServiceWorkerStore::open_for_service_worker(&name, &key)
      .await
      .unwrap();
    assert!(!store.is_on_opfs());
    assert_eq!(store.get(Table::Users, "1").unwrap(), Some(b"one".to_vec()));
    drop(store);
    OpfsStore::delete(&name).await.unwrap();
  }

  #[wasm_bindgen_test]
  async fn deleting_removes_the_log() {
    let name = database_name();
    let key = SymmetricKey::generate(&mut OsRng);
    let (store, _) = OpfsStore::open(&name, &key).await.unwrap();
    store.put(Table::Users, "1", b"one").unwrap();
    drop(store);
    assert!(has_opfs_directory(&name).await);
    OpfsStore::delete(&name).await.unwrap();
    assert!(!has_opfs_directory(&name).await);
  }
}

/// The web core's store type over either backend (issue #2363): the page opens it on IndexedDB,
/// the core's worker on OPFS.
mod browser_store {
  use super::*;

  #[wasm_bindgen_test]
  async fn the_worker_moves_the_store_and_the_page_can_tell() {
    let name = database_name();
    let key = SymmetricKey::generate(&mut OsRng);
    let page = BrowserStore::open_indexed_db_reporting(&name, &key, |_| ControlFlow::Continue(()))
      .await
      .unwrap();
    assert!(!page.is_on_opfs());
    page.put(Table::Users, "1", b"one").unwrap();
    page.flush().await.unwrap();
    drop(page);
    assert!(!OpfsStore::has_log(&name).await.unwrap());

    let (worker, opening) = BrowserStore::open_opfs_reporting(&name, &key, |_| ControlFlow::Continue(()))
      .await
      .unwrap();
    assert!(matches!(opening, OpfsOpening::Migrated { records } if records > 0));
    assert!(worker.is_on_opfs());
    assert_eq!(worker.get(Table::Users, "1").unwrap(), Some(b"one".to_vec()));
    let revision = worker.write_revision();
    worker.put(Table::Users, "2", b"two").unwrap();
    assert_ne!(worker.write_revision(), revision);
    // The page went away: the index is saved and the log flushed, so the next open reads no log.
    worker.write_now();
    drop(worker);
    assert!(OpfsStore::has_log(&name).await.unwrap());

    let (reopened, opening) = OpfsStore::open(&name, &key).await.unwrap();
    assert_eq!(opening, OpfsOpening::Opened);
    assert_eq!(reopened.get(Table::Users, "2").unwrap(), Some(b"two".to_vec()));
    drop(reopened);
    OpfsStore::delete(&name).await.unwrap();
    assert!(!OpfsStore::has_log(&name).await.unwrap());
  }

  /// The worker of a tab that handed over still holds the files: the next owner's open is refused
  /// until it let go, then opens what it wrote.
  #[wasm_bindgen_test]
  async fn the_next_owner_opens_once_the_last_one_let_go() {
    let name = database_name();
    let key = SymmetricKey::generate(&mut OsRng);
    let (first, _) = BrowserStore::open_opfs_reporting(&name, &key, |_| ControlFlow::Continue(()))
      .await
      .unwrap();
    first.put(Table::Users, "1", b"one").unwrap();
    first.flush().await.unwrap();
    let refused = BrowserStore::open_opfs_reporting(&name, &key, |_| ControlFlow::Continue(())).await;
    assert!(matches!(refused, Err(StoreError::InUse)));
    drop(first);
    let (next, opening) = BrowserStore::open_opfs_reporting(&name, &key, |_| ControlFlow::Continue(()))
      .await
      .unwrap();
    assert_eq!(opening, OpfsOpening::Opened);
    assert_eq!(next.get(Table::Users, "1").unwrap(), Some(b"one".to_vec()));
    drop(next);
    OpfsStore::delete(&name).await.unwrap();
  }

  /// A store whose directory exists but holds no log (a move that never wrote its header) has not
  /// moved: its records are still in IndexedDB.
  #[wasm_bindgen_test]
  async fn an_empty_log_is_no_move() {
    let name = database_name();
    write_opfs_file(&name, "records-0.log", b"").await;
    write_opfs_file(&name, "records-1.log", b"").await;
    assert!(!OpfsStore::has_log(&name).await.unwrap());
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

fn bodies(store: &impl LocalStore, conversation: i64) -> Vec<String> {
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

/// Whether OPFS holds a directory for the store `name`.
async fn has_opfs_directory(name: &str) -> bool {
  let Some(stores) = opfs_child(&opfs_root().await, "teamcraft-stores", false).await else {
    return false;
  };
  opfs_child(&stores, name, false).await.is_some()
}

/// Writes `bytes` as the file `file` of the store `name` on OPFS, as a crashed move might leave it.
async fn write_opfs_file(name: &str, file: &str, bytes: &[u8]) {
  let stores = opfs_child(&opfs_root().await, "teamcraft-stores", true)
    .await
    .unwrap();
  let directory = opfs_child(&stores, name, true).await.unwrap();
  let options = FileSystemGetFileOptions::new();
  options.set_create(true);
  let handle: FileSystemFileHandle = JsFuture::from(directory.get_file_handle_with_options(file, &options))
    .await
    .unwrap()
    .unchecked_into();
  let access: FileSystemSyncAccessHandle = JsFuture::from(handle.create_sync_access_handle())
    .await
    .unwrap()
    .unchecked_into();
  access.truncate_with_u32(0).unwrap();
  access.write_with_u8_array(bytes).unwrap();
  access.flush().unwrap();
  access.close();
}

async fn opfs_root() -> FileSystemDirectoryHandle {
  let scope: WorkerGlobalScope = js_sys::global().unchecked_into();
  JsFuture::from(scope.navigator().storage().get_directory())
    .await
    .unwrap()
    .unchecked_into()
}

async fn opfs_child(
  parent: &FileSystemDirectoryHandle,
  name: &str,
  is_created: bool,
) -> Option<FileSystemDirectoryHandle> {
  let options = FileSystemGetDirectoryOptions::new();
  options.set_create(is_created);
  JsFuture::from(parent.get_directory_handle_with_options(name, &options))
    .await
    .ok()
    .map(JsCast::unchecked_into)
}
