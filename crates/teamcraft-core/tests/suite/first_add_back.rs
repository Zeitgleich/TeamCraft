//! Issue #400: a new member's device that can't open the welcome of the addition that shared the
//! full history with them (its key package deleted before the welcome was read) is added back, and
//! the member that adds it back seals that history to it again. Nobody gets more than a signed
//! addition allowed, and nobody but the device added back (e2ee.md §4.3.1, §4.6).
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use common::{Peer, linked_options, settings, settle};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::{FakeServer, NetworkCut};
use teamcraft_core::store::{LocalStore, MemoryStore, Table};
use teamcraft_core::{ClientSettings, LinkOffer, PendingEnrollment};
use teamcraft_proto::{
  Base64Bytes, ClaimKeyPackagesRequest, ConversationId, HistoryPolicy, ListMlsMessagesQuery, MlsMessageKind,
  MlsMessageResponse, PostMlsMessageRequest, UserId,
};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
/// Longer than the members' `rejoin_cooldown` (30 minutes).
const PAST_COOLDOWN: Duration = Duration::from_secs(31 * 60);
/// What was written before Uma's addition, under two keys Gus (added from his join on) never had.
const HISTORY: [&str; 2] = ["with Carol", "without Carol"];
/// Starts a rejoin request's proposal payload.
const REJOIN_REQUEST_MAGIC: &[u8] = b"TCRJ";

/// The history of the addition comes back with the add-back.
mod shared_again {
  use super::*;

  /// Uma is added with the full history, but her device's key package is gone before she reads
  /// the welcome. Alice adds her back and seals the history to her again; Gus, who was in the group
  /// all along from his join on, still has no key from before it.
  #[tokio::test]
  async fn a_lost_first_welcome_still_brings_the_history() {
    let server = FakeServer::new();
    let (mut alice, mut gus, channel) = channel_with_history(&server).await;
    let mut uma = newcomer(&server, "Uma").await;
    spend_regular_packages(&mut alice, &mut [&mut uma]).await;

    add_with_lost_welcomes(&server, &mut alice, &mut [&mut uma], channel, HistoryPolicy::Full).await;
    settle(&mut [&mut alice, &mut gus, &mut uma]).await;

    assert_eq!(rejoin_requests(&alice, &uma, channel).await.len(), 1);
    assert!(uma.client.has_group(channel).unwrap());
    assert_has_history(&alice, &uma, channel).await;
    assert_has_no_history(&alice, &gus, channel).await;
    uma.client.full_sync().await.unwrap();
    for text in HISTORY {
      assert!(uma.texts(channel).contains(&text.to_string()), "{text}");
    }
  }

  /// Both of Uma's devices were in the addition and neither could open its welcome, so no device of
  /// hers had the history to pass on through her key bag: each gets it when it is added back.
  #[tokio::test]
  async fn every_device_of_the_addition_gets_the_history() {
    let server = FakeServer::new();
    let (mut alice, mut gus, channel) = channel_with_history(&server).await;
    let mut phone = newcomer(&server, "Uma").await;
    let mut laptop = link_newcomer_device(&mut phone).await;
    spend_regular_packages(&mut alice, &mut [&mut phone, &mut laptop]).await;

    add_with_lost_welcomes(
      &server,
      &mut alice,
      &mut [&mut phone, &mut laptop],
      channel,
      HistoryPolicy::Full,
    )
    .await;
    settle(&mut [&mut alice, &mut gus, &mut phone, &mut laptop]).await;

    for device in [&mut phone, &mut laptop] {
      assert!(device.client.has_group(channel).unwrap(), "{}", device.name);
      assert_has_history(&alice, device, channel).await;
    }
    assert_has_no_history(&alice, &gus, channel).await;
  }

  /// Gus, added from his join on before Uma's full-history addition, answers Uma's request: he saw
  /// the addition, but his ring has nothing from before his own join. Alice, whose record starts
  /// before his, follows up with the history.
  #[tokio::test]
  async fn an_older_member_follows_up_a_committer_without_the_history() {
    let server = FakeServer::new();
    let (mut alice, mut gus, channel) = channel_with_history_and(&server, settings(), quick()).await;
    let mut uma = newcomer(&server, "Uma").await;
    spend_regular_packages(&mut alice, &mut [&mut uma]).await;
    add_with_lost_welcomes(&server, &mut alice, &mut [&mut uma], channel, HistoryPolicy::Full).await;
    // Only Gus and Uma are online: Gus adds her back.
    settle(&mut [&mut gus, &mut uma]).await;
    assert!(uma.client.has_group(channel).unwrap());
    assert_has_no_history(&alice, &uma, channel).await;

    settle(&mut [&mut alice, &mut gus, &mut uma]).await;
    alice.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut gus, &mut uma]).await;

    assert_has_history(&alice, &uma, channel).await;
    assert_has_no_history(&alice, &gus, channel).await;
  }

  /// Uma, added from her join on and then removed, is added again with the full history; both
  /// welcomes are lost. The second addition is what counts.
  #[tokio::test]
  async fn an_addition_with_the_full_history_after_a_removal_counts() {
    let server = FakeServer::new();
    let (mut alice, mut gus, channel) = channel_with_history(&server).await;
    let mut uma = newcomer(&server, "Uma").await;
    let uma_id = uma.client.user_id();
    spend_regular_packages(&mut alice, &mut [&mut uma]).await;
    add_with_lost_welcomes(
      &server,
      &mut alice,
      &mut [&mut uma],
      channel,
      HistoryPolicy::FromJoin,
    )
    .await;
    settle(&mut [&mut alice, &mut gus, &mut uma]).await;
    assert_has_no_history(&alice, &uma, channel).await;
    alice.client.remove_member(channel, uma_id).await.unwrap();
    settle(&mut [&mut alice, &mut gus, &mut uma]).await;
    server.advance_clock(PAST_COOLDOWN);

    spend_regular_packages(&mut alice, &mut [&mut uma]).await;
    add_with_lost_welcomes(&server, &mut alice, &mut [&mut uma], channel, HistoryPolicy::Full).await;
    settle(&mut [&mut alice, &mut gus, &mut uma]).await;

    assert_eq!(
      rejoin_requests(&alice, &uma, channel).await.len(),
      2,
      "both welcomes lost"
    );
    assert!(uma.client.has_group(channel).unwrap());
    assert_has_history(&alice, &uma, channel).await;
  }

  /// Uma's device read its lost welcome over the server's API while the same welcome still waited
  /// on its gateway connection, so it asks a second time, after Alice already added it back and
  /// sealed the history. Joining from the welcome, it reads the group's queue from the welcome on
  /// and doesn't skip the keys that came before its second request.
  #[tokio::test]
  async fn a_device_that_asked_twice_still_reads_the_history() {
    let server = FakeServer::new();
    let (mut alice, mut gus, channel) = channel_with_history(&server).await;
    let mut uma = newcomer(&server, "Uma").await;
    spend_regular_packages(&mut alice, &mut [&mut uma]).await;
    add_with_lost_welcomes(&server, &mut alice, &mut [&mut uma], channel, HistoryPolicy::Full).await;
    uma.client.sync_welcomes().await.unwrap();
    settle(&mut [&mut alice, &mut gus]).await;

    settle(&mut [&mut alice, &mut gus, &mut uma]).await;

    assert_eq!(rejoin_requests(&alice, &uma, channel).await.len(), 2);
    assert!(uma.client.has_group(channel).unwrap());
    assert_has_history(&alice, &uma, channel).await;
  }
}

/// Nothing beyond what the signed addition shared, and nobody else.
mod nothing_more {
  use super::*;

  /// Added from her join on, Uma gets no key from before her addition when she is added back.
  #[tokio::test]
  async fn a_lost_first_welcome_of_an_addition_from_the_join_on_brings_no_history() {
    let server = FakeServer::new();
    let (mut alice, mut gus, channel) = channel_with_history(&server).await;
    let mut uma = newcomer(&server, "Uma").await;
    spend_regular_packages(&mut alice, &mut [&mut uma]).await;

    add_with_lost_welcomes(
      &server,
      &mut alice,
      &mut [&mut uma],
      channel,
      HistoryPolicy::FromJoin,
    )
    .await;
    settle(&mut [&mut alice, &mut gus, &mut uma]).await;

    assert_eq!(rejoin_requests(&alice, &uma, channel).await.len(), 1);
    assert_has_no_history(&alice, &uma, channel).await;
    alice
      .client
      .send_text(channel, None, "after the add-back")
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut gus, &mut uma]).await;
    assert_eq!(uma.texts(channel), ["after the add-back"]);
  }

  /// The server shows Uma as a guest by the time she is added back: guests never get history, so
  /// nothing from before her addition is shared again (the server's word can only lower it).
  #[tokio::test]
  async fn a_member_shown_as_a_guest_since_gets_no_history_again() {
    let server = FakeServer::new();
    let (mut alice, mut gus, channel) = channel_with_history(&server).await;
    let mut uma = newcomer(&server, "Uma").await;
    spend_regular_packages(&mut alice, &mut [&mut uma]).await;
    add_with_lost_welcomes(&server, &mut alice, &mut [&mut uma], channel, HistoryPolicy::Full).await;
    server.show_as_guest(uma.client.user_id());
    forget_cached_user(&alice, uma.client.user_id());

    settle(&mut [&mut alice, &mut gus, &mut uma]).await;

    assert_eq!(rejoin_requests(&alice, &uma, channel).await.len(), 1);
    assert!(uma.client.has_group(channel).unwrap());
    assert_has_no_history(&alice, &uma, channel).await;
  }

  /// Uma's laptop, linked after her addition and before her phone had any of the history, joins as
  /// a further device. Added back, the laptop gets only the keys of the epochs it was a member in:
  /// it wasn't in the addition. The phone, added back afterwards, gets the history (which reaches
  /// the laptop through Uma's key bag from then on).
  #[tokio::test]
  async fn a_further_device_gets_no_history_of_an_addition_it_was_not_in() {
    let server = FakeServer::new();
    let (mut alice, mut gus, channel) = channel_with_history(&server).await;
    let mut phone = newcomer(&server, "Uma").await;
    let uma_id = phone.client.user_id();
    spend_regular_packages(&mut alice, &mut [&mut phone]).await;
    add_with_lost_welcomes(
      &server,
      &mut alice,
      &mut [&mut phone],
      channel,
      HistoryPolicy::Full,
    )
    .await;
    // The phone's request to be added back waits on the server.
    server.hold_next_proposal(uma_id);
    settle(&mut [&mut phone]).await;
    assert!(!phone.client.has_group(channel).unwrap());
    let mut laptop = link_newcomer_device(&mut phone).await;
    laptop.client.full_sync().await.unwrap();
    settle(&mut [&mut alice, &mut gus, &mut laptop]).await;
    assert!(
      laptop.client.has_group(channel).unwrap(),
      "joined as a further device"
    );

    // The laptop refuses the commit that adds Dave, falls out and is added back.
    let mut dave = Peer::enroll(&server, "Dave").await;
    server.garble_next_commit_for(uma_id);
    alice
      .client
      .add_member_with_history(channel, dave.client.user_id(), Some(HistoryPolicy::FromJoin))
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut gus, &mut laptop, &mut dave]).await;
    assert_eq!(rejoin_requests(&alice, &laptop, channel).await.len(), 1);
    assert!(laptop.client.has_group(channel).unwrap());
    assert_eq!(
      laptop.client.mls_epoch(channel).unwrap(),
      alice.client.mls_epoch(channel).unwrap()
    );
    assert_has_no_history(&alice, &laptop, channel).await;

    server.release_held_messages();
    settle(&mut [&mut alice, &mut gus, &mut phone, &mut laptop, &mut dave]).await;
    assert_eq!(rejoin_requests(&alice, &phone, channel).await.len(), 1);
    assert_has_history(&alice, &phone, channel).await;
    assert_has_no_history(&alice, &dave, channel).await;
  }

  /// The server holds Uma's request back until she was removed and added again from her join on,
  /// then delivers it. A member adds her device back (the request is still fresh), but the history
  /// of the first addition went with the leaf it shared it with.
  #[tokio::test]
  async fn a_request_delivered_after_a_removal_brings_no_history() {
    let server = FakeServer::new();
    let (mut alice, mut gus, channel) = channel_with_history(&server).await;
    let mut uma = newcomer(&server, "Uma").await;
    let uma_id = uma.client.user_id();
    spend_regular_packages(&mut alice, &mut [&mut uma]).await;
    add_with_lost_welcomes(&server, &mut alice, &mut [&mut uma], channel, HistoryPolicy::Full).await;
    server.hold_next_proposal(uma_id);
    settle(&mut [&mut uma]).await;
    assert!(!uma.client.has_group(channel).unwrap());

    alice.client.remove_member(channel, uma_id).await.unwrap();
    settle(&mut [&mut alice, &mut gus, &mut uma]).await;
    server.advance_clock(Duration::from_secs(10 * 60));
    alice
      .client
      .add_member_with_history(channel, uma_id, Some(HistoryPolicy::FromJoin))
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut gus, &mut uma]).await;
    assert!(uma.client.has_group(channel).unwrap());
    let epoch = alice.client.mls_epoch(channel).unwrap().unwrap();

    server.advance_clock(PAST_COOLDOWN);
    server.release_held_messages();
    settle(&mut [&mut alice, &mut gus, &mut uma]).await;

    assert!(
      alice.client.mls_epoch(channel).unwrap().unwrap() > epoch,
      "the old request was answered"
    );
    assert!(uma.client.has_group(channel).unwrap());
    assert_has_no_history(&alice, &uma, channel).await;
    alice
      .client
      .send_text(channel, None, "welcome back")
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut gus, &mut uma]).await;
    assert!(uma.texts(channel).contains(&"welcome back".to_string()));
  }

  /// After Uma was added back, the server posts her request again, and forged copies of it (a new
  /// request ID under the old signature, or cut short): nobody adds her back again or seals keys
  /// to her again.
  #[tokio::test]
  async fn a_replayed_or_forged_request_shares_nothing_again() {
    let server = FakeServer::new();
    let (mut alice, mut gus, channel) = channel_with_history(&server).await;
    let mut uma = newcomer(&server, "Uma").await;
    spend_regular_packages(&mut alice, &mut [&mut uma]).await;
    add_with_lost_welcomes(&server, &mut alice, &mut [&mut uma], channel, HistoryPolicy::Full).await;
    settle(&mut [&mut alice, &mut gus, &mut uma]).await;
    assert_has_history(&alice, &uma, channel).await;
    let epoch = alice.client.mls_epoch(channel).unwrap();
    let before = commits_and_keys(&alice, channel).await;

    let request = rejoin_requests(&alice, &uma, channel).await.remove(0);
    let genuine = request.payload.as_slice().to_vec();
    let mut forged = genuine.clone();
    // The request ID follows the magic and the version byte.
    for byte in &mut forged[5..21] {
      *byte ^= 0x5a;
    }
    let truncated = genuine[..genuine.len() / 2].to_vec();
    server.advance_clock(PAST_COOLDOWN);
    for payload in [genuine, forged, truncated] {
      post_proposal(&uma, channel, payload).await;
    }
    settle(&mut [&mut alice, &mut gus, &mut uma]).await;
    alice.client.apply_pending_changes().await.unwrap();
    gus.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut gus, &mut uma]).await;

    assert_eq!(alice.client.mls_epoch(channel).unwrap(), epoch);
    assert_eq!(commits_and_keys(&alice, channel).await, before);
  }

  /// Bob adds Uma back, but his history share doesn't get out and waits. Before he sends it again,
  /// Alice removes Uma and adds her again from her join on. Bob sends from what he knew: the epoch
  /// conflict catches him up, and the share stops before any of it reaches Uma's new leaf (#403).
  #[tokio::test]
  async fn a_share_the_group_moved_past_while_sending_stops() {
    let server = FakeServer::new();
    let (mut alice, mut bob, channel) = channel_with_history_and_bob(&server).await;
    let mut uma = newcomer(&server, "Uma").await;
    let uma_id = uma.client.user_id();
    spend_regular_packages(&mut alice, &mut [&mut uma]).await;
    add_with_lost_welcomes(&server, &mut alice, &mut [&mut uma], channel, HistoryPolicy::Full).await;
    settle(&mut [&mut uma]).await;
    bob.client.sync_mls(channel).await.unwrap();
    server.cut_next_application(bob.client.user_id(), NetworkCut::BeforeRequest);
    bob.client.apply_pending_changes().await.unwrap();
    assert_has_no_history(&alice, &uma, channel).await;

    // Alice saw none of it.
    alice.client.remove_member(channel, uma_id).await.unwrap();
    server.advance_clock(Duration::from_secs(10 * 60));
    alice
      .client
      .add_member_with_history(channel, uma_id, Some(HistoryPolicy::FromJoin))
      .await
      .unwrap();
    bob.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut uma]).await;

    assert!(uma.client.has_group(channel).unwrap());
    assert_eq!(
      uma.client.mls_epoch(channel).unwrap(),
      alice.client.mls_epoch(channel).unwrap()
    );
    assert_has_no_history(&alice, &uma, channel).await;
  }

  /// The server doesn't say who Uma is when Alice adds her back, so Alice can't tell whether she
  /// may get the full history: she seals the keys of the epochs Uma's device was a member in, as
  /// for any device added back, and nothing from before the addition.
  #[tokio::test]
  async fn a_user_the_server_does_not_name_gets_the_missed_keys_only() {
    let server = FakeServer::new();
    let (mut alice, mut gus, channel) = channel_with_history(&server).await;
    let mut uma = newcomer(&server, "Uma").await;
    let mut dave = Peer::enroll(&server, "Dave").await;
    spend_regular_packages(&mut alice, &mut [&mut uma]).await;
    add_with_lost_welcomes(&server, &mut alice, &mut [&mut uma], channel, HistoryPolicy::Full).await;
    // A new key while Uma's device waits for her add-back: Dave from his join on.
    alice
      .client
      .add_member_with_history(channel, dave.client.user_id(), Some(HistoryPolicy::FromJoin))
      .await
      .unwrap();
    alice
      .client
      .send_text(channel, None, "while Uma waited")
      .await
      .unwrap();
    // Uma's device hasn't read its welcome yet.
    settle(&mut [&mut alice, &mut gus, &mut dave]).await;
    server.hide_user(uma.client.user_id());
    forget_cached_user(&alice, uma.client.user_id());

    // Only Alice and Uma are online: what Uma gets comes from Alice.
    settle(&mut [&mut alice, &mut uma]).await;

    assert!(uma.client.has_group(channel).unwrap());
    let waited = alice.content_key_of(channel, "while Uma waited").await;
    assert!(uma.client.has_content_key(channel, &waited), "the keys it missed");
    assert_has_no_history(&alice, &uma, channel).await;
  }
}

/// Alice's channel with Gus, who was added from his join on, and two keys he never had: "with
/// Carol", Carol's removal (a new key), "without Carol", Gus's addition (another new key). Alice
/// adds devices back without waiting for her rank.
async fn channel_with_history(server: &FakeServer) -> (Peer, Peer, ConversationId) {
  channel_with_history_and(server, quick(), settings()).await
}

/// [`channel_with_history`] with Alice's and Gus's client settings.
async fn channel_with_history_and(
  server: &FakeServer,
  alice_settings: ClientSettings,
  gus_settings: ClientSettings,
) -> (Peer, Peer, ConversationId) {
  let mut alice = Peer::enroll_with_settings(server, "Alice", alice_settings).await;
  let mut carol = Peer::enroll(server, "Carol").await;
  let mut gus = Peer::enroll_with_settings(server, "Gus", gus_settings).await;
  let space = alice.space().await;
  let channel = alice.client.create_channel(space, "general").await.unwrap();
  alice
    .client
    .add_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut carol]).await;
  alice.client.send_text(channel, None, HISTORY[0]).await.unwrap();
  settle(&mut [&mut alice, &mut carol]).await;
  alice
    .client
    .remove_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut carol]).await;
  alice.client.send_text(channel, None, HISTORY[1]).await.unwrap();
  alice
    .client
    .add_member_with_history(channel, gus.client.user_id(), Some(HistoryPolicy::FromJoin))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut gus]).await;
  (alice, gus, channel)
}

/// Alice's channel with Bob, a member with the full history from its start who adds devices back
/// without waiting for his rank, and the two keys of [`channel_with_history`]: "with Carol",
/// Carol's removal, "without Carol".
async fn channel_with_history_and_bob(server: &FakeServer) -> (Peer, Peer, ConversationId) {
  let mut alice = Peer::enroll(server, "Alice").await;
  let mut bob = Peer::enroll_with_settings(server, "Bob", quick()).await;
  let mut carol = Peer::enroll(server, "Carol").await;
  let space = alice.space().await;
  let channel = alice.client.create_channel(space, "general").await.unwrap();
  alice
    .client
    .add_member_with_history(channel, bob.client.user_id(), Some(HistoryPolicy::Full))
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  alice.client.send_text(channel, None, HISTORY[0]).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  alice
    .client
    .remove_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  alice.client.send_text(channel, None, HISTORY[1]).await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert_has_history(&alice, &bob, channel).await;
  (alice, bob, channel)
}

/// A user's first device, which deletes the private part of a replaced key package at once.
async fn newcomer(server: &FakeServer, name: &str) -> Peer {
  Peer::enroll_with_settings(server, name, no_retention()).await
}

/// Links a further device of `phone`'s user (the phone approves), like the phone without
/// retention for replaced key packages, and connects it.
async fn link_newcomer_device(phone: &mut Peer) -> Peer {
  let api = phone.api.clone();
  let offer = LinkOffer::generate();
  let code = offer.to_code();
  let (new_side, phone_side) = tokio::join!(
    teamcraft_core::client::NewDeviceLink::connect(&api, &offer),
    phone.client.start_link_approval(&code)
  );
  let store = Arc::new(MemoryStore::new());
  let (approved, secrets) = tokio::join!(
    phone.client.complete_link_approval(phone_side.unwrap()),
    new_side.unwrap().receive_secrets(&store, "Test device")
  );
  approved.unwrap();
  let mut client = PendingEnrollment::new()
    .complete(
      api.clone(),
      store.clone(),
      common::clock(),
      no_retention(),
      linked_options(secrets.unwrap()),
    )
    .await
    .unwrap();
  let (gateway, _) = client.connect_gateway().await.unwrap();
  Peer {
    name: format!("{} laptop", phone.name),
    api,
    store,
    client,
    gateway,
  }
}

/// A member that adds devices back (and follows up) without waiting for its rank.
fn quick() -> ClientSettings {
  ClientSettings {
    device_add_stagger: Duration::ZERO,
    ..settings()
  }
}

fn no_retention() -> ClientSettings {
  ClientSettings {
    key_package_retention: Duration::ZERO,
    ..settings()
  }
}

/// Puts the devices' user in a lobby with Alice and claims their regular key packages there, so
/// the next addition takes each device's last-resort package. The devices read their welcomes,
/// which lets them delete spent packages from then on.
async fn spend_regular_packages(alice: &mut Peer, devices: &mut [&mut Peer]) {
  let user = devices[0].client.user_id();
  let space = alice.space().await;
  let lobby = alice.client.create_channel(space, "lobby").await.unwrap();
  alice.client.add_member(lobby, user).await.unwrap();
  for device in devices.iter_mut() {
    device.client.sync_welcomes().await.unwrap();
  }
  for _ in 0..20 {
    let claimed = alice
      .api
      .claim_key_packages(user, &ClaimKeyPackagesRequest::new(lobby))
      .await
      .unwrap();
    if claimed.packages.iter().all(|package| package.is_last_resort) {
      return;
    }
  }
  panic!("regular key packages left");
}

/// Alice adds the devices' user with `history`; before the devices read the welcome, each one's
/// last-resort package is replaced on the server and deleted (e2ee.md §4.3.1).
async fn add_with_lost_welcomes(
  server: &FakeServer,
  alice: &mut Peer,
  devices: &mut [&mut Peer],
  channel: ConversationId,
  history: HistoryPolicy,
) {
  let user = devices[0].client.user_id();
  alice
    .client
    .add_member_with_history(channel, user, Some(history))
    .await
    .unwrap();
  server.advance_clock(30 * DAY);
  for device in devices.iter_mut() {
    let before = last_resort_refs(device);
    device.client.top_up_key_packages().await.unwrap();
    let after = last_resort_refs(device);
    assert!(
      before.iter().all(|package| !after.contains(package)),
      "{}: the used package is gone",
      device.name
    );
  }
}

/// Drops what `peer`'s device cached about `user`, so it asks the server again.
fn forget_cached_user(peer: &Peer, user: UserId) {
  peer.store.delete(Table::Users, &user.to_string()).unwrap();
}

async fn assert_has_history(alice: &Peer, device: &Peer, channel: ConversationId) {
  for text in HISTORY {
    let key = alice.content_key_of(channel, text).await;
    assert!(
      device.client.has_content_key(channel, &key),
      "{}: {text}",
      device.name
    );
  }
}

async fn assert_has_no_history(alice: &Peer, device: &Peer, channel: ConversationId) {
  for text in HISTORY {
    let key = alice.content_key_of(channel, text).await;
    assert!(
      !device.client.has_content_key(channel, &key),
      "{}: {text}",
      device.name
    );
  }
}

/// The rejoin requests `sender`'s device put into the conversation's queue.
async fn rejoin_requests(reader: &Peer, sender: &Peer, channel: ConversationId) -> Vec<MlsMessageResponse> {
  let device = sender.client.device_id();
  queue(reader, channel)
    .await
    .into_iter()
    .filter(|entry| {
      entry.kind == MlsMessageKind::Proposal
        && entry.sender_device_id == device
        && entry.payload.as_slice().starts_with(REJOIN_REQUEST_MAGIC)
    })
    .collect()
}

/// How many commits and application messages (content keys) the conversation's queue holds.
async fn commits_and_keys(reader: &Peer, channel: ConversationId) -> (usize, usize) {
  let entries = queue(reader, channel).await;
  let count = |kind: MlsMessageKind| entries.iter().filter(|entry| entry.kind == kind).count();
  (count(MlsMessageKind::Commit), count(MlsMessageKind::Application))
}

async fn queue(reader: &Peer, channel: ConversationId) -> Vec<MlsMessageResponse> {
  reader
    .api
    .list_mls_messages(channel, &ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .messages
}

/// Posts `payload` as a proposal of `device` in the group's current epoch, as the server could.
async fn post_proposal(device: &Peer, channel: ConversationId, payload: Vec<u8>) {
  let query = ListMlsMessagesQuery {
    maybe_after: None,
    maybe_limit: Some(1),
  };
  let epoch = device
    .api
    .list_mls_messages(channel, &query)
    .await
    .unwrap()
    .current_epoch;
  let request = PostMlsMessageRequest {
    device_id: device.client.device_id(),
    epoch,
    kind: MlsMessageKind::Proposal,
    payload: Base64Bytes::new(payload),
  };
  device.api.post_mls_message(channel, &request).await.unwrap();
}

fn last_resort_refs(device: &Peer) -> Vec<Vec<u8>> {
  device
    .client
    .stored_key_packages()
    .unwrap()
    .into_iter()
    .filter(|package| package.is_last_resort)
    .map(|package| package.hash_ref)
    .collect()
}
