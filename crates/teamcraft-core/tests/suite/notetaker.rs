//! The notetaker's requests through the fake server (e2ee.md §8.2): members ask it in and send it
//! away with encrypted control messages, and every device, the notetaker's own included, takes a
//! request only from a sender the verified policy allows.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::notetaker::{NotetakerLedger, notetaker_policy};
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::{
  ConversationId, HistoryPolicy, NotetakerInviters, NotetakerPolicy, NotetakerRequest, NotetakerScope,
  WorkspacePolicy,
};

const KEY_SHA256: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

#[tokio::test]
async fn a_members_request_reaches_the_notetaker_and_a_stop_ends_it() {
  let (server, mut alice, mut bob, mut notetaker, channel) = setup(NotetakerInviters::Members).await;
  let bob_ui = backend_of(&bob);
  bob_ui.request_notetaker(channel, every_call());
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob, &mut notetaker]).await;

  let ledger = NotetakerLedger::load(notetaker.store.as_ref(), channel).unwrap();
  let join = ledger.join_in_force(i64::MAX / 2, None).unwrap();
  assert_eq!(
    (join.by, join.scope),
    (bob.client.user_id(), NotetakerScope::EveryCall)
  );
  // The request is a hidden control message: nobody's timeline shows it.
  assert!(alice.texts(channel).is_empty());

  // Requests are ordered by the server's millisecond clock, and a stop only ends a join sent
  // strictly before it. The fake server's clock is the wall clock, so on a fast run the join and
  // the stop landed in the same millisecond and the stop did not count: move the clock on.
  server.advance_clock(std::time::Duration::from_millis(5));
  let alice_ui = backend_of(&alice);
  alice_ui.request_notetaker(channel, NotetakerRequest::Stop);
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob, &mut notetaker]).await;
  let ledger = NotetakerLedger::load(notetaker.store.as_ref(), channel).unwrap();
  assert!(ledger.is_stopped_after(join.at_ms));
  assert_eq!(ledger.join_in_force(i64::MAX / 2, None), None);
}

#[tokio::test]
async fn under_admins_only_a_members_request_never_reaches_the_ledger() {
  let (_server, mut alice, mut bob, mut notetaker, channel) = setup(NotetakerInviters::Admins).await;
  let bob_ui = backend_of(&bob);
  bob_ui.request_notetaker(channel, every_call());
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob, &mut notetaker]).await;
  let ledger = NotetakerLedger::load(notetaker.store.as_ref(), channel).unwrap();
  assert!(ledger.requests().is_empty());

  let alice_ui = backend_of(&alice);
  alice_ui.request_notetaker(channel, every_call());
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob, &mut notetaker]).await;
  let ledger = NotetakerLedger::load(notetaker.store.as_ref(), channel).unwrap();
  assert_eq!(
    ledger.join_in_force(i64::MAX / 2, None).unwrap().by,
    alice.client.user_id()
  );
}

#[tokio::test]
async fn the_ui_reads_the_verified_policy() {
  let (_server, alice, _bob, notetaker, _channel) = setup(NotetakerInviters::Members).await;
  let policy = backend_of(&alice).notetaker_policy().unwrap();
  assert!(policy.is_notetaker(notetaker.client.user_id()));
  assert_eq!(notetaker_policy(notetaker.store.as_ref()), Some(policy));
}

/// "Add notetaker" in a call where it is no member yet (issue #1018): the addition and the request
/// queued together, the request goes out after the commit that put the notetaker into the group,
/// so it can read it.
#[tokio::test]
async fn added_during_a_call_it_reads_the_join_sent_with_its_addition() {
  let (_server, mut alice, mut bob, mut notetaker, channel) =
    setup_with(NotetakerInviters::Members, false).await;
  let bot = notetaker.client.user_id();
  let alice_ui = backend_of(&alice);
  alice_ui.add_people_to_channel(channel, vec![(bot, true)], HistoryPolicy::FromJoin, false);
  alice_ui.request_notetaker(
    channel,
    NotetakerRequest::Join {
      scope: NotetakerScope::ThisCall,
    },
  );
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob, &mut notetaker]).await;

  let ledger = NotetakerLedger::load(notetaker.store.as_ref(), channel).unwrap();
  let [request] = ledger.requests() else {
    panic!("one request: {ledger:?}");
  };
  assert_eq!(request.by, alice.client.user_id());
  // A `this_call` join is in force for a while after it was sent.
  let join = ledger.join_in_force(request.at_ms + 1, None).unwrap();
  assert_eq!(join.scope, NotetakerScope::ThisCall);
}

/// A device added during a call joins it: the call asks for the conversation's devices again, and
/// they include the new member's before this device has seen its addition otherwise.
#[tokio::test]
async fn the_refreshed_call_devices_include_a_member_added_since() {
  let (_server, mut alice, mut bob, notetaker, channel) = setup_with(NotetakerInviters::Members, false).await;
  let prepared = bob.client.verified_call_devices(channel).await.unwrap();
  assert_eq!(prepared.len(), 2);
  alice
    .client
    .add_member(channel, notetaker.client.user_id())
    .await
    .unwrap();
  let refreshed = bob.client.refresh_call_devices(channel).await.unwrap();
  assert_eq!(refreshed.len(), 3, "the notetaker's device as well");
}

/// "Add notetaker" in a DM call (issue #1271, e2ee.md §8.2, "In every call"): the call moves into
/// a meeting room with the people in it and the notetaker, which is asked in there and never sees
/// the DM.
mod in_a_dm_call {
  use super::*;

  #[tokio::test]
  async fn the_notetaker_is_asked_into_the_room_and_not_rung() {
    let (server, mut alice, mut bob, mut notetaker, dm) = dm_call(NotetakerInviters::Members).await;
    let (alice_id, bob_id, bot) = (
      alice.client.user_id(),
      bob.client.user_id(),
      notetaker.client.user_id(),
    );
    let room = server.create_meeting_chat(alice_id, &[bob_id, bot]);

    alice
      .client
      .move_call_to_meeting(dm, room, &[alice_id, bob_id], &[bot])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut notetaker]).await;

    let moves = server.call_moves();
    assert_eq!(moves.len(), 1);
    assert!(moves[0].1.ring_user_ids.is_empty(), "the notetaker doesn't ring");
    let ledger = NotetakerLedger::load(notetaker.store.as_ref(), room).unwrap();
    let [request] = ledger.requests() else {
      panic!("one request: {ledger:?}");
    };
    assert_eq!(request.by, alice_id);
    let join = ledger.join_in_force(request.at_ms + 1, None).unwrap();
    assert_eq!(join.scope, NotetakerScope::ThisCall);
    // Everybody in the call and the notetaker; Bob's app checks the move against the DM's group.
    assert_eq!(bob.client.prepare_call(room).await.unwrap().devices.len(), 3);
    bob.client.accept_call_move(dm, room).await.unwrap();
  }

  #[tokio::test]
  async fn the_notetaker_never_sees_the_dm() {
    let (server, mut alice, mut bob, mut notetaker, dm) = dm_call(NotetakerInviters::Members).await;
    alice
      .client
      .send_text(dm, None, "before the notetaker")
      .await
      .unwrap();
    let epoch = alice.client.mls_epoch(dm).unwrap();
    let (alice_id, bob_id, bot) = (
      alice.client.user_id(),
      bob.client.user_id(),
      notetaker.client.user_id(),
    );
    let room = server.create_meeting_chat(alice_id, &[bob_id, bot]);

    alice
      .client
      .move_call_to_meeting(dm, room, &[alice_id, bob_id], &[bot])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut notetaker]).await;

    assert_eq!(alice.client.mls_epoch(dm).unwrap(), epoch, "no commit in the DM");
    assert!(!notetaker.client.has_group(dm).unwrap(), "out of the DM's group");
    assert!(notetaker.texts(dm).is_empty());
    assert!(notetaker.texts(room).is_empty(), "a new room has no history");
  }

  #[tokio::test]
  async fn anybody_in_the_room_sends_it_away() {
    let (server, mut alice, mut bob, mut notetaker, dm) = dm_call(NotetakerInviters::Members).await;
    let (alice_id, bob_id, bot) = (
      alice.client.user_id(),
      bob.client.user_id(),
      notetaker.client.user_id(),
    );
    let room = server.create_meeting_chat(alice_id, &[bob_id, bot]);
    alice
      .client
      .move_call_to_meeting(dm, room, &[alice_id, bob_id], &[bot])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut notetaker]).await;

    let bob_ui = backend_of(&bob);
    bob_ui.request_notetaker(room, NotetakerRequest::Stop);
    run_commands(&mut bob, &bob_ui).await;
    settle(&mut [&mut alice, &mut bob, &mut notetaker]).await;

    let ledger = NotetakerLedger::load(notetaker.store.as_ref(), room).unwrap();
    assert_eq!(ledger.join_in_force(i64::MAX / 2, None), None);
    assert!(ledger.last_stop_ms().is_some());
  }

  #[tokio::test]
  async fn a_user_the_policy_does_not_allow_asks_nobody_in() {
    // Admins only: Bob isn't one.
    let (server, mut alice, mut bob, mut notetaker, dm) = dm_call(NotetakerInviters::Admins).await;
    let (alice_id, bob_id, bot) = (
      alice.client.user_id(),
      bob.client.user_id(),
      notetaker.client.user_id(),
    );
    let room = server.create_meeting_chat(bob_id, &[alice_id, bot]);

    bob
      .client
      .move_call_to_meeting(dm, room, &[alice_id, bob_id], &[bot])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut notetaker]).await;

    let ledger = NotetakerLedger::load(notetaker.store.as_ref(), room).unwrap();
    assert!(ledger.requests().is_empty(), "no join was sent");
    let own = NotetakerLedger::load(bob.store.as_ref(), room).unwrap();
    assert!(own.requests().is_empty());
  }

  /// [`setup_with`] without the notetaker in the channel, and Alice and Bob in a call of their DM.
  async fn dm_call(inviters: NotetakerInviters) -> (FakeServer, Peer, Peer, Peer, ConversationId) {
    let (server, mut alice, mut bob, notetaker, _channel) = setup_with(inviters, false).await;
    let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
    bob.client.full_sync().await.unwrap();
    alice.client.prepare_call(dm).await.unwrap();
    bob.client.prepare_call(dm).await.unwrap();
    (server, alice, bob, notetaker, dm)
  }
}

/// "Add notetaker" in a call of a conversation the user can't add the notetaker to (issue #1274,
/// e2ee.md §8.2, "In every call"): a channel whose members somebody else manages, a meeting
/// somebody else organizes. The call moves into a room of the user's with the people in it and the
/// notetaker, as from a DM; the conversation itself gets no commit and the notetaker never sees it.
mod in_a_call_the_user_cannot_add_to {
  use super::*;

  #[tokio::test]
  async fn a_channel_member_moves_the_call_to_the_notetaker() {
    let (server, mut alice, mut bob, mut notetaker, channel) = channel_call(NotetakerInviters::Members).await;
    let epoch = alice.client.mls_epoch(channel).unwrap();

    let room = bob_moves_with_notetaker(&server, &mut alice, &mut bob, &mut notetaker, channel).await;

    let ledger = NotetakerLedger::load(notetaker.store.as_ref(), room).unwrap();
    let [request] = ledger.requests() else {
      panic!("one request: {ledger:?}");
    };
    assert_eq!(request.by, bob.client.user_id());
    assert!(ledger.join_in_force(request.at_ms + 1, None).is_some());
    assert!(
      server.call_moves()[0].1.ring_user_ids.is_empty(),
      "the notetaker doesn't ring"
    );
    assert_eq!(
      alice.client.mls_epoch(channel).unwrap(),
      epoch,
      "no commit in the channel"
    );
    assert!(
      !notetaker.client.has_group(channel).unwrap(),
      "out of the channel's group"
    );
    // Alice, who manages the channel, checks the move against its group before she follows.
    alice.client.accept_call_move(channel, room).await.unwrap();
  }

  #[tokio::test]
  async fn under_admins_only_a_channel_member_asks_nobody_in() {
    let (server, mut alice, mut bob, mut notetaker, channel) = channel_call(NotetakerInviters::Admins).await;

    let room = bob_moves_with_notetaker(&server, &mut alice, &mut bob, &mut notetaker, channel).await;

    let ledger = NotetakerLedger::load(notetaker.store.as_ref(), room).unwrap();
    assert!(ledger.requests().is_empty(), "no join was sent");
  }

  #[tokio::test]
  async fn an_invitee_moves_the_meeting_call_to_the_notetaker() {
    let (server, mut alice, mut bob, mut notetaker, _channel) =
      setup_with(NotetakerInviters::Members, false).await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    // Alice organizes the meeting; Bob is invited and can't change who is.
    let meeting = server.create_meeting_chat(alice_id, &[bob_id]);
    alice
      .client
      .set_up_meeting_chat(meeting, &[bob_id])
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();
    alice.client.prepare_call(meeting).await.unwrap();
    bob.client.prepare_call(meeting).await.unwrap();

    let room = bob_moves_with_notetaker(&server, &mut alice, &mut bob, &mut notetaker, meeting).await;

    let ledger = NotetakerLedger::load(notetaker.store.as_ref(), room).unwrap();
    let [request] = ledger.requests() else {
      panic!("one request: {ledger:?}");
    };
    assert_eq!(request.by, bob_id);
    assert!(
      !notetaker.client.has_group(meeting).unwrap(),
      "out of the meeting chat's group"
    );
    alice.client.accept_call_move(meeting, room).await.unwrap();
  }

  /// Bob starts a room with Alice and the notetaker and moves the call of `from` into it.
  async fn bob_moves_with_notetaker(
    server: &FakeServer,
    alice: &mut Peer,
    bob: &mut Peer,
    notetaker: &mut Peer,
    from: ConversationId,
  ) -> ConversationId {
    let (alice_id, bob_id, bot) = (
      alice.client.user_id(),
      bob.client.user_id(),
      notetaker.client.user_id(),
    );
    let room = server.create_meeting_chat(bob_id, &[alice_id, bot]);
    bob
      .client
      .move_call_to_meeting(from, room, &[alice_id, bob_id], &[bot])
      .await
      .unwrap();
    settle(&mut [alice, bob, notetaker]).await;
    room
  }

  /// [`setup_with`] without the notetaker in the channel, which Alice manages, and Alice and Bob
  /// in its call.
  async fn channel_call(inviters: NotetakerInviters) -> (FakeServer, Peer, Peer, Peer, ConversationId) {
    let (server, mut alice, mut bob, notetaker, channel) = setup_with(inviters, false).await;
    alice.client.prepare_call(channel).await.unwrap();
    bob.client.prepare_call(channel).await.unwrap();
    (server, alice, bob, notetaker, channel)
  }
}

/// Alice (owner) signs a policy with Notetaker's account as the notetaker; all three are in a
/// channel.
async fn setup(inviters: NotetakerInviters) -> (FakeServer, Peer, Peer, Peer, ConversationId) {
  setup_with(inviters, true).await
}

/// [`setup`], with the notetaker in the channel only when `is_member`.
async fn setup_with(
  inviters: NotetakerInviters,
  is_member: bool,
) -> (FakeServer, Peer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut notetaker = Peer::enroll(&server, "Notetaker").await;
  let alice_id = alice.client.user_id();
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner)])
    .await
    .unwrap();
  alice
    .client
    .publish_policy(WorkspacePolicy {
      notetaker: NotetakerPolicy {
        is_enabled: true,
        maybe_bot: Some(notetaker.client.user_id()),
        inviters,
        maybe_api_key_sha256: Some(KEY_SHA256.into()),
        ..NotetakerPolicy::default()
      },
      ..WorkspacePolicy::default()
    })
    .await
    .unwrap();
  let channel = alice
    .client
    .create_channel(alice.space().await, "meetings")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  if is_member {
    alice
      .client
      .add_member(channel, notetaker.client.user_id())
      .await
      .unwrap();
  }
  settle(&mut [&mut alice, &mut bob, &mut notetaker]).await;
  for peer in [&mut alice, &mut bob, &mut notetaker] {
    peer.client.sync_workspace().await.unwrap();
  }
  (server, alice, bob, notetaker, channel)
}

fn every_call() -> NotetakerRequest {
  NotetakerRequest::Join {
    scope: NotetakerScope::EveryCall,
  }
}
