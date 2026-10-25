//! Meeting chats: the server creates them with the meeting, without an encryption group. The
//! organizer's devices set the group up, the invitees join it by welcome, and a call never waits
//! for a group that isn't there (e2ee.md §4.6, "Meeting chats"; §8).
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use crate::common;

use common::{Peer, link_device, settings, settle};
#[cfg(feature = "ui")]
use teamcraft_client_api::failure::{CallFailure, FailureKind};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::error::CoreError;
use teamcraft_proto::{AddConversationMemberRequest, ConversationId};

const NOW_MS: i64 = 1_800_000_000_000;

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
  let mut mallory = Peer::enroll(&server, "Mallory").await;
  // The server lists Mallory as a member of the chat, but Alice invited only Bob.
  let chat = server.create_meeting_chat(
    alice.client.user_id(),
    &[bob.client.user_id(), mallory.client.user_id()],
  );
  let bob_id = bob.client.user_id();
  alice.client.set_up_meeting_chat(chat, &[bob_id]).await.unwrap();
  bob.client.full_sync().await.unwrap();
  mallory.client.full_sync().await.unwrap();
  assert_eq!(
    alice.client.verified_call_devices(chat).await.unwrap().len(),
    2,
    "Alice's and Bob's devices only"
  );
  assert_eq!(bob.client.prepare_call(chat).await.unwrap().devices.len(), 2);
  assert!(!mallory.client.has_group(chat).unwrap(), "Mallory got no welcome");
  assert!(mallory.client.prepare_call(chat).await.is_err());
}

#[tokio::test]
async fn an_instant_meeting_starts_with_the_organizer_alone() {
  let (server, mut alice, _bob) = two().await;
  let chat = server.create_meeting_chat(alice.client.user_id(), &[]);
  assert!(alice.client.set_up_meeting_chat(chat, &[]).await.unwrap());
  let prepared = alice.client.prepare_call(chat).await.unwrap();
  assert_eq!(prepared.devices.len(), 1);
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

mod organizer_devices {
  use super::*;

  #[tokio::test]
  async fn a_second_device_joins_a_scheduled_meetings_call() {
    let (server, mut alice, mut bob) = two().await;
    let chat = server.create_meeting_chat(alice.client.user_id(), &[bob.client.user_id()]);
    let bob_id = bob.client.user_id();
    alice.client.set_up_meeting_chat(chat, &[bob_id]).await.unwrap();
    bob.client.full_sync().await.unwrap();
    let mut laptop = link_device(&mut alice).await;
    laptop.full_sync().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let prepared = laptop.prepare_call(chat).await.unwrap();
    assert_eq!(prepared.devices.len(), 3, "both of Alice's devices and Bob's");
  }

  #[tokio::test]
  async fn a_second_device_joins_an_instant_meeting_that_holds_only_the_organizer() {
    let (server, mut alice, _bob) = two().await;
    let mut laptop = link_device(&mut alice).await;
    let chat = server.create_meeting_chat(alice.client.user_id(), &[]);
    // Before the phone set it up, the laptop waits: it never founds a second group.
    assert!(matches!(
      laptop.prepare_call(chat).await,
      Err(CoreError::CallEncryptionPending(_))
    ));
    assert!(!laptop.has_group(chat).unwrap());

    assert!(alice.client.set_up_meeting_chat(chat, &[]).await.unwrap());
    // The laptop asked to be added when it found nothing; the phone answers that on its sync.
    alice.client.full_sync().await.unwrap();
    let prepared = laptop.prepare_call(chat).await.unwrap();
    assert_eq!(
      prepared.devices.len(),
      2,
      "the phone and the laptop, in one group"
    );
    alice.client.full_sync().await.unwrap();
    assert_eq!(
      alice.client.mls_epoch(chat).unwrap(),
      laptop.mls_epoch(chat).unwrap()
    );
  }

  #[tokio::test]
  async fn a_second_setup_on_the_other_device_joins_instead_of_founding_again() {
    let (server, mut alice, _bob) = two().await;
    let mut laptop = link_device(&mut alice).await;
    let chat = server.create_meeting_chat(alice.client.user_id(), &[]);
    alice.client.set_up_meeting_chat(chat, &[]).await.unwrap();
    laptop.set_up_meeting_chat(chat, &[]).await.unwrap();
    alice.client.full_sync().await.unwrap();
    assert_eq!(
      alice.client.mls_epoch(chat).unwrap(),
      laptop.mls_epoch(chat).unwrap(),
      "one group"
    );
    assert_eq!(laptop.prepare_call(chat).await.unwrap().devices.len(), 2);
  }
}

#[tokio::test]
async fn the_hosts_admit_lets_somebody_into_the_chat() {
  let (server, mut alice, mut bob) = two().await;
  let chat = server.create_meeting_chat(alice.client.user_id(), &[]);
  alice.client.set_up_meeting_chat(chat, &[]).await.unwrap();
  let bob_id = bob.client.user_id();
  // The server admits Bob from the lobby (he becomes a member of the chat) ...
  alice
    .api
    .add_member(
      chat,
      &AddConversationMemberRequest {
        user_id: bob_id,
        maybe_role: None,
      },
    )
    .await
    .unwrap();
  bob.client.full_sync().await.unwrap();
  assert!(
    !bob.client.has_group(chat).unwrap(),
    "not on the server's word alone"
  );
  // ... and the host's own decision lets him into the encryption.
  alice.client.let_into_meeting_chat(chat, bob_id).await.unwrap();
  bob.client.full_sync().await.unwrap();
  assert_eq!(bob.client.prepare_call(chat).await.unwrap().devices.len(), 2);
}

#[tokio::test]
async fn a_setup_cut_off_before_its_first_request_is_finished_on_a_later_sync() {
  let server = FakeServer::new();
  let clock = Arc::new(ManualClock::new(NOW_MS));
  let mut alice = Peer::enroll_on_clock(
    server.add_user("Alice"),
    "Alice",
    settings(),
    Box::new(clock.clone()),
  )
  .await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let chat = server.create_meeting_chat(alice.client.user_id(), &[bob.client.user_id()]);
  let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
  server.set_offline(alice_id, true);
  assert!(alice.client.set_up_meeting_chat(chat, &[bob_id]).await.is_err());
  server.set_offline(alice_id, false);

  clock.advance(i64::try_from(settings().rejoin_retry.as_millis()).unwrap());
  alice.client.full_sync().await.unwrap();
  assert!(
    alice.client.has_group(chat).unwrap(),
    "finished from the setup record"
  );
  bob.client.full_sync().await.unwrap();
  assert_eq!(bob.client.prepare_call(chat).await.unwrap().devices.len(), 2);
}

#[tokio::test]
async fn an_admitted_user_without_a_device_is_not_let_in_later_unattended() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  // A user with no device yet (a guest before #446), whom the server could give one later.
  let carol_api = server.add_user("Carol");
  let carol_id = carol_api.user_id();
  let chat = server.create_meeting_chat(alice.client.user_id(), &[carol_id]);
  alice.client.set_up_meeting_chat(chat, &[]).await.unwrap();
  alice.client.let_into_meeting_chat(chat, carol_id).await.unwrap();

  let mut carol = Peer::enroll_as(carol_api, "Carol").await;
  alice.client.full_sync().await.unwrap();
  carol.client.full_sync().await.unwrap();
  assert!(
    !carol.client.has_group(chat).unwrap(),
    "a device that came later isn't added"
  );
  assert_eq!(alice.client.verified_call_devices(chat).await.unwrap().len(), 1);
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
