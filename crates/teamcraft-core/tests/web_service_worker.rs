//! The push path inside a real service worker (wasm-bindgen-test, headless Chromium): IndexedDB,
//! Web Crypto and timers in worker scope, and a preview from a store reopened the way the service
//! worker opens it. Showing the notification itself needs a permission headless tests don't have.
#![cfg(target_arch = "wasm32")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

mod common;

use std::rc::Rc;

use common::{PASSPHRASE, clock, options, settings};
use rand_core::{OsRng, RngCore};
use teamcraft_core::PendingEnrollment;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::{FakeServer, FakeServerApi};
use teamcraft_core::push_preview::{PreviewContent, PreviewGap, push_preview};
use teamcraft_core::store::{
  ConversationEvent, IndexedDbStore, LocalStore, MemoryStore, MessageContent, MessageFilter,
  browser_database_key,
};
use teamcraft_crypto::push::seal_wakeup;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{ListMessagesQuery, PushKind, PushProvider, PushWakeUp};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_service_worker);

#[wasm_bindgen_test]
async fn service_worker_previews_a_push_from_the_encrypted_store() {
  let server = FakeServer::new();
  let alice_api = server.add_user("Alice");
  let mut alice = PendingEnrollment::new()
    .complete(
      alice_api.clone(),
      Rc::new(MemoryStore::new()),
      clock(),
      settings(),
      options(PASSPHRASE),
    )
    .await
    .unwrap();
  let name = format!("teamcraft-sw-test-{}", OsRng.next_u64());
  let key = browser_database_key(&name).await.unwrap();
  let store = Rc::new(IndexedDbStore::open(&name, &key).await.unwrap());
  let bob_api = server.add_user("Bob");
  let mut bob = PendingEnrollment::new()
    .complete(
      bob_api.clone(),
      store.clone(),
      clock(),
      settings(),
      options(PASSPHRASE),
    )
    .await
    .unwrap();
  let (mut gateway, _) = bob.connect_gateway().await.unwrap();
  bob.register_push(PushProvider::WebPush, "{}").await.unwrap();

  let space = alice_api.list_spaces().await.unwrap().spaces[0].id;
  let channel = alice.create_channel(space, "worker").await.unwrap();
  alice.add_member(channel, bob.user_id()).await.unwrap();
  alice.send_text(channel, None, "from the worker").await.unwrap();
  while bob.pump(&mut gateway).await.unwrap() > 0 {}
  // The channel also holds the "member added" event; the push is about the text message.
  let server_id_of = |is_it: &dyn Fn(&MessageContent) -> bool| {
    store
      .messages(channel, MessageFilter::TopLevel)
      .unwrap()
      .into_iter()
      .find(|message| is_it(&message.content))
      .and_then(|message| message.maybe_server_id)
      .unwrap()
  };
  let message_id = server_id_of(&|content| matches!(content, MessageContent::Text { .. }));
  let bob_id = bob.user_id();
  let added_id = server_id_of(&|content| {
    matches!(
      content,
      MessageContent::Event {
        event: ConversationEvent::MemberAdded { user, .. },
      } if *user == bob_id
    )
  });
  store.flush().await.unwrap();
  let bob_device = bob.device_id();
  drop(bob);
  drop(store);

  // A message the store doesn't have yet: the worker fetches and decrypts it.
  alice
    .send_text(channel, None, "fetched by the worker")
    .await
    .unwrap();
  let newest = ListMessagesQuery {
    maybe_before: None,
    maybe_after: None,
    maybe_limit: Some(1),
  };
  let fetched_id = alice_api.list_messages(channel, &newest).await.unwrap().messages[0].id;

  // What arrives with the push, then the service worker's view of the store.
  let push_key = server.push_registration(bob_device).unwrap().push_key;
  let sealed = |message| {
    let wake_up = PushWakeUp {
      kind: PushKind::Message,
      conversation_id: channel,
      maybe_message_id: Some(message),
      maybe_sender_id: Some(alice.user_id()),
      is_urgent: false,
      maybe_thread_id: None,
      maybe_call_id: None,
      maybe_badge: None,
      maybe_sent_at: None,
    };
    seal_wakeup(
      &SymmetricKey::from_slice(push_key.as_slice()).unwrap(),
      &serde_json::to_vec(&wake_up).unwrap(),
      &mut OsRng,
    )
    .unwrap()
  };
  let key = browser_database_key(&name).await.unwrap();
  let reopened = IndexedDbStore::open_without_upgrade(&name, &key).await.unwrap();
  let preview = push_preview::<FakeServerApi>(&reopened, None, &sealed(message_id))
    .await
    .unwrap();
  assert_eq!(preview.content, PreviewContent::Text("from the worker".into()));
  assert_eq!(preview.maybe_conversation_name.as_deref(), Some("worker"));
  // Being added is the one change that is announced (#1452: read without the whole message).
  let added = push_preview::<FakeServerApi>(&reopened, None, &sealed(added_id))
    .await
    .unwrap();
  assert_eq!(added.maybe_gap, Some(PreviewGap::AddedYou));
  let fetched = push_preview(&reopened, Some(&bob_api), &sealed(fetched_id))
    .await
    .unwrap();
  assert_eq!(
    fetched.content,
    PreviewContent::Text("fetched by the worker".into())
  );
}
