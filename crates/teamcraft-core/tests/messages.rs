//! Attachments, edits and deletes, mentions, typing and push through the fake server.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Peer, settle};
use teamcraft_core::CoreError;
use teamcraft_core::api::GatewayConnection;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::OutgoingFile;
use teamcraft_core::store::{LocalStore, MessageContent, MessageFilter};
use teamcraft_crypto::push::seal_wakeup;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{ConversationId, PushKind, PushProvider, PushWakeUp};

#[tokio::test]
async fn files_are_encrypted_end_to_end() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let content = b"quarterly numbers: confidential".to_vec();
  let file = OutgoingFile {
    file_name: "numbers.txt".into(),
    media_type: "text/plain".into(),
    content: content.clone(),
  };
  let message = alice.client.send_file(channel, None, file).await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  let downloaded = bob.client.fetch_file(channel, message).await.unwrap();
  assert_eq!(downloaded.expose_secret(), content.as_slice());
  for blob in server.stored_blobs() {
    assert!(!blob.windows(9).any(|window| window == b"quarterly"));
  }

  // A swapped or corrupted blob is refused before decryption.
  server.tamper_blobs();
  assert!(matches!(
    bob.client.fetch_file(channel, message).await,
    Err(CoreError::Corrupt(_))
  ));
}

#[tokio::test]
async fn authors_edit_and_delete_their_own_messages_only() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  let original = alice
    .client
    .send_text(channel, None, "Meeting at 10")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  alice
    .client
    .edit_message(channel, original, "Meeting at 11")
    .await
    .unwrap();
  bob
    .client
    .edit_message(channel, original, "Meeting cancelled")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  for peer in [&alice, &bob] {
    let stored = peer.store.message(channel, original).unwrap().unwrap();
    assert!(stored.is_edited);
    assert!(
      matches!(&stored.content, MessageContent::Text { body, .. } if body == "Meeting at 11"),
      "{}",
      peer.name
    );
  }

  alice.client.delete_message(channel, original).await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  let stored = bob.store.message(channel, original).unwrap().unwrap();
  assert_eq!(stored.content, MessageContent::Deleted);
  assert!(bob.texts(channel).is_empty());
  // Edits and deletes themselves aren't listed.
  assert_eq!(
    bob
      .store
      .messages(channel, MessageFilter::TopLevel)
      .unwrap()
      .len(),
    1
  );
}

#[tokio::test]
async fn mentions_reach_the_server_as_metadata() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let bob_id = bob.client.user_id();
  alice
    .client
    .send_text_mentioning(channel, None, "@bob please review", vec![bob_id])
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  assert_eq!(server.stored_mentions(), vec![vec![bob_id]]);
  let received = bob.store.messages(channel, MessageFilter::TopLevel).unwrap();
  assert!(matches!(&received[0].content, MessageContent::Text { mentions, .. } if *mentions == vec![bob_id]));
}

#[tokio::test]
async fn typing_shows_until_the_message_arrives() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  bob
    .gateway
    .send(&common::TestClient::typing_frame(channel))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(alice.client.typing_users(channel), vec![bob.client.user_id()]);

  bob.client.send_text(channel, None, "done typing").await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert!(alice.client.typing_users(channel).is_empty());
}

#[tokio::test]
async fn push_payloads_open_with_the_registered_key() {
  let (server, alice, _bob, channel) = channel_with_two().await;
  let mut alice = alice;
  alice
    .client
    .register_push(PushProvider::Fcm, "fcm-token")
    .await
    .unwrap();
  let registration = server.push_registration(alice.client.device_id()).unwrap();
  assert_eq!(registration.token, "fcm-token");

  // What the server would send through FCM.
  let wake_up = PushWakeUp {
    kind: PushKind::Mention,
    conversation_id: channel,
    maybe_message_id: None,
    maybe_sender_id: None,
    is_urgent: false,
  };
  let key = SymmetricKey::from_slice(registration.push_key.as_slice()).unwrap();
  let sealed = seal_wakeup(
    &key,
    &serde_json::to_vec(&wake_up).unwrap(),
    &mut rand_core::OsRng,
  )
  .unwrap();
  assert_eq!(alice.client.open_push(&sealed).unwrap(), wake_up);

  let foreign = seal_wakeup(
    &SymmetricKey::generate(&mut rand_core::OsRng),
    b"{}",
    &mut rand_core::OsRng,
  )
  .unwrap();
  assert!(alice.client.open_push(&foreign).is_err());
}

async fn channel_with_two() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "files")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  (server, alice, bob, channel)
}
