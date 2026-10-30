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
  let (_server, mut alice, mut bob, mut notetaker, channel) = setup(NotetakerInviters::Members).await;
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
