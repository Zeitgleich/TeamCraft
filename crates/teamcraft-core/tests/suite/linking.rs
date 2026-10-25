//! Linking a new device to an existing one over the relay (e2ee.md §5), through the fake server.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use common::{Peer, clock, linked_options, settings, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::NewDeviceLink;
use teamcraft_core::store::MemoryStore;
use teamcraft_core::{CoreError, LinkOffer, PendingEnrollment};

#[tokio::test]
async fn new_laptop_is_linked_by_scanning_its_code() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "linked")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  alice
    .client
    .send_text(channel, None, "from before the laptop")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  // The laptop shows the code; the phone scans it. Both connect to the relay at the same time.
  let laptop_api = alice.api.clone();
  let offer = LinkOffer::generate();
  let code = offer.to_code();
  let (laptop_side, phone_side) = tokio::join!(
    NewDeviceLink::connect(&laptop_api, &offer),
    alice.client.start_link_approval(&code)
  );
  let (laptop_side, phone_side) = (laptop_side.unwrap(), phone_side.unwrap());
  assert_eq!(laptop_side.short_code(), phone_side.short_code());

  // The user confirmed the codes on the phone; the phone reports success once the laptop confirmed.
  let new_store = Arc::new(MemoryStore::new());
  let (approved, received) = tokio::join!(
    alice.client.complete_link_approval(phone_side),
    laptop_side.receive_secrets(&new_store, "Test device")
  );
  approved.unwrap();
  let secrets = received.unwrap();
  let link_options = linked_options(secrets);
  let mut laptop = PendingEnrollment::new()
    .complete(laptop_api, new_store, clock(), settings(), link_options)
    .await
    .unwrap();
  assert_eq!(laptop.identity_fingerprint(), alice.client.identity_fingerprint());

  alice.client.add_missing_devices(channel).await.unwrap();
  laptop.sync_welcomes().await.unwrap();
  laptop.load_older_messages(channel).await.unwrap();
  assert!(laptop.has_group(channel).unwrap());
  assert_eq!(laptop.undecrypted_count().unwrap(), 0);
}

#[tokio::test]
async fn linked_device_refuses_to_create_an_identity() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let laptop_api = alice.api.clone();
  let offer = LinkOffer::generate();
  let code = offer.to_code();
  let (laptop_side, phone_side) = tokio::join!(
    NewDeviceLink::connect(&laptop_api, &offer),
    alice.client.start_link_approval(&code)
  );
  let new_store = Arc::new(MemoryStore::new());
  let (approved, received) = tokio::join!(
    alice.client.complete_link_approval(phone_side.unwrap()),
    laptop_side.unwrap().receive_secrets(&new_store, "Test device")
  );
  approved.unwrap();
  let secrets = received.unwrap();

  // The server claims the account has no identity: a new one wrapped with a passphrase the
  // server could guess must not be created (security review B1).
  server.forget_identity_key(alice.client.user_id());
  let result = PendingEnrollment::new()
    .complete(
      laptop_api,
      new_store,
      clock(),
      settings(),
      linked_options(secrets),
    )
    .await;
  assert!(matches!(result, Err(CoreError::IdentityMissing)));
}

#[tokio::test]
async fn someone_else_cannot_take_over_the_relay() {
  let server = FakeServer::new();
  let alice = Peer::enroll(&server, "Alice").await;
  let mallory = Peer::enroll(&server, "Mallory").await;
  let offer = LinkOffer::generate();
  let code = offer.to_code();
  // Alice's new device waits on the relay; Mallory got hold of the code. The relay only joins two
  // devices of one user, so Mallory is turned away and the laptop never gets a handshake.
  let laptop_api = alice.api.clone();
  let waiting = tokio::time::timeout(
    Duration::from_millis(200),
    NewDeviceLink::connect(&laptop_api, &offer),
  );
  let (laptop_side, mallory_side) = tokio::join!(waiting, mallory.client.start_link_approval(&code));
  assert!(mallory_side.is_err());
  assert!(laptop_side.is_err(), "no handshake without the right peer");
}

#[tokio::test(start_paused = true)]
async fn a_lost_transfer_frame_fails_on_both_devices() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let laptop_api = alice.api.clone();
  let offer = LinkOffer::generate();
  let code = offer.to_code();
  let (laptop_side, phone_side) = tokio::join!(
    NewDeviceLink::connect(&laptop_api, &offer),
    alice.client.start_link_approval(&code)
  );
  // The frame with the keys never arrives. The phone waits for the receipt, gives up and leaves the
  // relay; the laptop learns that the transfer is not coming.
  server.lose_relay_frames(1);
  let new_store = Arc::new(MemoryStore::new());
  let (approved, received) = tokio::join!(
    alice.client.complete_link_approval(phone_side.unwrap()),
    laptop_side.unwrap().receive_secrets(&new_store, "Test device")
  );
  assert!(matches!(approved, Err(CoreError::LinkNotConfirmed)));
  assert!(received.is_err());
}
