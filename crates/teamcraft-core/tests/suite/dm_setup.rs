//! DMs whose MLS setup was cut short (issue #228): the device that created the DM finishes the
//! setup on a later sync, and two devices that both founded a group never end up in two.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{PASSPHRASE, Peer, options, settings, settle};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::{FakeServer, NetworkCut};
use teamcraft_core::clock::ManualClock;
use teamcraft_core::store::MemoryStore;
use teamcraft_core::{Client, PendingEnrollment};
use teamcraft_proto::ConversationId;

const NOW_MS: i64 = 1_800_000_000_000;
/// REST calls of `open_dm` that get through before the network goes: only the DM's creation.
const CUT_BEFORE_FOUNDING: u32 = 1;
/// Creation, welcomes and the queue's epoch: the group is founded, the add never goes out.
const CUT_AFTER_FOUNDING: u32 = 3;

mod repair {
  use super::*;

  #[tokio::test]
  async fn a_dm_cut_off_before_its_group_was_founded_is_set_up_on_a_later_sync() {
    let (server, clock, alice, mut bob) = two().await;
    let (alice, dm) = open_cut_short(&server, &clock, alice, &bob, CUT_BEFORE_FOUNDING).await;
    let mut alice = alice;
    assert!(!alice.client.has_group(dm).unwrap(), "crashed before founding");

    // Not again right away: the next attempt waits for the backoff.
    alice.client.full_sync().await.unwrap();
    assert!(!alice.client.has_group(dm).unwrap(), "waits for the backoff");

    clock.advance(retry_ms());
    alice.client.full_sync().await.unwrap();
    assert!(
      alice.client.has_group(dm).unwrap(),
      "founded on the sync after the backoff"
    );
    bob.client.full_sync().await.unwrap();
    assert_talks(&mut alice, &mut bob, dm).await;
  }

  #[tokio::test]
  async fn a_dm_founded_but_never_shared_adds_the_other_side_on_a_later_sync() {
    let (server, clock, alice, mut bob) = two().await;
    let (alice, dm) = open_cut_short(&server, &clock, alice, &bob, CUT_AFTER_FOUNDING).await;
    let mut alice = alice;
    assert_eq!(alice.client.mls_epoch(dm).unwrap(), Some(0), "a group of one");

    clock.advance(retry_ms());
    alice.client.full_sync().await.unwrap();
    bob.client.full_sync().await.unwrap();
    assert_talks(&mut alice, &mut bob, dm).await;
  }

  #[tokio::test]
  async fn opening_the_dm_again_finishes_the_setup_at_once() {
    let (server, clock, alice, mut bob) = two().await;
    let (alice, dm) = open_cut_short(&server, &clock, alice, &bob, CUT_AFTER_FOUNDING).await;
    let mut alice = alice;

    assert_eq!(alice.client.open_dm(bob.client.user_id()).await.unwrap(), dm);
    bob.client.full_sync().await.unwrap();
    assert_talks(&mut alice, &mut bob, dm).await;
  }
}

mod no_fork {
  use super::*;

  #[tokio::test]
  async fn the_other_side_founding_first_wins_over_a_group_of_one() {
    let (server, clock, alice, mut bob) = two().await;
    let (alice, dm) = open_cut_short(&server, &clock, alice, &bob, CUT_AFTER_FOUNDING).await;
    let mut alice = alice;

    // Bob opens the same DM before Alice's device repairs: the server has no group, so he founds.
    assert_eq!(bob.client.open_dm(alice.client.user_id()).await.unwrap(), dm);
    alice.client.full_sync().await.unwrap();
    assert_eq!(
      alice.client.mls_epoch(dm).unwrap(),
      bob.client.mls_epoch(dm).unwrap()
    );

    // The repair after the backoff finds everybody in Bob's group and founds nothing.
    clock.advance(retry_ms());
    alice.client.full_sync().await.unwrap();
    bob.client.full_sync().await.unwrap();
    assert_talks(&mut alice, &mut bob, dm).await;
  }

  #[tokio::test]
  async fn two_devices_that_both_founded_end_in_one_group() {
    let (server, clock, alice, bob) = two().await;
    let (alice, dm) = open_cut_short(&server, &clock, alice, &bob, CUT_AFTER_FOUNDING).await;
    let mut alice = alice;
    let (bob, bob_dm) = open_cut_short(&server, &clock, bob, &alice, CUT_AFTER_FOUNDING).await;
    let mut bob = bob;
    assert_eq!(bob_dm, dm);
    assert_eq!(alice.client.mls_epoch(dm).unwrap(), Some(0));
    assert_eq!(bob.client.mls_epoch(dm).unwrap(), Some(0));

    // Both repair on the same sync round; whoever commits first wins, the other joins.
    clock.advance(retry_ms());
    alice.client.full_sync().await.unwrap();
    bob.client.full_sync().await.unwrap();
    alice.client.full_sync().await.unwrap();
    assert_talks(&mut alice, &mut bob, dm).await;

    // Later syncs don't found or add anything more.
    let epoch = alice.client.mls_epoch(dm).unwrap();
    clock.advance(retry_ms() * 4);
    alice.client.full_sync().await.unwrap();
    bob.client.full_sync().await.unwrap();
    assert_eq!(alice.client.mls_epoch(dm).unwrap(), epoch);
    assert_eq!(bob.client.mls_epoch(dm).unwrap(), epoch);
  }
}

/// A server, a shared clock and two peers on it.
async fn two() -> (FakeServer, Arc<ManualClock>, Peer, Peer) {
  let server = FakeServer::new();
  let clock = Arc::new(ManualClock::new(NOW_MS));
  let alice = enroll(&server, "Alice", &clock).await;
  let bob = enroll(&server, "Bob", &clock).await;
  (server, clock, alice, bob)
}

/// `peer` opens a DM with `other`, the network goes after `calls` REST calls, and the app crashes
/// and starts again.
async fn open_cut_short(
  server: &FakeServer,
  clock: &Arc<ManualClock>,
  mut peer: Peer,
  other: &Peer,
  calls: u32,
) -> (Peer, ConversationId) {
  let me = peer.client.user_id();
  server.cut_network_after(me, calls, NetworkCut::BeforeRequest);
  assert!(
    peer.client.open_dm(other.client.user_id()).await.is_err(),
    "the setup is cut short"
  );
  server.set_offline(me, false);
  let conversations = peer.api.list_conversations().await.unwrap().conversations;
  assert_eq!(conversations.len(), 1, "the DM exists on the server");
  let dm = conversations[0].id;
  (restart(peer, clock).await, dm)
}

/// Both sides are in one group, in one epoch, and read each other.
async fn assert_talks(alice: &mut Peer, bob: &mut Peer, dm: ConversationId) {
  let epoch = alice.client.mls_epoch(dm).unwrap();
  assert!(epoch.is_some_and(|epoch| epoch > 0), "{epoch:?}");
  assert_eq!(bob.client.mls_epoch(dm).unwrap(), epoch, "one group");
  alice.client.send_text(dm, None, "hi Bob").await.unwrap();
  bob.client.send_text(dm, None, "hi Alice").await.unwrap();
  settle(&mut [&mut *alice, &mut *bob]).await;
  assert_eq!(alice.texts(dm), vec!["hi Bob", "hi Alice"]);
  assert_eq!(bob.texts(dm), vec!["hi Bob", "hi Alice"]);
}

fn retry_ms() -> i64 {
  i64::try_from(settings().rejoin_retry.as_millis()).unwrap()
}

async fn enroll(server: &FakeServer, name: &str, clock: &Arc<ManualClock>) -> Peer {
  let api = server.add_user(name);
  let store = Arc::new(MemoryStore::new());
  let mut client = PendingEnrollment::new()
    .complete(
      api.clone(),
      store.clone(),
      Box::new(clock.clone()),
      settings(),
      options(PASSPHRASE),
    )
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

/// The peer's app starts again from its store (after a crash), with a new gateway connection.
async fn restart(peer: Peer, clock: &Arc<ManualClock>) -> Peer {
  let Peer { name, api, store, .. } = peer;
  let mut client = Client::open(api.clone(), store.clone(), Box::new(clock.clone()), settings())
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
