//! A device that is a member on the server but can't write asks the members again (issue #1878,
//! e2ee.md §4.6, "Asking again for a lost hand-over"): after a bounded wait that doubles up to a
//! cap, and on a new gateway connection. A request only makes due what a member decided or would
//! do for a join; a forged or replayed one adds nobody.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use common::{Peer, settle};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::{ReaskTrigger, is_waiting_for_device, pending_user_additions};
use teamcraft_core::clock::ManualClock;
use teamcraft_core::store::{LocalStore, Table};
use teamcraft_core::upkeep::Upkeep;
use teamcraft_core::{ClientSettings, GroupAccess};
use teamcraft_proto::{
  AddConversationMemberRequest, Base64Bytes, ConversationId, DeviceId, Id, ListMlsMessagesQuery,
  MlsMessageKind, PostMlsMessageRequest, Snowflake,
};

const NOW_MS: i64 = 1_800_000_000_000;
const RETRY: Duration = Duration::from_secs(2 * 60);
const RETRY_MAX: Duration = Duration::from_secs(8 * 60);

mod outside_the_group {
  use super::*;

  /// The prod case: Bob's first device appeared while Alice's device, the only one holding his
  /// addition to their DM, missed the event. Bob writes; after the wait his device asks, Alice's
  /// device makes its addition due, and the message goes out.
  #[tokio::test]
  async fn a_held_message_asks_again_and_the_member_holding_the_addition_lets_the_device_in() {
    let (server, clock, mut alice, mut bob, dm) = dm_with_a_lost_device_add().await;
    bob.client.send_text(dm, None, "from Bob").await.unwrap();
    let report = bob.client.flush_outbox().await.unwrap();
    assert!(report.sent == 0 && report.pending == 1, "{report:?}");
    assert!(is_waiting_for_device(bob.store.as_ref(), dm).unwrap());

    // Seen now; asked only once the wait ran out.
    assert_eq!(ask(&mut bob, ReaskTrigger::Timer).await, 0);
    clock.advance(ms(RETRY) - 1);
    assert_eq!(ask(&mut bob, ReaskTrigger::Timer).await, 0);
    clock.advance(1);
    assert_eq!(ask(&mut bob, ReaskTrigger::Timer).await, 1);
    assert_eq!(requests_from(&alice, &bob, dm, b"TCDA").await, 1);

    // Alice's device reads the request and commits the addition it held.
    alice.client.sync_mls(dm).await.unwrap();
    alice.client.apply_pending_changes().await.unwrap();
    assert!(
      pending_user_additions(alice.store.as_ref(), dm)
        .unwrap()
        .is_empty()
    );
    bob.client.full_sync().await.unwrap();
    assert_eq!(bob.client.group_access(dm).unwrap(), GroupAccess::Member);
    // The sync's flush or this one sends it.
    let report = bob.client.flush_outbox().await.unwrap();
    assert_eq!(report.pending, 0, "{report:?}");
    alice.client.full_sync().await.unwrap();
    assert!(alice.texts(dm).contains(&"from Bob".to_string()));

    // Nothing waits any more: no further request, and the note is gone.
    clock.advance(ms(RETRY_MAX));
    assert_eq!(ask(&mut bob, ReaskTrigger::Timer).await, 0);
    assert!(!is_waiting_for_device(bob.store.as_ref(), dm).unwrap());
    drop(server);
  }

  /// A member who comes online to many requests from the same device (it asked often, or the
  /// server replays them) commits the addition once.
  #[tokio::test]
  async fn many_requests_commit_the_addition_once() {
    let (_server, _clock, mut alice, mut bob, dm) = dm_with_a_lost_device_add().await;
    for _ in 0..4 {
      post_proposal(&bob, dm, device_add_payload(&bob)).await;
    }
    let before = alice.client.mls_epoch(dm).unwrap();
    alice.client.sync_mls(dm).await.unwrap();
    alice.client.apply_pending_changes().await.unwrap();
    alice.client.apply_pending_changes().await.unwrap();
    assert_eq!(alice.client.mls_epoch(dm).unwrap(), before.map(|epoch| epoch + 1));

    // Replayed after Bob is in: nothing left to make due, no commit.
    post_proposal(&bob, dm, device_add_payload(&bob)).await;
    alice.client.sync_mls(dm).await.unwrap();
    alice.client.apply_pending_changes().await.unwrap();
    assert_eq!(alice.client.mls_epoch(dm).unwrap(), before.map(|epoch| epoch + 1));
    bob.client.full_sync().await.unwrap();
    assert!(bob.client.has_group(dm).unwrap());
  }

  /// A server that floods the queue with requests for Bob, each in the name of a device it made
  /// up, wakes Alice's addition of him at most once per `rejoin_retry`, and never resets its
  /// backoff.
  #[tokio::test]
  async fn forged_requests_wake_a_waiting_addition_once_per_window_and_keep_its_backoff() {
    let (server, clock, mut alice, bob, dm) = dm_with_a_lost_device_add().await;
    let key = format!("add_user/{dm}/{}", bob.client.user_id());
    let mut record: serde_json::Value =
      serde_json::from_slice(&alice.store.get(Table::PendingChanges, &key).unwrap().unwrap()).unwrap();
    record["attempts"] = 3.into();
    alice
      .store
      .put(Table::PendingChanges, &key, &serde_json::to_vec(&record).unwrap())
      .unwrap();
    let flood = |first: i64| {
      for offset in 0..5 {
        let forged = DeviceId::from_snowflake(Snowflake::new(first + offset).unwrap());
        server.inject_proposal(dm, forged, device_add_payload(&bob));
      }
    };
    let pending = |alice: &Peer| -> serde_json::Value {
      serde_json::from_slice(&alice.store.get(Table::PendingChanges, &key).unwrap().unwrap()).unwrap()
    };

    flood(900_000_000_000_000_001);
    alice.client.sync_mls(dm).await.unwrap();
    let woken = pending(&alice);
    assert_eq!(woken["attempts"], 3, "the backoff stays");
    assert_eq!(woken["maybe_not_before_ms"], NOW_MS, "due now");

    // Back to waiting (as a failed try would leave it); more forged requests in the same window
    // don't wake it again.
    let mut waiting = woken.clone();
    waiting["maybe_not_before_ms"] = (NOW_MS + 3_600_000).into();
    alice
      .store
      .put(
        Table::PendingChanges,
        &key,
        &serde_json::to_vec(&waiting).unwrap(),
      )
      .unwrap();
    flood(900_000_000_000_000_101);
    alice.client.sync_mls(dm).await.unwrap();
    assert_eq!(pending(&alice)["maybe_not_before_ms"], NOW_MS + 3_600_000);

    // The next window wakes it once more.
    clock.advance(ms(common::settings().rejoin_retry));
    flood(900_000_000_000_000_201);
    alice.client.sync_mls(dm).await.unwrap();
    let woken = pending(&alice);
    assert_eq!(
      woken["maybe_not_before_ms"],
      NOW_MS + ms(common::settings().rejoin_retry)
    );
    assert_eq!(woken["attempts"], 3);
  }

  /// The server lists Mallory in a channel and posts a request in her device's name: no member
  /// decided to add her, so nobody does.
  #[tokio::test]
  async fn a_request_from_somebody_no_member_decided_to_add_adds_nobody() {
    let server = FakeServer::new();
    let (mut alice, mut bob, channel) = channel(&server).await;
    let mallory = Peer::enroll(&server, "Mallory").await;
    alice
      .api
      .add_member(
        channel,
        &AddConversationMemberRequest {
          user_id: mallory.client.user_id(),
          maybe_role: None,
        },
      )
      .await
      .unwrap();
    let before = alice.client.mls_epoch(channel).unwrap();
    for _ in 0..3 {
      post_proposal(&mallory, channel, device_add_payload(&mallory)).await;
      post_proposal(&mallory, channel, b"TCKR".to_vec()).await;
    }
    for peer in [&mut alice, &mut bob] {
      peer.client.sync_mls(channel).await.unwrap();
      peer.client.apply_pending_changes().await.unwrap();
      assert!(
        !peer
          .client
          .group_users(channel)
          .unwrap()
          .contains(&mallory.client.user_id())
      );
      assert!(pending_keys(peer).is_empty(), "{}: nothing queued", peer.name);
      assert_eq!(peer.client.mls_epoch(channel).unwrap(), before);
    }
  }

  /// A device with nothing to write doesn't ask on its own (a first device may be listed in many
  /// conversations).
  #[tokio::test]
  async fn a_device_that_wrote_nothing_does_not_ask() {
    let (_server, clock, alice, mut bob, dm) = dm_with_a_lost_device_add().await;
    assert_eq!(ask(&mut bob, ReaskTrigger::Timer).await, 0);
    clock.advance(ms(RETRY_MAX) * 4);
    assert_eq!(ask(&mut bob, ReaskTrigger::NewConnection).await, 0);
    assert_eq!(requests_from(&alice, &bob, dm, b"TCDA").await, 0);
  }
}

mod in_the_group {
  use super::*;

  /// The laptop joined on its own; the members' hand-overs of the current key were lost. After the
  /// wait the laptop asks, a member hands the key over again, and the held message goes out.
  #[tokio::test]
  async fn a_device_without_a_current_key_asks_again_and_a_member_hands_it_over() {
    let (server, clock, mut phone, mut bob, channel, mut laptop) = channel_with_a_lost_hand_over().await;
    laptop
      .client
      .send_text(channel, None, "from the laptop")
      .await
      .unwrap();
    assert_eq!(laptop.client.flush_outbox().await.unwrap().pending, 1);

    assert_eq!(ask(&mut laptop, ReaskTrigger::Timer).await, 0);
    clock.advance(ms(RETRY));
    assert_eq!(ask(&mut laptop, ReaskTrigger::Timer).await, 1);
    settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
    phone.client.apply_pending_changes().await.unwrap();
    bob.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut phone, &mut bob, &mut laptop]).await;

    assert!(!is_waiting_for_device(laptop.store.as_ref(), channel).unwrap());
    assert_eq!(laptop.client.flush_outbox().await.unwrap().sent, 1);
    settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
    assert!(bob.texts(channel).contains(&"from the laptop".to_string()));
    assert!(phone.texts(channel).contains(&"from the laptop".to_string()));
    drop(server);
  }

  /// Many current-key requests from the same device cost each member at most one hand-over.
  #[tokio::test]
  async fn many_current_key_requests_cost_each_member_one_hand_over() {
    let (_server, _clock, mut phone, mut bob, channel, mut laptop) = channel_with_a_lost_hand_over().await;
    let before = server_queue(&bob, channel).await.len();
    for _ in 0..4 {
      post_proposal(&laptop, channel, b"TCKR".to_vec()).await;
    }
    settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
    phone.client.apply_pending_changes().await.unwrap();
    bob.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
    assert!(!is_waiting_for_device(laptop.store.as_ref(), channel).unwrap());

    let queue = server_queue(&bob, channel).await;
    for member in [&phone, &bob] {
      let bundles = queue[before..]
        .iter()
        .filter(|entry| {
          entry.kind == MlsMessageKind::Application && entry.sender_device_id == member.client.device_id()
        })
        .count();
      assert!(bundles <= 1, "{}: {bundles} hand-overs", member.name);
    }
  }
}

mod schedule {
  use super::*;

  /// The wait doubles after each request, up to `key_reask_retry_max`.
  #[tokio::test]
  async fn requests_back_off_up_to_the_cap() {
    let (_server, clock, alice, mut bob, dm) = dm_with_a_lost_device_add().await;
    hold_a_message(&mut bob, dm).await;
    assert_eq!(ask(&mut bob, ReaskTrigger::Timer).await, 0);
    // 2, 4, 8, then 8 minutes again (the cap).
    for wait in [RETRY, RETRY * 2, RETRY * 4, RETRY_MAX, RETRY_MAX] {
      clock.advance(ms(wait) - 1);
      assert_eq!(ask(&mut bob, ReaskTrigger::Timer).await, 0, "not before {wait:?}");
      clock.advance(1);
      assert_eq!(ask(&mut bob, ReaskTrigger::Timer).await, 1, "after {wait:?}");
    }
    assert_eq!(requests_from(&alice, &bob, dm, b"TCDA").await, 5);
  }

  /// A new connection asks at once where the device waited before, but never twice within
  /// `key_reask_retry`.
  #[tokio::test]
  async fn a_new_connection_asks_at_once_but_not_twice_in_a_row() {
    let (_server, clock, alice, mut bob, dm) = dm_with_a_lost_device_add().await;
    hold_a_message(&mut bob, dm).await;
    // Seen first on a connection: the wait starts, no request yet.
    assert_eq!(ask(&mut bob, ReaskTrigger::NewConnection).await, 0);
    clock.advance(30_000);
    assert_eq!(ask(&mut bob, ReaskTrigger::NewConnection).await, 1);
    assert_eq!(ask(&mut bob, ReaskTrigger::NewConnection).await, 0);
    clock.advance(ms(RETRY));
    // The backoff would wait 4 minutes; the reconnect asks after 2.
    assert_eq!(ask(&mut bob, ReaskTrigger::Timer).await, 0);
    assert_eq!(ask(&mut bob, ReaskTrigger::NewConnection).await, 1);
    assert_eq!(requests_from(&alice, &bob, dm, b"TCDA").await, 2);
  }

  /// The app's upkeep asks on a reconnect, between its periodic checks.
  #[tokio::test]
  async fn the_upkeep_asks_on_a_reconnect() {
    let (server, clock, alice, mut bob, dm) = dm_with_a_lost_device_add().await;
    hold_a_message(&mut bob, dm).await;
    let mut upkeep = Upkeep::new(Duration::from_secs(60 * 60));
    upkeep.tick(&mut bob.client).await;
    clock.advance(30_000);
    upkeep.tick(&mut bob.client).await;
    assert_eq!(
      requests_from(&alice, &bob, dm, b"TCDA").await,
      0,
      "no reconnect, no timer"
    );

    server.drop_gateways(bob.client.user_id());
    bob.gateway = bob.client.connect_gateway().await.unwrap().0;
    settle(&mut [&mut bob]).await;
    upkeep.tick(&mut bob.client).await;
    assert_eq!(requests_from(&alice, &bob, dm, b"TCDA").await, 1);
  }
}

/// Alice opened a group DM with Carol and Bob before he set up the app (as an import device opens
/// a conversation for the people it maps); his first device appeared while Alice's gateway was
/// gone, and her full syncs had checked for it within `rejoin_retry` (the clock stands still), so
/// her addition of Bob still waits. Bob's device is in no group.
async fn dm_with_a_lost_device_add() -> (FakeServer, Arc<ManualClock>, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let clock = Arc::new(ManualClock::new(NOW_MS));
  let mut alice = Peer::enroll_on_clock(
    server.add_user("Alice"),
    "Alice",
    settings(),
    Box::new(clock.clone()),
  )
  .await;
  let carol = Peer::enroll_on_clock(
    server.add_user("Carol"),
    "Carol",
    settings(),
    Box::new(clock.clone()),
  )
  .await;
  let bob_api = server.add_user("Bob");
  let dm = alice
    .client
    .open_group_dm(&[carol.client.user_id(), bob_api.user_id()])
    .await
    .unwrap();
  alice.client.send_text(dm, None, "welcome Bob").await.unwrap();
  assert_eq!(alice.client.recheck_waiting_additions().await.unwrap(), 0);
  server.drop_gateways(alice.client.user_id());
  let mut bob = Peer::enroll_on_clock(bob_api, "Bob", settings(), Box::new(clock.clone())).await;
  bob.client.full_sync().await.unwrap();
  alice.client.full_sync().await.unwrap();
  alice.client.apply_pending_changes().await.unwrap();
  assert!(!bob.client.has_group(dm).unwrap(), "the hand-over is lost");
  assert_eq!(pending_user_additions(alice.store.as_ref(), dm).unwrap().len(), 1);
  (server, clock, alice, bob, dm)
}

/// Alice's phone and Bob share a channel; Alice's laptop joined it on its own, and both members'
/// hand-overs of the current key never reached the group.
async fn channel_with_a_lost_hand_over() -> (FakeServer, Arc<ManualClock>, Peer, Peer, ConversationId, Peer) {
  let server = FakeServer::new();
  let clock = Arc::new(ManualClock::new(NOW_MS));
  let mut phone = Peer::enroll_on_clock(
    server.add_user("Alice"),
    "Alice",
    settings(),
    Box::new(clock.clone()),
  )
  .await;
  let mut bob =
    Peer::enroll_on_clock(server.add_user("Bob"), "Bob", settings(), Box::new(clock.clone())).await;
  let channel = make_channel(&mut phone, &mut bob).await;
  phone.client.upload_key_bag().await.unwrap();
  let (client, store) = common::link_device_with_store(&mut phone, settings(), Box::new(clock.clone())).await;
  let mut client = client;
  let (gateway, _) = client.connect_gateway().await.unwrap();
  let mut laptop = Peer {
    name: "Alice laptop".into(),
    api: phone.api.clone(),
    store,
    client,
    gateway,
  };
  laptop.client.full_sync().await.unwrap();
  assert_eq!(laptop.client.group_access(channel).unwrap(), GroupAccess::Member);
  // The members merge the laptop's join and hand the key over, but the server never delivers it.
  server.hold_next_application(phone.client.user_id());
  server.hold_next_application(bob.client.user_id());
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  phone.client.apply_pending_changes().await.unwrap();
  bob.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  assert!(
    is_waiting_for_device(laptop.store.as_ref(), channel).unwrap(),
    "the hand-over is lost"
  );
  (server, clock, phone, bob, channel, laptop)
}

/// Alice and Bob in a channel with a message.
async fn channel(server: &FakeServer) -> (Peer, Peer, ConversationId) {
  let mut alice = Peer::enroll(server, "Alice").await;
  let mut bob = Peer::enroll(server, "Bob").await;
  let channel = make_channel(&mut alice, &mut bob).await;
  (alice, bob, channel)
}

async fn make_channel(owner: &mut Peer, other: &mut Peer) -> ConversationId {
  let space = owner.space().await;
  let channel = owner.client.create_channel(space, "general").await.unwrap();
  owner
    .client
    .add_member(channel, other.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut *owner, &mut *other]).await;
  owner.client.send_text(channel, None, "one").await.unwrap();
  settle(&mut [&mut *owner, &mut *other]).await;
  channel
}

fn settings() -> ClientSettings {
  ClientSettings {
    device_add_stagger: Duration::ZERO,
    key_reask_retry: RETRY,
    key_reask_retry_max: RETRY_MAX,
    ..common::settings()
  }
}

async fn hold_a_message(peer: &mut Peer, conversation: ConversationId) {
  peer.client.send_text(conversation, None, "held").await.unwrap();
  assert_eq!(peer.client.flush_outbox().await.unwrap().pending, 1);
}

async fn ask(peer: &mut Peer, trigger: ReaskTrigger) -> usize {
  peer.client.ask_again_for_keys(trigger).await.unwrap()
}

/// Requests of `sender`'s device in the conversation's queue, as `reader` sees it.
async fn requests_from(reader: &Peer, sender: &Peer, conversation: ConversationId, magic: &[u8]) -> usize {
  server_queue(reader, conversation)
    .await
    .iter()
    .filter(|entry| {
      entry.sender_device_id == sender.client.device_id() && entry.payload.as_slice().starts_with(magic)
    })
    .count()
}

async fn server_queue(
  reader: &Peer,
  conversation: ConversationId,
) -> Vec<teamcraft_proto::MlsMessageResponse> {
  reader
    .api
    .list_mls_messages(conversation, &ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .messages
}

fn device_add_payload(device: &Peer) -> Vec<u8> {
  let mut payload = b"TCDA".to_vec();
  payload.extend_from_slice(&device.client.user_id().snowflake().get().to_be_bytes());
  payload
}

/// Posts a proposal in the device's name straight to the queue, past its client's own schedule (a
/// device asking often, or a server replaying or forging entries).
async fn post_proposal(device: &Peer, conversation: ConversationId, payload: Vec<u8>) {
  let query = ListMlsMessagesQuery {
    maybe_after: None,
    maybe_limit: Some(1),
  };
  let epoch = device
    .api
    .list_mls_messages(conversation, &query)
    .await
    .unwrap()
    .current_epoch;
  let request = PostMlsMessageRequest {
    device_id: device.client.device_id(),
    epoch,
    kind: MlsMessageKind::Proposal,
    payload: Base64Bytes::new(payload),
  };
  device.api.post_mls_message(conversation, &request).await.unwrap();
}

fn pending_keys(peer: &Peer) -> Vec<String> {
  peer
    .store
    .list(Table::PendingChanges)
    .unwrap()
    .into_iter()
    .map(|(key, _)| key)
    .filter(|key| {
      key.starts_with("share_current/") || key.starts_with("add_user/") || key.starts_with("add_device/")
    })
    .collect()
}

fn ms(duration: Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap()
}
