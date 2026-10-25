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
use teamcraft_core::client::call_move_announcement;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::error::CoreError;
use teamcraft_proto::{AddConversationMemberRequest, ConversationId, GatewayEvent, IdempotencyKey, UserId};

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

/// An instant meeting nobody joins stays at epoch 0: its group information is looked up until the
/// server has it, then not again on every sync (#494).
#[tokio::test]
async fn a_meeting_nobody_joins_costs_no_group_info_request_per_sync() {
  let (server, mut alice, _bob) = two().await;
  let chat = server.create_meeting_chat(alice.client.user_id(), &[]);
  alice.client.set_up_meeting_chat(chat, &[]).await.unwrap();
  alice.client.full_sync().await.unwrap();

  // The server lost the information; a device that remembers it was published doesn't ask again.
  server.forget_group_infos();
  alice.client.full_sync().await.unwrap();

  let missing = alice.api.mls_group_info(chat).await;
  assert!(missing.is_err_and(|error| error.is_not_found()));
}

/// A meeting from before 1.0.3 has a chat without a group: the organizer's device founds it once it
/// opens the meeting, and asking again costs nothing (#504).
#[tokio::test]
async fn the_organizer_founds_the_group_of_an_old_meeting_once() {
  let (server, mut alice, mut bob) = two().await;
  let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
  let chat = server.create_meeting_chat(alice_id, &[bob_id]);
  assert!(matches!(
    bob.client.prepare_call(chat).await,
    Err(CoreError::CallEncryptionPending(_))
  ));

  assert!(alice.client.set_up_meeting_chat(chat, &[bob_id]).await.unwrap());
  let calls_after_setup = server.rest_call_count(alice_id);
  assert!(alice.client.set_up_meeting_chat(chat, &[bob_id]).await.unwrap());
  assert_eq!(server.rest_call_count(alice_id), calls_after_setup, "no request");

  bob.client.full_sync().await.unwrap();
  assert_eq!(bob.client.prepare_call(chat).await.unwrap().devices.len(), 2);
}

/// Two devices of the organizer open an old meeting at the same moment: both found a group, the
/// server keeps the first one's information, and the other device joins that group (#504).
#[tokio::test]
async fn two_organizer_devices_founding_at_once_end_up_in_one_group() {
  let (server, mut alice, _bob) = two().await;
  let mut laptop = link_device(&mut alice).await;
  let chat = server.create_meeting_chat(alice.client.user_id(), &[]);
  alice.client.set_up_meeting_chat(chat, &[]).await.unwrap();
  // The laptop looked before the phone published: it founds its own group and publishes second.
  server.hide_group_info_reads(chat, 2);
  laptop.set_up_meeting_chat(chat, &[]).await.unwrap();

  alice.client.full_sync().await.unwrap();
  laptop.full_sync().await.unwrap();
  assert_eq!(
    alice.client.mls_epoch(chat).unwrap(),
    laptop.mls_epoch(chat).unwrap(),
    "one group"
  );
  assert_eq!(laptop.prepare_call(chat).await.unwrap().devices.len(), 2);
  assert_eq!(alice.client.prepare_call(chat).await.unwrap().devices.len(), 2);
}

/// People invited later (a meeting edit) go into the group by the organizer's signed add, the
/// ones taken off the list out of it; the server's member list decides nothing (#475).
#[tokio::test]
async fn people_invited_later_join_the_group_and_removed_ones_leave_it() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let (alice_id, bob_id, carol_id) = (
    alice.client.user_id(),
    bob.client.user_id(),
    carol.client.user_id(),
  );
  let chat = server.create_meeting_chat(alice_id, &[bob_id, carol_id]);
  alice.client.set_up_meeting_chat(chat, &[bob_id]).await.unwrap();
  carol.client.full_sync().await.unwrap();
  assert!(!carol.client.has_group(chat).unwrap(), "not invited at first");

  assert!(
    alice
      .client
      .update_meeting_chat_members(chat, &[bob_id, carol_id], &[])
      .await
      .unwrap()
  );
  carol.client.full_sync().await.unwrap();
  assert_eq!(carol.client.prepare_call(chat).await.unwrap().devices.len(), 3);

  alice
    .client
    .update_meeting_chat_members(chat, &[carol_id], &[bob_id])
    .await
    .unwrap();
  assert_eq!(
    alice.client.verified_call_devices(chat).await.unwrap().len(),
    2,
    "Alice and Carol"
  );
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

/// A DM call that grows moves into a meeting: its group has the people in the call and the added
/// ones, from the user's own action and the DM's group, never the server's lists (docs/meetings.md,
/// "A call becomes a meeting").
mod call_moves {
  use super::*;

  #[tokio::test]
  async fn the_meeting_has_exactly_the_people_in_the_call_and_the_added_ones() {
    let (server, mut alice, mut bob, mut carol, mallory) = dm_call().await;
    let dm = in_dm_call(&mut alice, &mut bob).await;
    let (alice_id, bob_id, carol_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      carol.client.user_id(),
    );
    // The server lists Mallory in the meeting chat, too.
    let meeting = server.create_meeting_chat(alice_id, &[bob_id, carol_id, mallory]);

    alice
      .client
      .move_call_to_meeting(dm, meeting, &[alice_id, bob_id], &[carol_id])
      .await
      .unwrap();

    bob.client.full_sync().await.unwrap();
    carol.client.full_sync().await.unwrap();
    for peer in [&mut bob, &mut carol] {
      let prepared = peer.client.prepare_call(meeting).await.unwrap();
      assert_eq!(prepared.devices.len(), 3, "Alice, Bob and Carol only");
    }
    let moves = server.call_moves();
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].0, dm);
    assert_eq!(moves[0].1.to_conversation_id, meeting);
    assert_eq!(
      moves[0].1.ring_user_ids,
      vec![carol_id],
      "only the added person rings"
    );
  }

  #[tokio::test]
  async fn the_dm_stays_as_it_was() {
    let (server, mut alice, mut bob, mut carol, _) = dm_call().await;
    let dm = in_dm_call(&mut alice, &mut bob).await;
    alice.client.send_text(dm, None, "before the call").await.unwrap();
    let epoch = alice.client.mls_epoch(dm).unwrap();
    let (alice_id, bob_id, carol_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      carol.client.user_id(),
    );
    let meeting = server.create_meeting_chat(alice_id, &[bob_id, carol_id]);

    alice
      .client
      .move_call_to_meeting(dm, meeting, &[alice_id, bob_id], &[carol_id])
      .await
      .unwrap();

    carol.client.full_sync().await.unwrap();
    assert_eq!(alice.client.mls_epoch(dm).unwrap(), epoch, "no commit in the DM");
    assert!(
      !carol.client.has_group(dm).unwrap(),
      "the DM's history isn't shared"
    );
    assert!(carol.texts(meeting).is_empty());
  }

  #[tokio::test]
  async fn people_the_call_lists_outside_the_dms_group_are_left_out() {
    let (server, mut alice, mut bob, mut carol, mallory) = dm_call().await;
    let dm = in_dm_call(&mut alice, &mut bob).await;
    let (alice_id, bob_id, carol_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      carol.client.user_id(),
    );
    let meeting = server.create_meeting_chat(alice_id, &[bob_id, carol_id, mallory]);

    // A roster that names Mallory in the call doesn't get her into the meeting's group.
    alice
      .client
      .move_call_to_meeting(dm, meeting, &[alice_id, bob_id, mallory], &[carol_id])
      .await
      .unwrap();

    carol.client.full_sync().await.unwrap();
    let prepared = carol.client.prepare_call(meeting).await.unwrap();
    assert_eq!(prepared.devices.len(), 3, "Alice, Bob and Carol only");
  }

  #[tokio::test]
  async fn a_lost_answer_is_retried_into_the_same_group() {
    let (server, mut alice, mut bob, mut carol, _) = dm_call().await;
    let dm = in_dm_call(&mut alice, &mut bob).await;
    let (alice_id, bob_id, carol_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      carol.client.user_id(),
    );
    let meeting = server.create_meeting_chat(alice_id, &[bob_id, carol_id]);
    alice
      .client
      .move_call_to_meeting(dm, meeting, &[alice_id, bob_id], &[carol_id])
      .await
      .unwrap();
    let epoch = alice.client.mls_epoch(meeting).unwrap();

    alice
      .client
      .move_call_to_meeting(dm, meeting, &[alice_id, bob_id], &[carol_id])
      .await
      .unwrap();

    assert_eq!(
      alice.client.mls_epoch(meeting).unwrap(),
      epoch,
      "nobody added twice"
    );
    assert_eq!(server.call_moves().len(), 2, "everybody is told again");
    carol.client.full_sync().await.unwrap();
    assert_eq!(carol.client.prepare_call(meeting).await.unwrap().devices.len(), 3);
  }

  #[tokio::test]
  async fn the_others_follow_only_a_move_announced_in_the_dms_group() {
    let (server, mut alice, mut bob, carol, _) = dm_call().await;
    let dm = in_dm_call(&mut alice, &mut bob).await;
    let (alice_id, bob_id, carol_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      carol.client.user_id(),
    );
    let meeting = server.create_meeting_chat(alice_id, &[bob_id, carol_id]);
    alice
      .client
      .move_call_to_meeting(dm, meeting, &[alice_id, bob_id], &[carol_id])
      .await
      .unwrap();

    bob.client.full_sync().await.unwrap();
    bob.client.accept_call_move(dm, meeting).await.unwrap();
    assert!(bob.client.has_group(meeting).unwrap());
  }

  #[tokio::test]
  async fn a_move_the_server_made_up_is_refused() {
    let (server, mut alice, mut bob, _, mallory) = dm_call().await;
    let dm = in_dm_call(&mut alice, &mut bob).await;
    // A room Mallory organizes, which nobody in the DM announced.
    let elsewhere = server.create_meeting_chat(mallory, &[bob.client.user_id()]);

    let Err(error) = bob.client.accept_call_move(dm, elsewhere).await else {
      panic!("followed a move nobody announced");
    };
    assert!(matches!(error, CoreError::CallMoveRefused { .. }), "{error}");
    #[cfg(feature = "ui")]
    assert_eq!(
      error.to_failure().kind,
      FailureKind::Call(CallFailure::MoveRefused)
    );
  }

  #[tokio::test]
  async fn a_room_holding_somebody_the_move_didnt_name_is_refused() {
    let (server, mut alice, mut bob, mut carol, _) = dm_call().await;
    let dm = in_dm_call(&mut alice, &mut bob).await;
    let dave = Peer::enroll(&server, "Dave").await;
    let (alice_id, bob_id, carol_id, dave_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      carol.client.user_id(),
      dave.client.user_id(),
    );
    let meeting = server.create_meeting_chat(alice_id, &[bob_id, carol_id, dave_id]);
    alice
      .client
      .move_call_to_meeting(dm, meeting, &[alice_id, bob_id], &[carol_id])
      .await
      .unwrap();
    // Somebody the announcement didn't name is in the room by the time Bob follows.
    alice
      .client
      .let_into_meeting_chat(meeting, dave_id)
      .await
      .unwrap();
    carol.client.full_sync().await.unwrap();

    bob.client.full_sync().await.unwrap();
    let Err(error) = bob.client.accept_call_move(dm, meeting).await else {
      panic!("followed into a room with somebody unnamed");
    };
    assert!(matches!(error, CoreError::CallMoveRefused { .. }), "{error}");
  }

  #[tokio::test]
  async fn adding_people_starts_one_room_also_when_retried() {
    let (_server, mut alice, mut bob, mut carol, _) = dm_call().await;
    let dm = in_dm_call(&mut alice, &mut bob).await;
    let (alice_id, bob_id, carol_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      carol.client.user_id(),
    );
    let key = IdempotencyKey::random();

    let room = alice
      .client
      .add_people_to_call(dm, &[alice_id, bob_id], &[carol_id], None, key)
      .await
      .unwrap();
    let again = alice
      .client
      .add_people_to_call(dm, &[alice_id, bob_id], &[carol_id], None, key)
      .await
      .unwrap();

    assert_eq!(room, again, "a retry gets the same room");
    carol.client.full_sync().await.unwrap();
    assert_eq!(carol.client.prepare_call(room).await.unwrap().devices.len(), 3);
    bob.client.full_sync().await.unwrap();
    bob.client.accept_call_move(dm, room).await.unwrap();
  }

  #[tokio::test]
  async fn a_move_is_followed_once() {
    let (server, mut alice, mut bob, _, _) = dm_call().await;
    let dm = in_dm_call(&mut alice, &mut bob).await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    let meeting = server.create_meeting_chat(alice_id, &[bob_id]);
    alice
      .client
      .move_call_to_meeting(dm, meeting, &[alice_id, bob_id], &[])
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();
    bob.client.accept_call_move(dm, meeting).await.unwrap();

    let again = bob.client.accept_call_move(dm, meeting).await;

    assert!(
      matches!(again, Err(CoreError::CallMoveRefused { .. })),
      "{again:?}"
    );
  }

  /// The server replays an old move in a later call: neither the mover's device (which would
  /// follow its own move without a tap) nor anybody else's moves.
  #[tokio::test]
  async fn a_move_of_an_earlier_call_moves_nothing_in_a_later_one() {
    let (server, mut alice, mut bob, _, _) = dm_call().await;
    let dm = in_dm_call(&mut alice, &mut bob).await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    let meeting = server.create_meeting_chat(alice_id, &[bob_id]);
    alice
      .client
      .move_call_to_meeting(dm, meeting, &[alice_id, bob_id], &[])
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();
    // Nobody followed; the call ends, and a new private call starts later.
    server.end_call(dm);
    alice.client.prepare_call(dm).await.unwrap();
    bob.client.prepare_call(dm).await.unwrap();

    for peer in [&mut alice, &mut bob] {
      let replayed = peer.client.accept_call_move(dm, meeting).await;
      assert!(
        matches!(replayed, Err(CoreError::CallMoveRefused { .. })),
        "{replayed:?}"
      );
    }
  }

  /// A follow that failed halfway leaves the record behind; the call's end clears it and the call id.
  #[tokio::test]
  async fn the_end_of_the_call_clears_its_move_records() {
    let (server, mut alice, mut bob, _, _) = dm_call().await;
    let dm = in_dm_call(&mut alice, &mut bob).await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    let meeting = server.create_meeting_chat(alice_id, &[bob_id]);
    let (mut gateway, _) = alice.client.connect_gateway().await.unwrap();
    let call = alice.client.prepare_call(dm).await.unwrap().join.call_id;
    alice
      .client
      .move_call_to_meeting(dm, meeting, &[alice_id, bob_id], &[])
      .await
      .unwrap();
    assert!(
      call_move_announcement(alice.client.store(), dm, meeting)
        .unwrap()
        .is_some()
    );

    server.publish_event(
      &[alice_id],
      &GatewayEvent::CallEnd {
        conversation_id: dm,
        call_id: call,
      },
    );
    alice.client.pump(&mut gateway).await.unwrap();

    assert!(
      call_move_announcement(alice.client.store(), dm, meeting)
        .unwrap()
        .is_none()
    );
    let refused = alice
      .client
      .move_call_to_meeting(dm, meeting, &[alice_id], &[])
      .await;
    assert!(
      matches!(refused, Err(CoreError::CallMoveRefused { .. })),
      "{refused:?}"
    );
  }

  #[tokio::test]
  async fn only_the_device_that_added_people_follows_without_a_tap() {
    let (server, mut alice, mut bob, mut carol, _) = dm_call().await;
    let dm = in_dm_call(&mut alice, &mut bob).await;
    let (alice_id, bob_id, carol_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      carol.client.user_id(),
    );
    let meeting = server.create_meeting_chat(alice_id, &[bob_id, carol_id]);
    let mut laptop = link_device(&mut alice).await;
    alice
      .client
      .move_call_to_meeting(dm, meeting, &[alice_id, bob_id], &[carol_id])
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();
    laptop.full_sync().await.unwrap();
    carol.client.full_sync().await.unwrap();

    let own = call_move_announcement(alice.client.store(), dm, meeting)
      .unwrap()
      .unwrap();
    let others = call_move_announcement(bob.client.store(), dm, meeting)
      .unwrap()
      .unwrap();
    let other_own_device = call_move_announcement(laptop.store(), dm, meeting)
      .unwrap()
      .unwrap();
    assert!(own.is_requested_here);
    assert!(!others.is_requested_here);
    assert!(
      !other_own_device.is_requested_here,
      "the user's other device asks too"
    );
  }

  async fn in_dm_call(alice: &mut Peer, bob: &mut Peer) -> ConversationId {
    let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
    bob.client.full_sync().await.unwrap();
    alice.client.prepare_call(dm).await.unwrap();
    bob.client.prepare_call(dm).await.unwrap();
    dm
  }

  async fn dm_call() -> (FakeServer, Peer, Peer, Peer, UserId) {
    let server = FakeServer::new();
    let alice = Peer::enroll(&server, "Alice").await;
    let bob = Peer::enroll(&server, "Bob").await;
    let carol = Peer::enroll(&server, "Carol").await;
    let mallory = Peer::enroll(&server, "Mallory").await.client.user_id();
    (server, alice, bob, carol, mallory)
  }
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
