//! Key package lifecycle (e2ee.md §4.3) through the fake server: expired packages are never handed
//! out, running devices renew theirs, and one device's bad package doesn't keep its user out.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::time::Duration;

use common::{Peer, settings, settle};
use teamcraft_core::ClientSettings;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::{AddConversationMemberRequest, ClaimKeyPackagesRequest, ErrorCode};

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
