//! MLS commits whose answer is lost, devices that fall out of a group, and skewed clocks
//! (e2ee.md §4.6), through the fake server. Every scenario ends with all devices in one epoch.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{PASSPHRASE, Peer, options, settings, settle};
use teamcraft_core::Client;
use teamcraft_core::PendingEnrollment;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::{Clock, ManualClock, SystemClock};
use teamcraft_core::store::{LocalStore, MemoryStore, Table};
use teamcraft_proto::{ConversationId, HistoryPolicy, ListMlsMessagesQuery, MlsMessageKind};

const TEN_MINUTES_MS: i64 = 10 * 60 * 1000;

mod lost_commit_answers {
  use super::*;

  #[tokio::test]
  async fn a_commit_the_server_took_is_merged_when_it_shows_up() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob]).await;
    alice
      .client
      .send_text(channel, None, "before Carol")
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    server.lose_next_commit_answer(alice.client.user_id());
    let error = alice
      .client
      .add_member_with_history(channel, carol.client.user_id(), Some(HistoryPolicy::Full))
      .await
      .unwrap_err();
    assert!(error.is_transient(), "{error}");
    // Alice's own commit arrives over the gateway and is merged, not refused.
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert_same_epoch(channel, &mut [&mut alice, &mut bob, &mut carol]);

    // The history Alice owed Carol was sent once the commit was merged.
    alice.client.send_text(channel, None, "after").await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    carol.client.load_older_messages(channel).await.unwrap();
    assert_eq!(carol.texts(channel), ["before Carol", "after"]);
    assert_eq!(bob.texts(channel), ["before Carol", "after"]);
  }

  #[tokio::test]
  async fn a_restart_after_a_lost_answer_finds_the_commit_in_the_queue() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob]).await;

    server.lose_next_commit_answer(alice.client.user_id());
    alice
      .client
      .add_member(channel, carol.client.user_id())
      .await
      .unwrap_err();
    // The app is killed before it reads anything else; it starts again from its store.
    let mut alice = restart(alice).await;
    alice.client.full_sync().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert_same_epoch(channel, &mut [&mut alice, &mut bob, &mut carol]);

    carol.client.send_text(channel, None, "hello").await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert_eq!(alice.texts(channel), ["hello"]);
  }

  #[tokio::test]
  async fn a_commit_lost_on_the_way_is_sent_again() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob]).await;
    let epoch = alice.client.mls_epoch(channel).unwrap();

    server.drop_next_commit(alice.client.user_id());
    alice
      .client
      .add_member(channel, carol.client.user_id())
      .await
      .unwrap_err();
    assert_eq!(bob.client.mls_epoch(channel).unwrap(), epoch);
    // The next catch-up finds no commit for the epoch and sends the same one again.
    alice.client.sync_mls(channel).await.unwrap();
    alice.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert_eq!(
      alice.client.mls_epoch(channel).unwrap(),
      epoch.map(|epoch| epoch + 1)
    );
    assert_same_epoch(channel, &mut [&mut alice, &mut bob, &mut carol]);

    bob.client.send_text(channel, None, "welcome").await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert_eq!(carol.texts(channel), ["welcome"]);
  }

  #[tokio::test]
  async fn another_commit_that_won_the_epoch_replaces_the_lost_one() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let mut dave = Peer::enroll(&server, "Dave").await;
    let channel = channel_with(&mut alice, &mut [&mut bob]).await;

    server.drop_next_commit(alice.client.user_id());
    alice
      .client
      .add_member(channel, carol.client.user_id())
      .await
      .unwrap_err();
    // Bob commits first; Alice's lost commit must not come back.
    bob
      .client
      .add_member(channel, dave.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut dave]).await;
    assert_same_epoch(channel, &mut [&mut alice, &mut bob, &mut dave]);
    // Alice adds Carol again, on top of Bob's commit.
    alice
      .client
      .add_member(channel, carol.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    assert_same_epoch(channel, &mut [&mut alice, &mut bob, &mut carol, &mut dave]);
  }
}

mod fallen_out {
  use super::*;

  #[tokio::test]
  async fn a_device_that_refused_a_commit_is_added_back() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let mut dave = Peer::enroll(&server, "Dave").await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;

    // Bob's client refuses the commit that adds Dave; the others merge it and move on.
    server.garble_next_commit_for(bob.client.user_id());
    alice
      .client
      .add_member(channel, dave.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    // Bob noticed, asked to be added back, a member did; nobody had to do anything.
    assert_same_epoch(channel, &mut [&mut alice, &mut bob, &mut carol, &mut dave]);
    assert!(
      alice
        .client
        .group_device_ids(channel)
        .unwrap()
        .contains(&bob.client.device_id())
    );

    alice.client.send_text(channel, None, "all here?").await.unwrap();
    dave.client.send_text(channel, None, "yes").await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    assert_eq!(bob.texts(channel), ["all here?", "yes"]);
    bob.client.send_text(channel, None, "back").await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    assert_eq!(dave.texts(channel), ["all here?", "yes", "back"]);
  }

  /// Issue #149: members add a device back at most once per `rejoin_cooldown` (server times). A
  /// device that falls out again right away is added back only after it.
  #[tokio::test]
  async fn a_device_added_back_is_not_added_back_again_within_the_cooldown() {
    let server = FakeServer::new();
    let bob_clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = enroll_on(&server, "Bob", Box::new(bob_clock.clone())).await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let mut dave = Peer::enroll(&server, "Dave").await;
    let mut erin = Peer::enroll(&server, "Erin").await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol, &mut erin]).await;

    server.garble_next_commit_for(bob.client.user_id());
    alice
      .client
      .add_member(channel, dave.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave, &mut erin]).await;
    assert_same_epoch(
      channel,
      &mut [&mut alice, &mut bob, &mut carol, &mut dave, &mut erin],
    );

    // Bob falls out again at once and asks again: the members, who all saw him added back, don't
    // commit for him this time.
    server.garble_next_commit_for(bob.client.user_id());
    alice
      .client
      .remove_member(channel, dave.client.user_id())
      .await
      .unwrap();
    // The next commit shows Bob that he missed one.
    alice
      .client
      .remove_member(channel, erin.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave, &mut erin]).await;
    assert_eq!(
      proposals_by(&alice, &bob, channel).await,
      2,
      "Bob asked again at once"
    );
    // Unanswered, he asks again after `rejoin_retry` by his clock; the server's has hardly moved.
    let retry = settings().rejoin_retry + std::time::Duration::from_secs(60);
    bob_clock.advance(i64::try_from(retry.as_millis()).unwrap());
    bob.client.full_sync().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    assert_eq!(
      proposals_by(&alice, &bob, channel).await,
      3,
      "and after the retry"
    );
    assert_same_epoch(channel, &mut [&mut alice, &mut carol]);
    assert_ne!(
      bob.client.mls_epoch(channel).unwrap(),
      alice.client.mls_epoch(channel).unwrap(),
      "not added back within the cooldown"
    );

    // Past the cooldown Bob's next request is answered.
    let past_cooldown = settings().rejoin_cooldown + std::time::Duration::from_secs(60);
    server.advance_clock(past_cooldown);
    bob_clock.advance(i64::try_from(past_cooldown.as_millis()).unwrap());
    bob.client.full_sync().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    assert_same_epoch(channel, &mut [&mut alice, &mut bob, &mut carol]);
  }

  /// Issue #391: a member's note that it saw a device added back goes once the cooldown is over
  /// by the server's time, not only when another request for the device comes.
  #[tokio::test]
  async fn the_note_of_an_add_back_is_dropped_after_the_cooldown() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let mut dave = Peer::enroll(&server, "Dave").await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    server.garble_next_commit_for(bob.client.user_id());
    alice
      .client
      .add_member(channel, dave.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    assert_same_epoch(channel, &mut [&mut alice, &mut bob, &mut carol, &mut dave]);
    assert_eq!(readded_notes(&alice), 1);

    // Within the cooldown a full sync keeps it.
    alice.client.full_sync().await.unwrap();
    assert_eq!(readded_notes(&alice), 1);

    // Past it, by a queue entry the server stored later, the note goes.
    server.advance_clock(settings().rejoin_cooldown + std::time::Duration::from_secs(60));
    alice
      .client
      .remove_member(channel, dave.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    alice.client.full_sync().await.unwrap();
    assert_eq!(readded_notes(&alice), 0);
  }

  fn readded_notes(peer: &Peer) -> usize {
    peer
      .store
      .list(Table::AppliedChanges)
      .unwrap()
      .iter()
      .filter(|(key, _)| key.starts_with("readded/"))
      .count()
  }
}

mod skewed_clocks {
  use super::*;

  /// Bob's clock runs ten minutes behind, Carol's ten minutes ahead. Carol writes right before
  /// Alice removes her; Bob learns of both only later, removal first: the message counts (it was
  /// sent before the removal, by the server's clock), and everyone ends in the same epoch.
  #[tokio::test]
  async fn a_ten_minute_offset_changes_nothing() {
    let server = FakeServer::new();
    let now = SystemClock.now_ms();
    let mut alice = enroll_at(&server, "Alice", now).await;
    let mut bob = enroll_at(&server, "Bob", now - TEN_MINUTES_MS).await;
    let mut carol = enroll_at(&server, "Carol", now + TEN_MINUTES_MS).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;

    carol.client.send_text(channel, None, "last words").await.unwrap();
    alice
      .client
      .remove_member(channel, carol.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut carol]).await;
    // Bob catches up as after a while offline: the MLS queue (the removal) before the messages.
    bob.client.full_sync().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert_eq!(bob.texts(channel), ["last words"]);
    assert_eq!(alice.texts(channel), ["last words"]);
    assert_same_epoch(channel, &mut [&mut alice, &mut bob]);
    assert!(!carol.client.has_group(channel).unwrap());

    bob.client.send_text(channel, None, "still here").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    assert_eq!(alice.texts(channel), ["last words", "still here"]);
  }
}

/// How many proposals (rejoin requests) `sender`'s device put into the conversation's queue.
async fn proposals_by(reader: &Peer, sender: &Peer, channel: ConversationId) -> usize {
  let device = sender.client.device_id();
  reader
    .api
    .list_mls_messages(channel, &ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .messages
    .iter()
    .filter(|entry| entry.kind == MlsMessageKind::Proposal && entry.sender_device_id == device)
    .count()
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

fn assert_same_epoch(channel: ConversationId, peers: &mut [&mut Peer]) {
  let epochs: Vec<(String, Option<u64>)> = peers
    .iter_mut()
    .map(|peer| (peer.name.clone(), peer.client.mls_epoch(channel).unwrap()))
    .collect();
  let first = epochs[0].1;
  assert!(first.is_some(), "{epochs:?}");
  assert!(epochs.iter().all(|(_, epoch)| *epoch == first), "{epochs:?}");
}

/// The peer's app starts again from its store, with a new gateway connection.
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

/// A peer whose clock shows `now_ms`.
async fn enroll_at(server: &FakeServer, name: &str, now_ms: i64) -> Peer {
  enroll_on(server, name, Box::new(ManualClock::new(now_ms))).await
}

/// A peer whose client runs on `clock`.
async fn enroll_on(server: &FakeServer, name: &str, clock: Box<dyn Clock>) -> Peer {
  let api = server.add_user(name);
  let store = Arc::new(MemoryStore::new());
  let mut client = PendingEnrollment::new()
    .complete(api.clone(), store.clone(), clock, settings(), options(PASSPHRASE))
    .await
    .unwrap();
  let (gateway, _) = client.connect_gateway().await.unwrap();
  Peer {
    name: name.to_string(),
    api,
    store,
    client,
    gateway,
  }
}
