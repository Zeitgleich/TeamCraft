//! Browser tests (headless Chromium via wasm-bindgen-test): the encrypted IndexedDB store, the
//! Web Crypto key source and the sync engine's crypto running as wasm. See the crate README for
//! how to run them.
#![cfg(target_arch = "wasm32")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

mod common;

use std::rc::Rc;

use common::{PASSPHRASE, clock, options, settings};
use rand_core::{OsRng, RngCore};
use teamcraft_core::PendingEnrollment;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::{
  IndexedDbStore, LocalStore, MessageContent, MessageFilter, StoreError, StoredMessage, Table,
  browser_database_key,
};
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

fn texts(store: &impl LocalStore, conversation: ConversationId) -> Vec<String> {
  store
    .messages(conversation, MessageFilter::TopLevel)
    .unwrap()
    .iter()
    .map(body)
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
    is_pinned: false,
  }
}

fn id<T: Id>(raw: i64) -> T {
  T::from_snowflake(Snowflake::new(raw).unwrap())
}

/// Each test gets its own database, so tests don't see each other's data.
fn database_name() -> String {
  format!("teamcraft-test-{}", OsRng.next_u64())
}
