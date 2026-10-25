//! Key package lifecycle (e2ee.md §4.3) through the fake server: expired packages are never handed
//! out, running devices renew theirs, and one device's bad package doesn't keep its user out.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::time::Duration;

use common::{Peer, settings, settle};
use teamcraft_core::ClientSettings;
use teamcraft_core::GatewayOutcome;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::{
  AddConversationMemberRequest, ClaimKeyPackagesRequest, ConversationId, ErrorCode, ListMlsMessagesQuery,
  MlsMessageKind, SpaceId, UserId,
};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

#[tokio::test]
async fn a_user_can_still_be_added_after_100_days() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  // Checks its key packages on every runner tick.
  let renew_always = ClientSettings {
    key_package_check_interval: Duration::ZERO,
    ..settings()
  };
  let mut bob = Peer::enroll_with_settings(&server, "Bob", renew_always).await;
  let space = alice.space().await;
  let channel = alice.client.create_channel(space, "general").await.unwrap();
  server.advance_clock(100 * DAY);

  // Everything Bob uploaded at enrollment has expired: none of it is handed out.
  let bob_id = bob.client.user_id();
  alice
    .api
    .add_member(
      channel,
      &AddConversationMemberRequest {
        user_id: bob_id,
        maybe_role: None,
      },
    )
    .await
    .unwrap();
  let claim = alice
    .api
    .claim_key_packages(bob_id, &ClaimKeyPackagesRequest::new(channel))
    .await
    .unwrap_err();
  assert_eq!(claim.error_code(), Some(&ErrorCode::KeyPackageExhausted));
  let status = bob.api.key_package_status(bob.client.device_id()).await.unwrap();
  assert_eq!((status.available, status.has_last_resort), (0, false));

  // Bob's running client renews them on its next tick; then Alice adds him.
  assert!(bob.client.renew_key_packages_if_due().await.unwrap());
  let status = bob.api.key_package_status(bob.client.device_id()).await.unwrap();
  assert!(status.available >= settings().key_package_target && status.has_last_resort);
  alice.client.add_member(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert!(bob.client.has_group(channel).unwrap());
  alice
    .client
    .send_text(channel, None, "welcome back")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(bob.texts(channel), ["welcome back"]);
}

#[tokio::test]
async fn renewal_waits_for_the_check_interval() {
  let server = FakeServer::new();
  let mut bob = Peer::enroll(&server, "Bob").await;
  // Enrollment just checked; the default interval is hours.
  assert!(!bob.client.renew_key_packages_if_due().await.unwrap());
}

#[tokio::test]
async fn packages_expiring_soon_are_replaced_and_the_last_resort_rotated() {
  let server = FakeServer::new();
  let mut bob = Peer::enroll(&server, "Bob").await;
  let device = bob.client.device_id();
  let before = server.stored_key_packages(device);
  // 30 days on, Bob's packages have 53 days left: inside the 56-day renewal window.
  server.advance_clock(30 * DAY);
  let status = bob.api.key_package_status(device).await.unwrap();
  assert_eq!(status.available, 0);
  assert_eq!(status.expiring_soon as usize, before);
  assert!(!status.has_last_resort);

  bob.client.top_up_key_packages().await.unwrap();
  let status = bob.api.key_package_status(device).await.unwrap();
  assert_eq!(status.available, settings().key_package_target);
  assert!(status.has_last_resort);
}

#[tokio::test]
async fn a_replaced_last_resort_package_is_deleted_on_the_device() {
  let server = FakeServer::new();
  // Without a retention the replaced package goes at once.
  let no_retention = ClientSettings {
    key_package_retention: Duration::ZERO,
    ..settings()
  };
  let mut bob = Peer::enroll_with_settings(&server, "Bob", no_retention).await;
  let enrolled = bob.client.stored_key_packages().unwrap();
  let first_last_resort = last_resort_refs(&enrolled);
  assert_eq!(first_last_resort.len(), 1);
  assert_eq!(enrolled.len(), settings().key_package_target as usize + 1);

  server.advance_clock(30 * DAY);
  bob.client.top_up_key_packages().await.unwrap();

  let stored = bob.client.stored_key_packages().unwrap();
  let last_resort = last_resort_refs(&stored);
  assert_eq!(last_resort.len(), 1, "only the new last-resort package is left");
  assert_ne!(last_resort, first_last_resort);
  // The regular packages from enrollment haven't expired yet: they stay next to the new ones.
  let regular = stored.iter().filter(|package| !package.is_last_resort).count();
  assert_eq!(regular, 2 * settings().key_package_target as usize);
}

#[tokio::test]
async fn a_lost_rotation_answer_keeps_the_old_last_resort_package() {
  let server = FakeServer::new();
  let no_retention = ClientSettings {
    key_package_retention: Duration::ZERO,
    ..settings()
  };
  let mut bob = Peer::enroll_with_settings(&server, "Bob", no_retention).await;
  bob.client.sync_welcomes().await.unwrap();
  server.advance_clock(30 * DAY);
  server.lose_next_key_package_answer();
  assert!(bob.client.top_up_key_packages().await.is_err());

  // The server may still hand out the old package: both private parts stay.
  let stored = bob.client.stored_key_packages().unwrap();
  assert_eq!(last_resort_refs(&stored).len(), 2);
}

/// After a start, spent packages go only once the welcomes were read: one may wait for a package
/// the device would otherwise delete first (the key package check runs before any sync).
#[tokio::test]
async fn spent_packages_wait_for_the_welcomes_after_a_start() {
  let server = FakeServer::new();
  let no_retention = ClientSettings {
    key_package_retention: Duration::ZERO,
    ..settings()
  };
  let bob = Peer::enroll_with_settings(&server, "Bob", no_retention.clone()).await;
  let mut client =
    teamcraft_core::Client::open(bob.api.clone(), bob.store.clone(), common::clock(), no_retention)
      .unwrap()
      .unwrap();
  server.advance_clock(30 * DAY);
  client.top_up_key_packages().await.unwrap();
  assert_eq!(last_resort_refs(&client.stored_key_packages().unwrap()).len(), 2);

  client.sync_welcomes().await.unwrap();
  client.top_up_key_packages().await.unwrap();
  assert_eq!(last_resort_refs(&client.stored_key_packages().unwrap()).len(), 1);
}

/// A welcome that comes after its key package was deleted (here a replaced last-resort package)
/// can't be opened: the device asks to be added back and is, instead of losing the conversation.
#[tokio::test]
async fn a_welcome_for_a_deleted_key_package_ends_in_an_add_back() {
  let server = FakeServer::new();
  let no_retention = ClientSettings {
    key_package_retention: Duration::ZERO,
    ..settings()
  };
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll_with_settings(&server, "Bob", no_retention).await;
  let bob_id = bob.client.user_id();
  let space = alice.space().await;
  // Bob's regular packages are used up in another channel, so the next add takes his last-resort
  // package.
  let lobby = alice.client.create_channel(space, "lobby").await.unwrap();
  alice.client.add_member(lobby, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  for _ in 1..settings().key_package_target {
    alice
      .api
      .claim_key_packages(bob_id, &ClaimKeyPackagesRequest::new(lobby))
      .await
      .unwrap();
  }
  bob.client.sync_welcomes().await.unwrap();
  let channel = alice.client.create_channel(space, "general").await.unwrap();
  alice.client.add_member(channel, bob_id).await.unwrap();

  // Before Bob reads that welcome, his last-resort package is rotated and the old one deleted.
  server.advance_clock(30 * DAY);
  let before = last_resort_refs(&bob.client.stored_key_packages().unwrap());
  bob.client.top_up_key_packages().await.unwrap();
  let after = last_resort_refs(&bob.client.stored_key_packages().unwrap());
  assert!(
    before.iter().all(|package| !after.contains(package)),
    "the used one is gone"
  );

  settle(&mut [&mut alice, &mut bob]).await;
  assert!(bob.client.has_group(channel).unwrap());
  alice.client.send_text(channel, None, "hi Bob").await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(bob.texts(channel), ["hi Bob"]);
}

/// Issue #395: a welcome sync deletes no key package before it read the last welcome. Here the
/// first welcome's package is gone, so it asks to be added back, which tops up the packages; the
/// second welcome waits for a last-resort package that top-up replaces.
#[tokio::test]
async fn no_package_is_deleted_in_the_middle_of_a_welcome_sync() {
  let server = FakeServer::new();
  let no_retention = ClientSettings {
    key_package_retention: Duration::ZERO,
    ..settings()
  };
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll_with_settings(&server, "Bob", no_retention).await;
  let bob_id = bob.client.user_id();
  let space = alice.space().await;
  let lobby = lobby_with_spent_packages(&mut alice, &mut bob, space).await;
  bob.client.sync_welcomes().await.unwrap();
  // The first welcome takes the last-resort package, which is replaced and deleted before Bob
  // reads it.
  let first = alice.client.create_channel(space, "first").await.unwrap();
  alice.client.add_member(first, bob_id).await.unwrap();
  server.advance_clock(30 * DAY);
  bob.client.top_up_key_packages().await.unwrap();
  // The second takes the new last-resort package (the regular ones are claimed elsewhere).
  claim_regular_packages(&alice, bob_id, lobby).await;
  let second = alice.client.create_channel(space, "second").await.unwrap();
  alice.client.add_member(second, bob_id).await.unwrap();
  // By now that package is due for replacement too.
  server.advance_clock(30 * DAY);

  bob.client.sync_welcomes().await.unwrap();
  // Opened at once, no add-back needed; the first one is answered by Alice.
  assert!(bob.client.has_group(second).unwrap());
  assert_eq!(proposals_by(&alice, &bob, second).await, 0);
  assert_eq!(proposals_by(&alice, &bob, first).await, 1);
  // With all welcomes read, the replaced package goes at the next check.
  bob.client.top_up_key_packages().await.unwrap();
  assert_eq!(
    last_resort_refs(&bob.client.stored_key_packages().unwrap()).len(),
    1
  );
  settle(&mut [&mut alice, &mut bob]).await;
  assert!(bob.client.has_group(first).unwrap());
}

/// Issue #395: after a reconnect (here a resumed gateway session) no package is deleted until
/// the welcomes were read again; the periodic check reads them first.
#[tokio::test]
async fn a_reconnect_waits_for_the_welcomes_before_deleting_packages() {
  let server = FakeServer::new();
  let settings = ClientSettings {
    key_package_retention: Duration::ZERO,
    key_package_check_interval: Duration::ZERO,
    ..settings()
  };
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll_with_settings(&server, "Bob", settings).await;
  let bob_id = bob.client.user_id();
  let space = alice.space().await;
  lobby_with_spent_packages(&mut alice, &mut bob, space).await;
  bob.client.sync_welcomes().await.unwrap();
  let channel = alice.client.create_channel(space, "general").await.unwrap();
  alice.client.add_member(channel, bob_id).await.unwrap();

  let (gateway, outcome) = bob.client.connect_gateway().await.unwrap();
  assert_eq!(outcome, GatewayOutcome::Resumed);
  bob.gateway = gateway;
  server.advance_clock(30 * DAY);
  bob.client.top_up_key_packages().await.unwrap();
  assert_eq!(
    last_resort_refs(&bob.client.stored_key_packages().unwrap()).len(),
    2,
    "the replaced package waits for the welcomes"
  );

  assert!(bob.client.renew_key_packages_if_due().await.unwrap());
  assert!(bob.client.has_group(channel).unwrap());
  assert_eq!(proposals_by(&alice, &bob, channel).await, 0);
  assert_eq!(
    last_resort_refs(&bob.client.stored_key_packages().unwrap()).len(),
    1
  );
}

/// Issue #395: a welcome for a deleted package of a conversation the user is no longer in is
/// dropped, without asking to be added back (the server would refuse the request).
mod welcome_after_leaving {
  use super::*;

  #[tokio::test]
  async fn listed_to_an_admin_it_is_dropped() {
    let (server, alice, mut bob, channel) = welcome_for_a_deleted_package_after_leaving().await;
    // An instance admin may still list the members.
    server.set_admin(bob.client.user_id());
    bob.client.sync_welcomes().await.unwrap();
    assert!(!bob.client.has_group(channel).unwrap());
    assert_eq!(proposals_by(&alice, &bob, channel).await, 0);
  }

  #[tokio::test]
  async fn refused_the_member_list_it_is_dropped() {
    let (_server, alice, mut bob, channel) = welcome_for_a_deleted_package_after_leaving().await;
    bob.client.sync_welcomes().await.unwrap();
    assert!(!bob.client.has_group(channel).unwrap());
    assert_eq!(proposals_by(&alice, &bob, channel).await, 0);
    // Done with: the next sync doesn't read it again.
    bob.client.sync_welcomes().await.unwrap();
  }

  /// Alice adds Bob with his last-resort package, which Bob deletes before he reads the welcome;
  /// then the server takes him out of the channel.
  async fn welcome_for_a_deleted_package_after_leaving() -> (FakeServer, Peer, Peer, ConversationId) {
    let server = FakeServer::new();
    let no_retention = ClientSettings {
      key_package_retention: Duration::ZERO,
      ..settings()
    };
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll_with_settings(&server, "Bob", no_retention).await;
    let bob_id = bob.client.user_id();
    let space = alice.space().await;
    lobby_with_spent_packages(&mut alice, &mut bob, space).await;
    bob.client.sync_welcomes().await.unwrap();
    let channel = alice.client.create_channel(space, "general").await.unwrap();
    alice.client.add_member(channel, bob_id).await.unwrap();
    server.advance_clock(30 * DAY);
    bob.client.top_up_key_packages().await.unwrap();
    server.remove_member_as_admin(channel, bob_id);
    (server, alice, bob, channel)
  }
}

/// A channel Bob is in, with all his regular packages claimed for it: the next add takes his
/// last-resort package.
async fn lobby_with_spent_packages(alice: &mut Peer, bob: &mut Peer, space: SpaceId) -> ConversationId {
  let bob_id = bob.client.user_id();
  let lobby = alice.client.create_channel(space, "lobby").await.unwrap();
  alice.client.add_member(lobby, bob_id).await.unwrap();
  settle(&mut [alice, bob]).await;
  for _ in 1..settings().key_package_target {
    alice
      .api
      .claim_key_packages(bob_id, &ClaimKeyPackagesRequest::new(lobby))
      .await
      .unwrap();
  }
  lobby
}

/// Claims every regular package the server holds for `user`.
async fn claim_regular_packages(claimer: &Peer, user: UserId, conversation: ConversationId) {
  for _ in 0..settings().key_package_target {
    claimer
      .api
      .claim_key_packages(user, &ClaimKeyPackagesRequest::new(conversation))
      .await
      .unwrap();
  }
}

/// How many proposals (rejoin requests) `sender`'s device put into the conversation's queue.
async fn proposals_by(reader: &Peer, sender: &Peer, conversation: ConversationId) -> usize {
  let device = sender.client.device_id();
  reader
    .api
    .list_mls_messages(conversation, &ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .messages
    .iter()
    .filter(|entry| entry.kind == MlsMessageKind::Proposal && entry.sender_device_id == device)
    .count()
}

fn last_resort_refs(packages: &[teamcraft_crypto::mls::StoredKeyPackage]) -> Vec<Vec<u8>> {
  packages
    .iter()
    .filter(|package| package.is_last_resort)
    .map(|package| package.hash_ref.clone())
    .collect()
}

mod one_bad_device {
  use super::*;

  #[tokio::test]
  async fn a_revoked_device_is_left_out_and_the_good_one_added() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let laptop = common::link_device(&mut bob).await;
    let laptop_id = laptop.device_id();
    // The server keeps serving the revoked laptop's packages; Bob's signed device list drops it.
    server.ignore_revocations();
    bob.client.revoke_device(laptop_id).await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    let channel = channel_with_bob(&mut alice, &mut bob).await;
    let devices = alice.client.group_device_ids(channel).unwrap();
    assert!(devices.contains(&bob.client.device_id()));
    assert!(!devices.contains(&laptop_id));
  }

  #[tokio::test]
  async fn a_broken_package_is_left_out_and_the_good_one_added() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let laptop = common::link_device(&mut bob).await;
    server.corrupt_key_packages(laptop.device_id());

    let channel = channel_with_bob(&mut alice, &mut bob).await;
    let devices = alice.client.group_device_ids(channel).unwrap();
    assert!(devices.contains(&bob.client.device_id()));
    assert!(!devices.contains(&laptop.device_id()));
  }

  /// Alice creates a channel and adds Bob; Bob joins with what was added.
  async fn channel_with_bob(alice: &mut Peer, bob: &mut Peer) -> teamcraft_proto::ConversationId {
    let space = alice.space().await;
    let channel = alice.client.create_channel(space, "general").await.unwrap();
    alice
      .client
      .add_member(channel, bob.client.user_id())
      .await
      .unwrap();
    settle(&mut [alice, bob]).await;
    assert!(bob.client.has_group(channel).unwrap());
    channel
  }
}
