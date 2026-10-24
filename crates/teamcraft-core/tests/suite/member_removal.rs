//! Removing members, leaving and revoking devices as one recoverable operation (e2ee.md §4.6,
//! removal order): the MLS removal goes first, the server's membership afterwards, and a crash or
//! lost answer in between is finished on the next round. Through the fake server.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settings, settle};
use teamcraft_core::Client;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::{FakeServer, NetworkCut};
use teamcraft_proto::{
  ConversationId, GatewayEvent, GatewayServerMessage, ListMessagesQuery, ListMlsMessagesQuery,
  MlsMessageKind, UserId,
};

mod interrupted {
  use super::*;

  #[tokio::test]
  async fn a_lost_commit_answer_is_finished_on_the_next_round() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let commits_before = commits(&alice, channel).await;

    server.lose_next_commit_answer(alice.client.user_id());
    let error = alice
      .client
      .remove_member(channel, carol.client.user_id())
      .await
      .unwrap_err();
    assert!(error.is_transient(), "{error}");
    // The commit went through; the server still lists Carol until Alice's client finishes.
    assert_eq!(commits(&alice, channel).await, commits_before + 1);
    assert!(is_listed(&alice, channel, carol.client.user_id()).await);

    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    alice.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert!(!is_listed(&alice, channel, carol.client.user_id()).await);
    assert_eq!(commits(&alice, channel).await, commits_before + 1);
    assert_removed(channel, &mut alice, &mut bob, &mut carol).await;
  }

  #[tokio::test]
  async fn a_crash_before_the_server_removal_is_finished_after_a_restart() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;

    // The app is killed after the commit, before the server hears about the removal.
    server.cut_next_removal(alice.client.user_id(), NetworkCut::BeforeRequest);
    alice
      .client
      .remove_member(channel, carol.client.user_id())
      .await
      .unwrap_err();
    assert!(is_listed(&alice, channel, carol.client.user_id()).await);
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    // Carol is out of the group already (her removal commit came first): nothing she could read
    // in between.
    assert!(!carol.client.has_group(channel).unwrap());

    let mut alice = restart(alice).await;
    alice.client.full_sync().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert!(!is_listed(&alice, channel, carol.client.user_id()).await);
    assert_removed(channel, &mut alice, &mut bob, &mut carol).await;
  }

  #[tokio::test]
  async fn a_lost_answer_to_the_server_removal_is_retried_without_a_second_commit() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let commits_before = commits(&alice, channel).await;

    server.cut_next_removal(alice.client.user_id(), NetworkCut::ResponseLost);
    alice
      .client
      .remove_member(channel, carol.client.user_id())
      .await
      .unwrap_err();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    // The user tries again: the server and the group are done already, both steps are no-ops.
    alice
      .client
      .remove_member(channel, carol.client.user_id())
      .await
      .unwrap();
    alice.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert_eq!(commits(&alice, channel).await, commits_before + 1);
    assert_removed(channel, &mut alice, &mut bob, &mut carol).await;
  }
}

mod hostile_server {
  use super::*;

  /// Carol's client never hears she was removed, and a server serves her the removal commit and
  /// the next message anyway: she can't read it. An honest server refuses her the queue.
  #[tokio::test]
  async fn a_removed_member_gets_nothing_of_the_next_epoch() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let welcomes_before = welcomes_of(&carol).await;

    alice
      .client
      .remove_member(channel, carol.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.send_text(channel, None, "secret").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    assert_eq!(bob.texts(channel), ["secret"]);

    let error = carol
      .api
      .list_mls_messages(channel, &ListMlsMessagesQuery::default())
      .await
      .unwrap_err();
    assert!(error.is_not_found(), "{error}");
    assert_eq!(welcomes_of(&carol).await, welcomes_before);

    let removal = alice
      .api
      .list_mls_messages(channel, &ListMlsMessagesQuery::default())
      .await
      .unwrap()
      .messages
      .into_iter()
      .rfind(|entry| entry.kind == MlsMessageKind::Commit)
      .unwrap();
    let secret = alice
      .api
      .list_messages(channel, &ListMessagesQuery::default())
      .await
      .unwrap()
      .messages
      .pop()
      .unwrap();
    for (seq, event) in [
      GatewayEvent::MlsMessage { message: removal },
      GatewayEvent::MessageCreate { message: secret },
    ]
    .into_iter()
    .enumerate()
    {
      let frame = GatewayServerMessage::Event {
        seq: 10_000 + seq as u64,
        event,
      };
      // Refused or ignored; either way nothing becomes readable.
      let _ = carol.client.handle_frame(frame).await;
    }
    assert!(!carol.texts(channel).iter().any(|text| text == "secret"));
  }
}

mod concurrent {
  use super::*;

  #[tokio::test]
  async fn two_members_removing_the_same_person_both_succeed_with_one_commit() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let commits_before = commits(&alice, channel).await;
    let carol_id = carol.client.user_id();

    let (by_alice, by_bob) = tokio::join!(
      alice.client.remove_member(channel, carol_id),
      bob.client.remove_member(channel, carol_id),
    );
    by_alice.unwrap();
    by_bob.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert_eq!(commits(&alice, channel).await, commits_before + 1);
    assert!(!is_listed(&alice, channel, carol_id).await);
    assert_removed(channel, &mut alice, &mut bob, &mut carol).await;
  }

  /// Bob's commit takes the epoch Alice signed her removal for: Alice catches up and commits it in
  /// the next epoch.
  #[tokio::test]
  async fn an_epoch_conflict_during_the_removal_is_rebased() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let mut dave = Peer::enroll(&server, "Dave").await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let before = epoch(&mut alice, channel);

    // Alice hasn't seen Bob's commit when she removes Carol.
    bob
      .client
      .add_member(channel, dave.client.user_id())
      .await
      .unwrap();
    alice
      .client
      .remove_member(channel, carol.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

    assert_eq!(epoch(&mut alice, channel), before + 2);
    assert_eq!(epoch(&mut dave, channel), before + 2);
    assert!(!is_listed(&alice, channel, carol.client.user_id()).await);
    assert_removed(channel, &mut alice, &mut bob, &mut carol).await;
    settle(&mut [&mut dave]).await;
    assert_eq!(dave.texts(channel).last().map(String::as_str), Some("after"));
  }
}

mod refused {
  use super::*;

  #[tokio::test]
  async fn a_removal_the_server_refuses_leaves_the_group_alone() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let commits_before = commits(&alice, channel).await;

    server.refuse_removals_by(bob.client.user_id());
    let error = bob
      .client
      .remove_member(channel, carol.client.user_id())
      .await
      .unwrap_err();
    assert!(!error.is_transient(), "{error}");
    bob.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert_eq!(commits(&alice, channel).await, commits_before);
    assert!(is_listed(&alice, channel, carol.client.user_id()).await);
    assert!(carol.client.has_group(channel).unwrap());
  }

  #[tokio::test]
  async fn a_refused_leave_sends_no_goodbye() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let entries_before = queue_len(&alice, channel).await;

    server.refuse_removals_by(carol.client.user_id());
    carol.client.leave_channel(channel).await.unwrap_err();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert_eq!(queue_len(&alice, channel).await, entries_before);
    assert!(carol.client.has_group(channel).unwrap());
    assert!(bob.texts(channel).is_empty());
  }
}

mod leaving {
  use super::*;

  /// Before, leaving only dropped the server's membership and the leaver's devices stayed in the
  /// group: they got the keys of every later epoch.
  #[tokio::test]
  async fn the_remaining_members_remove_the_leaver_from_the_group() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let commits_before = commits(&alice, channel).await;

    carol.client.leave_channel(channel).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    alice.client.apply_pending_changes().await.unwrap();
    bob.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert_eq!(commits(&alice, channel).await, commits_before + 1);
    assert!(!is_listed(&alice, channel, carol.client.user_id()).await);
    assert!(!carol.client.has_group(channel).unwrap());
    assert_removed(channel, &mut alice, &mut bob, &mut carol).await;
  }

  #[tokio::test]
  async fn a_leave_cut_off_before_the_server_heard_is_finished_on_the_next_round() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;

    server.cut_next_removal(carol.client.user_id(), NetworkCut::BeforeRequest);
    carol.client.leave_channel(channel).await.unwrap_err();
    assert!(is_listed(&alice, channel, carol.client.user_id()).await);
    // The signed request is in the queue already: the members commit it meanwhile.
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    bob.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    carol.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert!(!is_listed(&alice, channel, carol.client.user_id()).await);
    assert_removed(channel, &mut alice, &mut bob, &mut carol).await;
  }
}

mod revoking {
  use super::*;

  #[tokio::test]
  async fn a_revocation_cut_off_after_the_server_is_finished_on_the_next_round() {
    let server = FakeServer::new();
    let (mut alice, mut bob, _) = three(&server).await;
    let laptop = common::link_device(&mut alice).await;
    let laptop_device = laptop.device_id();
    let channel = channel_with(&mut alice, &mut [&mut bob]).await;
    alice.client.add_missing_devices(channel).await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let commits_before = commits(&alice, channel).await;
    let list_before = server.device_list_of(alice.client.user_id()).unwrap();

    // The server revoked the laptop, the answer was lost: no device list, no group removal yet.
    server.cut_next_removal(alice.client.user_id(), NetworkCut::ResponseLost);
    alice.client.revoke_device(laptop_device).await.unwrap_err();
    assert_eq!(commits(&alice, channel).await, commits_before);

    alice.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    assert_eq!(commits(&alice, channel).await, commits_before + 1);
    let newest = server.device_list_of(alice.client.user_id()).unwrap();
    assert!(newest.sequence > list_before.sequence);
    bob.client.send_text(channel, None, "after").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    assert_eq!(alice.texts(channel), ["after"]);
  }
}

async fn three(server: &FakeServer) -> (Peer, Peer, Peer) {
  (
    Peer::enroll(server, "Alice").await,
    Peer::enroll(server, "Bob").await,
    Peer::enroll(server, "Carol").await,
  )
}

/// A channel of `owner` with `others` added and joined.
async fn channel_with(owner: &mut Peer, others: &mut [&mut Peer]) -> ConversationId {
  let space = owner.space().await;
  let channel = owner.client.create_channel(space, "general").await.unwrap();
  for other in others.iter() {
    owner
      .client
      .add_member(channel, other.client.user_id())
      .await
      .unwrap();
  }
  let mut peers: Vec<&mut Peer> = vec![owner];
  peers.extend(others.iter_mut().map(|peer| &mut **peer));
  settle(&mut peers).await;
  channel
}

/// Carol is out: the others share an epoch, a new message reaches them and not her.
async fn assert_removed(channel: ConversationId, alice: &mut Peer, bob: &mut Peer, carol: &mut Peer) {
  assert_eq!(epoch(alice, channel), epoch(bob, channel));
  alice.client.send_text(channel, None, "after").await.unwrap();
  settle(&mut [alice, bob, carol]).await;
  assert_eq!(bob.texts(channel).last().map(String::as_str), Some("after"));
  assert!(!carol.texts(channel).iter().any(|text| text == "after"));
}

fn epoch(peer: &mut Peer, channel: ConversationId) -> u64 {
  peer.client.mls_epoch(channel).unwrap().unwrap()
}

async fn is_listed(peer: &Peer, channel: ConversationId, user: UserId) -> bool {
  peer
    .api
    .list_members(channel)
    .await
    .unwrap()
    .members
    .iter()
    .any(|member| member.user.id == user)
}

async fn queue_len(peer: &Peer, channel: ConversationId) -> usize {
  peer
    .api
    .list_mls_messages(channel, &ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .messages
    .len()
}

async fn commits(peer: &Peer, channel: ConversationId) -> usize {
  peer
    .api
    .list_mls_messages(channel, &ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .messages
    .iter()
    .filter(|entry| entry.kind == MlsMessageKind::Commit)
    .count()
}

async fn welcomes_of(peer: &Peer) -> usize {
  peer
    .api
    .list_welcomes(peer.client.device_id(), &ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .messages
    .len()
}

async fn restart(peer: Peer) -> Peer {
  let Peer { name, api, store, .. } = peer;
  let mut client = Client::open(api.clone(), store.clone(), common::clock(), settings())
    .unwrap()
    .unwrap();
  let (gateway, _) = client.connect_gateway().await.unwrap();
  Peer {
    name,
    api,
    store,
    client,
    gateway,
  }
}
