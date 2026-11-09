//! Leavers leave the encryption groups with nobody asked (docs/provisioning.md §3): an admin's app
//! signs the removal from all chats of somebody the identity provider deactivated, after the grace,
//! within its own daily limit, never for somebody on the roster and only under an `automatic`
//! signed policy. Through the fake server, with an injected clock.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use crate::common;

use common::{PASSPHRASE, Peer, options, settings, settle};
use teamcraft_core::PendingEnrollment;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::LeaverRound;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::{
  ConversationId, LeaverPolicy, LeaverRemovalMode, ListMlsMessagesQuery, MlsMessageKind, WorkspacePolicy,
};

const HOUR_SECS: u64 = 60 * 60;
const HOUR_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// Alice owns the workspace, the admin's app removes leavers, Bob and Carol are members of
/// Alice's channel.
struct Setup {
  server: FakeServer,
  clock: Arc<ManualClock>,
  alice: Peer,
  bob: Peer,
  carol: Peer,
  admin: Peer,
  channel: ConversationId,
}

mod removal {
  use super::*;

  /// The provider disables Carol. The admin's app waits out the grace, then signs her removal from
  /// the channel; Alice's app commits it (keys rotate) and the server drops what is left.
  #[tokio::test]
  async fn a_leaver_leaves_the_groups_after_the_grace() {
    let mut setup = setup(LeaverPolicy::default()).await;
    let carol_id = setup.carol.client.user_id();
    let commits_before = commits(&setup.alice, setup.channel).await;

    setup.server.deactivate_leaver(carol_id, 0);
    setup.settle().await;
    assert_eq!(
      run(&mut setup.admin).await,
      LeaverRound::default(),
      "within the grace"
    );
    assert_eq!(setup.server.leaver_claims(carol_id), 0);

    // Nothing is asked again before the grace ends.
    setup.clock.advance(HOUR_MS / 2);
    assert_eq!(run(&mut setup.admin).await, LeaverRound::default());

    setup.server.age_leaver(carol_id, HOUR_SECS);
    setup.clock.advance(HOUR_MS / 2);
    let round = run(&mut setup.admin).await;
    assert_eq!(round.removed, 1);
    assert_eq!(proposals_by(&setup.alice, &setup.admin, setup.channel).await, 1);
    assert!(!setup.server.is_listed_leaver(carol_id));

    setup.settle().await;
    assert_eq!(commits(&setup.alice, setup.channel).await, commits_before + 1);
  }

  /// With no grace, the event alone starts the removal.
  #[tokio::test]
  async fn no_grace_removes_at_once() {
    let mut setup = setup(LeaverPolicy {
      grace_minutes: 0,
      ..LeaverPolicy::default()
    })
    .await;
    let carol_id = setup.carol.client.user_id();

    setup.server.deactivate_leaver(carol_id, 0);
    setup.settle().await;
    assert_eq!(run(&mut setup.admin).await.removed, 1);
    assert_eq!(setup.server.leaver_claims(carol_id), 1);
  }

  /// A leaver another device claimed is left to it.
  #[tokio::test]
  async fn a_leaver_another_device_claimed_is_skipped() {
    let mut setup = setup(LeaverPolicy {
      grace_minutes: 0,
      ..LeaverPolicy::default()
    })
    .await;
    let carol_id = setup.carol.client.user_id();
    let other_device = setup.alice.client.device_id();

    setup.server.deactivate_leaver(carol_id, 0);
    setup.server.claim_leaver_elsewhere(carol_id, other_device);
    setup.settle().await;
    assert_eq!(run(&mut setup.admin).await, LeaverRound::default());
    assert!(setup.server.is_listed_leaver(carol_id));
  }
}

mod refused {
  use super::*;

  /// Somebody on the roster is never removed on the server's word: role evidence and an owner go
  /// first (e2ee.md §3.1.3).
  #[tokio::test]
  async fn nobody_on_the_roster_is_removed() {
    let mut setup = setup(LeaverPolicy {
      grace_minutes: 0,
      ..LeaverPolicy::default()
    })
    .await;
    let alice_id = setup.alice.client.user_id();

    setup.server.deactivate_leaver(alice_id, 10 * HOUR_SECS);
    setup.settle().await;
    assert_eq!(run(&mut setup.admin).await, LeaverRound::default());
    assert_eq!(setup.server.leaver_claims(alice_id), 0);
  }

  /// A signed policy that says manual turns it off; so does a mode this app doesn't know.
  #[tokio::test]
  async fn manual_or_unknown_mode_removes_nobody() {
    for mode in [
      LeaverRemovalMode::Manual,
      LeaverRemovalMode::Unknown("later".into()),
    ] {
      let mut setup = setup(LeaverPolicy {
        mode,
        grace_minutes: 0,
        ..LeaverPolicy::default()
      })
      .await;
      let carol_id = setup.carol.client.user_id();

      setup.server.deactivate_leaver(carol_id, HOUR_SECS);
      setup.settle().await;
      assert_eq!(run(&mut setup.admin).await, LeaverRound::default());
      assert!(setup.server.is_listed_leaver(carol_id));
    }
  }

  /// A member's app never asks: the list is for admins.
  #[tokio::test]
  async fn a_member_removes_nobody() {
    let mut setup = setup(LeaverPolicy {
      grace_minutes: 0,
      ..LeaverPolicy::default()
    })
    .await;
    let carol_id = setup.carol.client.user_id();

    setup.server.deactivate_leaver(carol_id, HOUR_SECS);
    setup.settle().await;
    assert_eq!(run(&mut setup.bob).await, LeaverRound::default());
    assert!(setup.server.is_listed_leaver(carol_id));
  }

  /// A hostile server that reports members as leavers gets at most `max_per_day` removed per admin
  /// device and day; the rest wait for an admin, and a slot frees up a day later. A restart of the
  /// app doesn't reset the count.
  #[tokio::test]
  async fn the_daily_limit_holds_the_rest() {
    let mut setup = setup(LeaverPolicy {
      grace_minutes: 0,
      max_per_day: 1,
      ..LeaverPolicy::default()
    })
    .await;
    let (bob_id, carol_id) = (setup.bob.client.user_id(), setup.carol.client.user_id());

    setup.server.deactivate_leaver(bob_id, HOUR_SECS);
    setup.server.deactivate_leaver(carol_id, HOUR_SECS);
    setup.settle().await;
    let round = run(&mut setup.admin).await;
    assert_eq!(round.removed, 1);
    assert_eq!(round.held_by_limit, 1);
    assert!(setup.server.is_listed_leaver(bob_id) ^ setup.server.is_listed_leaver(carol_id));

    // A new connection reads the list again, still at the limit.
    setup.admin = reconnect(setup.admin, &setup.clock).await;
    let round = run(&mut setup.admin).await;
    assert_eq!((round.removed, round.held_by_limit), (0, 1));

    setup.clock.advance(DAY_MS);
    assert_eq!(run(&mut setup.admin).await.removed, 1);
    assert!(!setup.server.is_listed_leaver(bob_id) && !setup.server.is_listed_leaver(carol_id));
  }
}

impl Setup {
  async fn settle(&mut self) {
    settle(&mut [&mut self.alice, &mut self.bob, &mut self.carol, &mut self.admin]).await;
  }
}

async fn setup(leavers: LeaverPolicy) -> Setup {
  let server = FakeServer::new();
  let clock = Arc::new(ManualClock::new(1_800_000_000_000));
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let admin = enroll_with_clock(&server, "Admin", &clock).await;
  let (alice_id, admin_id) = (alice.client.user_id(), admin.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (admin_id, RosterRole::Admin)])
    .await
    .unwrap();
  alice
    .client
    .publish_policy(WorkspacePolicy {
      leavers,
      ..WorkspacePolicy::default()
    })
    .await
    .unwrap();
  server.set_admin(admin_id);
  let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
  let mut setup = Setup {
    server,
    clock,
    alice,
    bob,
    carol,
    admin,
    channel,
  };
  setup.settle().await;
  // The first round after the connection: nobody is listed yet.
  assert_eq!(run(&mut setup.admin).await, LeaverRound::default());
  setup
}

async fn run(peer: &mut Peer) -> LeaverRound {
  peer.client.run_leaver_removal().await.unwrap()
}

/// A peer whose client runs on `clock`, to move its time.
async fn enroll_with_clock(server: &FakeServer, name: &str, clock: &Arc<ManualClock>) -> Peer {
  let api = server.add_user(name);
  let store = Arc::new(teamcraft_core::store::MemoryStore::new());
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

/// The app starts anew on the same store and connects again.
async fn reconnect(peer: Peer, clock: &Arc<ManualClock>) -> Peer {
  let Peer { name, api, store, .. } = peer;
  let mut client =
    teamcraft_core::Client::open(api.clone(), store.clone(), Box::new(clock.clone()), settings())
      .unwrap()
      .unwrap();
  let (gateway, _) = client.connect_gateway().await.unwrap();
  let mut peer = Peer {
    name,
    api,
    store,
    client,
    gateway,
  };
  settle(&mut [&mut peer]).await;
  peer
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

async fn commits(peer: &Peer, channel: ConversationId) -> usize {
  mls_entries(peer, channel)
    .await
    .iter()
    .filter(|entry| entry.kind == MlsMessageKind::Commit)
    .count()
}

/// Signed changes `sender` posted into the group's queue, as `reader` (a member) sees them.
async fn proposals_by(reader: &Peer, sender: &Peer, channel: ConversationId) -> usize {
  let device = sender.client.device_id();
  mls_entries(reader, channel)
    .await
    .iter()
    .filter(|entry| entry.kind == MlsMessageKind::Proposal && entry.sender_device_id == device)
    .count()
}

async fn mls_entries(peer: &Peer, channel: ConversationId) -> Vec<teamcraft_proto::MlsMessageResponse> {
  peer
    .api
    .list_mls_messages(channel, &ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .messages
}
