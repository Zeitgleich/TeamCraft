//! Browser tests (headless Chromium via wasm-bindgen-test): the encrypted IndexedDB store, the
//! Web Crypto key source and the sync engine's crypto running as wasm. See the crate README for
//! how to run them.
#![cfg(target_arch = "wasm32")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

mod common;

use std::rc::Rc;

use common::{PASSPHRASE, clock, options, settings};
use openmls_traits::OpenMlsProvider as _;
use rand_core::{OsRng, RngCore};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::{
  IndexedDbStore, LocalStore, MessageContent, MessageFilter, StoreError, StoredMessage, Table,
  browser_database_key, browser_database_key_without_upgrade,
};
use teamcraft_core::{PendingEnrollment, mls_storage};
use teamcraft_crypto::push::{open_wakeup, seal_wakeup};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{ClientMessageId, ConversationId, Id, Snowflake, UserId};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn store_survives_reopening() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  store.put(Table::Users, "1", b"one").unwrap();
  store.put(Table::Users, "2", b"two").unwrap();
  store.delete(Table::Users, "2").unwrap();
  store.upsert_message(&message(7, 1, "hello")).unwrap();
  store.upsert_message(&message(7, 2, "world")).unwrap();
  store.upsert_message(&message(8, 3, "elsewhere")).unwrap();
  store.delete_messages(id(8)).unwrap();
  store.flush().await.unwrap();
  drop(store);

  let reopened = IndexedDbStore::open(&name, &key).await.unwrap();
  assert_eq!(
    reopened.list(Table::Users).unwrap(),
    vec![("1".to_string(), b"one".to_vec())]
  );
  let bodies: Vec<String> = reopened
    .messages(id(7), MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .map(|message| body(&message))
    .collect();
  assert_eq!(bodies, ["hello", "world"]);
  assert!(reopened.messages(id(8), MessageFilter::All).unwrap().is_empty());
}

#[wasm_bindgen_test]
async fn another_key_cannot_open_the_store() {
  let name = database_name();
  let store = IndexedDbStore::open(&name, &SymmetricKey::generate(&mut OsRng))
    .await
    .unwrap();
  store.put(Table::Account, "account", b"secret").unwrap();
  store.flush().await.unwrap();
  drop(store);

  let result = IndexedDbStore::open(&name, &SymmetricKey::generate(&mut OsRng)).await;
  assert!(matches!(result, Err(StoreError::WrongKey)));
}

/// e2ee.md §9.4: a newer version of the app upgrading the database closes this page's copy; writes
/// after that are not lost silently but reported until the page reloads.
#[wasm_bindgen_test]
async fn a_newer_version_closes_the_store_and_writes_report_it() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  store.put(Table::Users, "1", b"one").unwrap();
  store.flush().await.unwrap();

  open_with_newer_version(&name).await;
  store.put(Table::Users, "2", b"two").unwrap();
  assert!(matches!(store.flush().await, Err(StoreError::Closed)));
  // The cache still answers; the page asks the user to reload.
  assert_eq!(store.get(Table::Users, "2").unwrap(), Some(b"two".to_vec()));
}

/// e2ee.md §9.4: a write the browser refuses (quota) is reported by every flush and kept; once space
/// is free it goes through, and a reopened store has it.
#[wasm_bindgen_test]
async fn a_refused_write_is_reported_and_written_once_there_is_space() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  refuse_puts(true);
  store.put(Table::Users, "1", b"one").unwrap();
  assert!(matches!(store.flush().await, Err(StoreError::QuotaExceeded)));
  assert!(matches!(store.flush().await, Err(StoreError::QuotaExceeded)));

  refuse_puts(false);
  store.flush().await.unwrap();
  store.flush().await.unwrap();
  drop(store);
  let reopened = IndexedDbStore::open(&name, &key).await.unwrap();
  assert_eq!(reopened.get(Table::Users, "1").unwrap(), Some(b"one".to_vec()));
}

#[wasm_bindgen_test]
async fn browser_key_is_created_once_and_reused() {
  let name = database_name();
  let first = browser_database_key(&name).await.unwrap();
  let second = browser_database_key(&name).await.unwrap();
  assert_eq!(first.expose_secret(), second.expose_secret());
  assert_ne!(
    first.expose_secret(),
    browser_database_key(&database_name())
      .await
      .unwrap()
      .expose_secret()
  );

  // The store opens with it, also after reopening.
  let store = IndexedDbStore::open(&name, &first).await.unwrap();
  store.put(Table::Spaces, "1", b"space").unwrap();
  store.flush().await.unwrap();
  drop(store);
  let key = browser_database_key(&name).await.unwrap();
  let reopened = IndexedDbStore::open(&name, &key).await.unwrap();
  assert_eq!(reopened.get(Table::Spaces, "1").unwrap(), Some(b"space".to_vec()));
}

/// Enrollment (Argon2id, identity and device keys), MLS group setup, content keys and message
/// encryption all run as wasm; the receiver's store is IndexedDB and is read back after reopening.
#[wasm_bindgen_test]
async fn two_browser_clients_exchange_an_encrypted_message() {
  let server = FakeServer::new();
  let alice_api = server.add_user("Alice");
  let mut alice = PendingEnrollment::new()
    .complete(
      alice_api.clone(),
      Rc::new(teamcraft_core::store::MemoryStore::new()),
      clock(),
      settings(),
      options(PASSPHRASE),
    )
    .await
    .unwrap();
  let name = database_name();
  let key = browser_database_key(&name).await.unwrap();
  let bob_store = Rc::new(IndexedDbStore::open(&name, &key).await.unwrap());
  let mut bob = PendingEnrollment::new()
    .complete(
      server.add_user("Bob"),
      bob_store.clone(),
      clock(),
      settings(),
      options(PASSPHRASE),
    )
    .await
    .unwrap();
  let (mut bob_gateway, _) = bob.connect_gateway().await.unwrap();

  let space = alice_api.list_spaces().await.unwrap().spaces[0].id;
  let channel = alice.create_channel(space, "browser").await.unwrap();
  alice.add_member(channel, bob.user_id()).await.unwrap();
  alice.send_text(channel, None, "sealed in wasm").await.unwrap();
  for _ in 0..10 {
    if bob.pump(&mut bob_gateway).await.unwrap() == 0 {
      break;
    }
  }
  assert_eq!(texts(bob_store.as_ref(), channel), ["sealed in wasm"]);

  bob_store.flush().await.unwrap();
  drop(bob);
  drop(bob_store);
  let reopened = IndexedDbStore::open(&name, &key).await.unwrap();
  assert_eq!(texts(&reopened, channel), ["sealed in wasm"]);
}

/// Issue #424: a database of the first schema version opens with this one, and the MLS state a
/// version before 1.0.9 wrote as one record moves to one record per entry, which reopens.
#[wasm_bindgen_test]
async fn an_older_database_opens_and_its_mls_state_moves_to_entries() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  create_first_version(&name).await;
  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  let mut legacy = Vec::new();
  for chunk in [b"group".as_slice(), b"state".as_slice()] {
    legacy.extend_from_slice(&u32::try_from(chunk.len()).unwrap().to_be_bytes());
    legacy.extend_from_slice(chunk);
  }
  store.put(Table::MlsStorage, "provider", &legacy).unwrap();
  store.flush().await.unwrap();
  drop(store);

  for _ in 0..2 {
    let store = IndexedDbStore::open(&name, &key).await.unwrap();
    let (provider, _) = mls_storage::load_mls_storage(&store).unwrap();
    assert_eq!(
      provider.storage().values.read().unwrap().get(b"group".as_slice()),
      Some(&b"state".to_vec())
    );
    store.flush().await.unwrap();
    assert_eq!(store.get(Table::MlsStorage, "provider").unwrap(), None);
  }
}

/// Issue #424 review, M2: the service worker opens the store at the version it has and never
/// upgrades it, so a push never closes an older page that still owns the store; a page does.
#[wasm_bindgen_test]
async fn the_service_worker_never_upgrades_the_database() {
  let name = database_name();
  create_first_version(&name).await;
  let key = browser_database_key_without_upgrade(&name).await.unwrap();
  drop(IndexedDbStore::open_without_upgrade(&name, &key).await.unwrap());
  assert_eq!(database_version(&name).await, 1.0);

  let key = browser_database_key(&name).await.unwrap();
  drop(IndexedDbStore::open(&name, &key).await.unwrap());
  assert_eq!(database_version(&name).await, 2.0);
}

#[wasm_bindgen_test]
fn push_wake_up_round_trips_in_wasm() {
  let key = SymmetricKey::generate(&mut OsRng);
  let sealed = seal_wakeup(&key, b"{\"kind\":\"message\"}", &mut OsRng).unwrap();
  assert_eq!(
    open_wakeup(&key, &sealed).unwrap().expose_secret(),
    b"{\"kind\":\"message\"}"
  );
  assert!(open_wakeup(&SymmetricKey::generate(&mut OsRng), &sealed).is_err());
}

/// The text messages of a conversation; system events (a member joined) are left out.
fn texts(store: &impl LocalStore, conversation: ConversationId) -> Vec<String> {
  store
    .messages(conversation, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .filter_map(|message| match message.content {
      MessageContent::Text { body, .. } => Some(body),
      _ => None,
    })
    .collect()
}

fn body(message: &StoredMessage) -> String {
  match &message.content {
    MessageContent::Text { body, .. } => body.clone(),
    other => format!("{other:?}"),
  }
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
  }
}

fn id<T: Id>(raw: i64) -> T {
  T::from_snowflake(Snowflake::new(raw).unwrap())
}

/// Makes `IDBObjectStore.put` throw `QuotaExceededError`, as a full disk does, or restores it.
fn refuse_puts(is_refusing: bool) {
  let script = if is_refusing {
    "globalThis.__put = IDBObjectStore.prototype.put; IDBObjectStore.prototype.put = function () { \
     throw new DOMException('quota', 'QuotaExceededError'); };"
  } else {
    "IDBObjectStore.prototype.put = globalThis.__put;"
  };
  js_sys::eval(script).unwrap();
}

/// Opens `name` with a higher schema version, as a newer release of the app would.
async fn open_with_newer_version(name: &str) {
  use wasm_bindgen::JsCast;
  let factory: web_sys::IdbFactory = js_sys::Reflect::get(&js_sys::global(), &"indexedDB".into())
    .unwrap()
    .unchecked_into();
  let request = factory.open_with_u32(name, 1_000).unwrap();
  let opened = js_sys::Promise::new(&mut |resolve, reject| {
    request.set_onsuccess(Some(&resolve));
    request.set_onerror(Some(&reject));
  });
  let database: web_sys::IdbDatabase = wasm_bindgen_futures::JsFuture::from(opened)
    .await
    .map(|_| request.result().unwrap())
    .unwrap()
    .unchecked_into();
  database.close();
}

/// Creates `name` as the first schema version did: version 1 with the same object stores.
async fn create_first_version(name: &str) {
  use wasm_bindgen::JsCast;
  let script = format!(
    "new Promise((resolve, reject) => {{
      const request = indexedDB.open({name:?}, 1);
      request.onupgradeneeded = () => {{
        request.result.createObjectStore('records');
        request.result.createObjectStore('keys');
      }};
      request.onsuccess = () => {{ request.result.close(); resolve(); }};
      request.onerror = () => reject(request.error);
    }})"
  );
  let created: js_sys::Promise = js_sys::eval(&script).unwrap().unchecked_into();
  wasm_bindgen_futures::JsFuture::from(created).await.unwrap();
}

/// The version `name` has now (opened without one, so nothing is upgraded).
async fn database_version(name: &str) -> f64 {
  use wasm_bindgen::JsCast;
  let script = format!(
    "new Promise((resolve, reject) => {{
      const request = indexedDB.open({name:?});
      request.onsuccess = () => {{
        const version = request.result.version;
        request.result.close();
        resolve(version);
      }};
      request.onerror = () => reject(request.error);
    }})"
  );
  let opened: js_sys::Promise = js_sys::eval(&script).unwrap().unchecked_into();
  wasm_bindgen_futures::JsFuture::from(opened)
    .await
    .unwrap()
    .as_f64()
    .unwrap()
}

/// Each test gets its own database, so tests don't see each other's data.
fn database_name() -> String {
  format!("teamcraft-test-{}", OsRng.next_u64())
}
