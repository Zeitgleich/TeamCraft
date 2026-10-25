//! Meeting chats: the server creates them with the meeting, without an encryption group. The
//! organizer's devices set the group up, the invitees join it by welcome, and a call never waits
//! for a group that isn't there (e2ee.md §4.6, "Meeting chats"; §8).
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, link_device, settle};
#[cfg(feature = "ui")]
use teamcraft_client_api::failure::{CallFailure, FailureKind};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::error::CoreError;
use teamcraft_proto::ConversationId;

#[tokio::test]
async fn the_organizer_sets_the_chat_up_and_both_can_start_the_call() {
  let (server, mut alice, mut bob) = two().await;
  let chat = server.create_meeting_chat(alice.client.user_id(), &[bob.client.user_id()]);
  let bob_id = bob.client.user_id();
  assert!(alice.client.set_up_meeting_chat(chat, &[bob_id]).await.unwrap());
  bob.client.full_sync().await.unwrap();
  assert!(
    bob.client.has_group(chat).unwrap(),
    "the invitee joined by welcome"
  );

  let alices = alice.client.prepare_call(chat).await.unwrap();
  let bobs = bob.client.prepare_call(chat).await.unwrap();
  assert_eq!(alices.devices.len(), 2);
  assert_eq!(bobs.devices.len(), 2);
  assert_talks(&mut alice, &mut bob, chat).await;
}

#[tokio::test]
async fn the_server_cannot_slip_somebody_into_the_group() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mallory = Peer::enroll(&server, "Mallory").await;
  // The server lists Mallory as a member of the chat, but Alice invited only Bob.
  let chat = server.create_meeting_chat(
    alice.client.user_id(),
    &[bob.client.user_id(), mallory.client.user_id()],
  );
  let bob_id = bob.client.user_id();
  alice.client.set_up_meeting_chat(chat, &[bob_id]).await.unwrap();
  bob.client.full_sync().await.unwrap();
  let prepared = bob.client.prepare_call(chat).await.unwrap();
  assert_eq!(prepared.devices.len(), 2, "Alice's and Bob's devices only");
}

#[tokio::test]
async fn an_instant_meetings_call_starts_before_its_setup_arrives() {
  let (server, mut alice, _bob) = two().await;
  let chat = server.create_meeting_chat(alice.client.user_id(), &[]);
  let prepared = alice.client.prepare_call(chat).await.unwrap();
  assert_eq!(
    prepared.devices.len(),
    1,
    "the organizer founded it with themselves"
  );
  // The setup the UI sends afterwards finds the group and changes nothing.
  assert!(alice.client.set_up_meeting_chat(chat, &[]).await.unwrap());
  assert_eq!(alice.client.mls_epoch(chat).unwrap(), Some(0));
}

#[tokio::test]
async fn an_invitee_is_told_clearly_until_the_organizer_set_the_chat_up() {
  let (server, mut alice, mut bob) = two().await;
  let chat = server.create_meeting_chat(alice.client.user_id(), &[bob.client.user_id()]);
  // The organizer's app hasn't set the chat up yet (offline, or a meeting from before this).
  let Err(error) = bob.client.prepare_call(chat).await else {
    panic!("the call started without a group");
  };
  assert!(
    matches!(error, CoreError::CallEncryptionPending(id) if id == chat),
    "{error}"
  );
  // Worded by the UI, with a retry, instead of a call that waits for keys.
  #[cfg(feature = "ui")]
  assert_eq!(
    error.to_failure().kind,
    FailureKind::Call(CallFailure::EncryptionPending)
  );
  assert!(!bob.client.has_group(chat).unwrap(), "an invitee never founds it");

  let bob_id = bob.client.user_id();
  alice.client.set_up_meeting_chat(chat, &[bob_id]).await.unwrap();
  let prepared = bob.client.prepare_call(chat).await.unwrap();
  assert_eq!(prepared.devices.len(), 2, "the retry gets in");
}

#[tokio::test]
async fn a_second_device_of_the_organizer_joins_the_chat_and_its_call() {
  let (server, mut alice, mut bob) = two().await;
  let chat = server.create_meeting_chat(alice.client.user_id(), &[bob.client.user_id()]);
  let bob_id = bob.client.user_id();
  alice.client.set_up_meeting_chat(chat, &[bob_id]).await.unwrap();
  bob.client.full_sync().await.unwrap();
  let mut laptop = link_device(&mut alice).await;
  laptop.full_sync().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  laptop.full_sync().await.unwrap();
  let prepared = laptop.prepare_call(chat).await.unwrap();
  assert_eq!(prepared.devices.len(), 3, "both of Alice's devices and Bob's");
}

async fn two() -> (FakeServer, Peer, Peer) {
  let server = FakeServer::new();
  let alice = Peer::enroll(&server, "Alice").await;
  let bob = Peer::enroll(&server, "Bob").await;
  (server, alice, bob)
}

async fn assert_talks(alice: &mut Peer, bob: &mut Peer, chat: ConversationId) {
  alice.client.send_text(chat, None, "hi Bob").await.unwrap();
  bob.client.send_text(chat, None, "hi Alice").await.unwrap();
  settle(&mut [&mut *alice, &mut *bob]).await;
  assert_eq!(alice.texts(chat), vec!["hi Bob", "hi Alice"]);
  assert_eq!(bob.texts(chat), vec!["hi Bob", "hi Alice"]);
}
