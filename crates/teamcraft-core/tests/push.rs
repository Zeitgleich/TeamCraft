//! Push previews (e2ee.md §7): what a notification process shows, from the sealed wake-up and the
//! local store, fetching the message when it may.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Peer, settle};
use teamcraft_core::api::fake::{FakeServer, FakeServerApi};
use teamcraft_core::push_preview::{PreviewContent, push_preview};
use teamcraft_core::store::{LocalStore, MessageFilter};
use teamcraft_crypto::push::seal_wakeup;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{ConversationId, MessageId, PushKind, PushProvider, PushWakeUp};

#[tokio::test]
async fn preview_fetches_and_decrypts_a_message_the_app_has_not_seen() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let message = alice_says(&mut alice, channel, "lunch at noon?").await;
  let sealed = wake_up_for(&server, &bob, &alice, channel, message);

  // Bob's app hasn't processed its gateway, so only the fetch can show the text.
  let stored_before = bob.store.messages(channel, MessageFilter::All).unwrap().len();
  let preview = push_preview(bob.store.as_ref(), Some(&bob.api), &sealed)
    .await
    .unwrap();
  assert_eq!(preview.content, PreviewContent::Text("lunch at noon?".into()));
  assert_eq!(preview.maybe_conversation_name.as_deref(), Some("planning"));
  assert_eq!(preview.maybe_sender_name.as_deref(), Some("Alice"));
  assert!(!preview.is_direct);
  // A preview only reads: the store didn't change.
  assert_eq!(
    bob.store.messages(channel, MessageFilter::All).unwrap().len(),
    stored_before
  );
  settle(&mut [&mut bob]).await;
}

#[tokio::test]
async fn without_a_session_only_names_are_shown_unless_the_store_has_the_message() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let message = alice_says(&mut alice, channel, "see you").await;
  let sealed = wake_up_for(&server, &bob, &alice, channel, message);

  let preview = push_preview::<FakeServerApi>(bob.store.as_ref(), None, &sealed)
    .await
    .unwrap();
  assert_eq!(preview.content, PreviewContent::Unavailable);
  assert_eq!(preview.maybe_sender_name.as_deref(), Some("Alice"));

  settle(&mut [&mut alice, &mut bob]).await;
  let preview = push_preview::<FakeServerApi>(bob.store.as_ref(), None, &sealed)
    .await
    .unwrap();
  assert_eq!(preview.content, PreviewContent::Text("see you".into()));
}

#[tokio::test]
async fn a_payload_sealed_with_another_key_is_rejected() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  alice_says(&mut alice, channel, "hi").await;
  let foreign = seal_wakeup(
    &SymmetricKey::generate(&mut rand_core::OsRng),
    b"{}",
    &mut rand_core::OsRng,
  )
  .unwrap();
  assert!(
    push_preview(bob.store.as_ref(), Some(&bob.api), &foreign)
      .await
      .is_err()
  );
  settle(&mut [&mut bob]).await;
}

async fn channel_with_two() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "planning")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  bob
    .client
    .register_push(PushProvider::WebPush, "{}")
    .await
    .unwrap();
  (server, alice, bob, channel)
}

async fn alice_says(alice: &mut Peer, channel: ConversationId, text: &str) -> MessageId {
  alice.client.send_text(channel, None, text).await.unwrap();
  alice
    .store
    .messages(channel, MessageFilter::TopLevel)
    .unwrap()
    .last()
    .and_then(|message| message.maybe_server_id)
    .unwrap()
}

/// Seals what the server would push for Alice's message.
fn wake_up_for(
  server: &FakeServer,
  bob: &Peer,
  alice: &Peer,
  channel: ConversationId,
  message: MessageId,
) -> Vec<u8> {
  let registration = server.push_registration(bob.client.device_id()).unwrap();
  let key = SymmetricKey::from_slice(registration.push_key.as_slice()).unwrap();
  let wake_up = PushWakeUp {
    kind: PushKind::Message,
    conversation_id: channel,
    maybe_message_id: Some(message),
    maybe_sender_id: Some(alice.client.user_id()),
    is_urgent: false,
  };
  seal_wakeup(
    &key,
    &serde_json::to_vec(&wake_up).unwrap(),
    &mut rand_core::OsRng,
  )
  .unwrap()
}
