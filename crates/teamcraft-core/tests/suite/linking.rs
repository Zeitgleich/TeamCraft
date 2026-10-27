//! Linking a new device to an existing one over the relay (e2ee.md §5), through the fake server.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use common::{Peer, clock, linked_options, settings, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::{LinkAnnouncement, NewDeviceLink};
use teamcraft_core::store::MemoryStore;
use teamcraft_core::{CoreError, LinkOffer, PendingEnrollment};
use teamcraft_proto::DeviceLinkPlatform;

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

/// Issue #415: the wait for the new device's receipt holds the relay, not the client. The apps run it
/// on a task of their own ([`LinkReceipt::detach`]); these tests run it the same way, on a local
/// set, while the "loop" goes on with the client. Time is tokio's paused clock: a minute of silence
/// passes only when nothing else is left to do.
mod receipt_beside_the_loop {
  use teamcraft_core::api::fake::FakeRendezvous;
  use teamcraft_core::client::LinkReceipt;
  use tokio::task::LocalSet;
  use tokio::time::Instant;

  use super::*;

  #[tokio::test(start_paused = true)]
  async fn the_client_keeps_working_while_the_receipt_is_awaited() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let channel = alice
      .client
      .create_channel(alice.space().await, "general")
      .await
      .unwrap();
    let (laptop_side, receipt) = keys_sent(&mut alice).await;
    let (waiting, _cancel) = receipt.detach();
    LocalSet::new()
      .run_until(async move {
        let started = Instant::now();
        let outcome = tokio::task::spawn_local(waiting);
        tokio::task::yield_now().await;
        // The loop goes on with the client while the new device has not answered yet.
        alice.client.send_text(channel, None, "meanwhile").await.unwrap();
        settle(&mut [&mut alice]).await;
        assert_eq!(alice.texts(channel), ["meanwhile"]);
        assert!(!outcome.is_finished(), "no receipt yet");

        // The new device opens the keys and confirms; the wait ends with success at once.
        let new_store = Arc::new(MemoryStore::new());
        laptop_side.receive_secrets(&new_store, "Laptop").await.unwrap();
        outcome.await.unwrap().unwrap();
        assert_eq!(started.elapsed(), Duration::ZERO, "no timeout was involved");
      })
      .await;
  }

  #[tokio::test(start_paused = true)]
  async fn a_new_device_that_never_confirms_fails_after_a_minute_and_the_loop_goes_on() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let channel = alice
      .client
      .create_channel(alice.space().await, "general")
      .await
      .unwrap();
    // The new device keeps its side of the relay open but never reads the keys (it went to sleep).
    let (silent_laptop, receipt) = keys_sent(&mut alice).await;
    let (waiting, _cancel) = receipt.detach();
    LocalSet::new()
      .run_until(async move {
        let started = Instant::now();
        let outcome = tokio::task::spawn_local(waiting);
        tokio::task::yield_now().await;
        alice.client.send_text(channel, None, "meanwhile").await.unwrap();
        assert!(!outcome.is_finished());

        let result = outcome.await.unwrap();
        assert!(matches!(result, Err(CoreError::LinkNotConfirmed)), "{result:?}");
        assert_eq!(started.elapsed(), Duration::from_secs(60));
        // The client is still usable after the failure.
        alice.client.send_text(channel, None, "after").await.unwrap();
        drop(silent_laptop);
      })
      .await;
  }

  #[tokio::test(start_paused = true)]
  async fn cancelling_ends_the_wait_at_once() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let (laptop_side, receipt) = keys_sent(&mut alice).await;
    let (waiting, cancel) = receipt.detach();
    LocalSet::new()
      .run_until(async move {
        let started = Instant::now();
        let outcome = tokio::task::spawn_local(waiting);
        tokio::task::yield_now().await;

        // The user closed the dialog.
        cancel.cancel();

        let result = outcome.await.unwrap();
        assert!(matches!(result, Err(CoreError::LinkCancelled)), "{result:?}");
        assert_eq!(started.elapsed(), Duration::ZERO, "no minute of waiting");
        drop(laptop_side);
      })
      .await;
  }

  #[tokio::test(start_paused = true)]
  async fn dropping_the_handle_cancels_too() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let (laptop_side, receipt) = keys_sent(&mut alice).await;
    let (waiting, cancel) = receipt.detach();
    // The app stopped: its handle went with it.
    drop(cancel);
    assert!(matches!(waiting.await, Err(CoreError::LinkCancelled)));
    drop(laptop_side);
  }

  /// The handshake of a new laptop of `owner`, and the keys sent after the user confirmed the code.
  async fn keys_sent(owner: &mut Peer) -> (NewDeviceLink<FakeRendezvous>, LinkReceipt<FakeRendezvous>) {
    let api = owner.api.clone();
    let offer = LinkOffer::generate();
    let code = offer.to_code();
    let (laptop_side, approval) = tokio::join!(
      NewDeviceLink::connect(&api, &offer),
      owner.client.start_link_approval(&code)
    );
    let receipt = owner.client.send_link_transfer(approval.unwrap()).await.unwrap();
    (laptop_side.unwrap(), receipt)
  }
}

mod announced {
  use super::*;

  /// The new device waits in the relay and the user asked for the prompt on the other devices.
  async fn connect_and_announce(
    api: &teamcraft_core::api::fake::FakeServerApi,
    offer: &LinkOffer,
    announcement: &LinkAnnouncement,
  ) -> Result<NewDeviceLink<teamcraft_core::api::fake::FakeRendezvous>, CoreError> {
    let (link, announced) = tokio::join!(
      NewDeviceLink::connect(api, offer),
      offer.announce(api, announcement)
    );
    announced.unwrap();
    link
  }

  #[tokio::test]
  async fn a_scanned_code_alone_never_reaches_the_server_as_a_request() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let laptop_api = alice.api.clone();
    let offer = LinkOffer::generate();
    let code = offer.to_code();
    let (laptop_side, phone_side) = tokio::join!(
      NewDeviceLink::connect(&laptop_api, &offer),
      alice.client.start_link_approval(&code)
    );
    assert_eq!(
      laptop_side.unwrap().short_code(),
      phone_side.unwrap().short_code()
    );
    settle(&mut [&mut alice]).await;
    assert!(alice.client.link_requests().is_empty());
    assert!(server.link_requests_held() == 0);
  }

  #[tokio::test]
  async fn a_failed_join_keeps_the_request_listed() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let laptop_api = alice.api.clone();
    let offer = LinkOffer::generate();
    let announcement = tablet();
    let waiting = tokio::time::timeout(
      Duration::from_millis(200),
      connect_and_announce(&laptop_api, &offer, &announcement),
    );
    let (_, id) = tokio::join!(waiting, async {
      settle(&mut [&mut alice]).await;
      server.set_offline(alice.client.user_id(), true);
      let id = alice.client.link_requests()[0].id.clone();
      assert!(alice.client.review_link_request(&id).await.is_err());
      server.set_offline(alice.client.user_id(), false);
      id
    });
    assert_eq!(alice.client.link_requests()[0].id, id, "the user may try again");
  }

  fn tablet() -> LinkAnnouncement {
    LinkAnnouncement {
      device_name: "Tablet".into(),
      platform: DeviceLinkPlatform::Android,
    }
  }

  #[tokio::test]
  async fn a_new_device_shows_up_on_the_existing_one_which_links_it_without_scanning() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let laptop_api = alice.api.clone();
    let offer = LinkOffer::generate();
    let announcement = tablet();

    let (laptop_side, approval) =
      tokio::join!(connect_and_announce(&laptop_api, &offer, &announcement), async {
        settle(&mut [&mut alice]).await;
        let requests = alice.client.link_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].device_name, "Tablet");
        assert_eq!(requests[0].platform, DeviceLinkPlatform::Android);
        assert!(requests[0].seconds_left > 0);
        let approval = alice.client.review_link_request(&requests[0].id).await.unwrap();
        // Taken up: the prompt is gone, and a second review of it is refused.
        assert!(alice.client.link_requests().is_empty());
        assert!(matches!(
          alice.client.review_link_request(&requests[0].id).await,
          Err(CoreError::LinkRequestGone)
        ));
        approval
      });
    let laptop_side = laptop_side.unwrap();
    assert_eq!(laptop_side.short_code(), approval.short_code());

    let new_store = Arc::new(MemoryStore::new());
    let (approved, received) = tokio::join!(
      alice.client.complete_link_approval(approval),
      laptop_side.receive_secrets(&new_store, "Tablet")
    );
    approved.unwrap();
    assert!(received.is_ok());
  }

  #[tokio::test]
  async fn a_denied_request_stops_the_new_device_and_clears_the_prompt() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let laptop_api = alice.api.clone();
    let offer = LinkOffer::generate();
    let announcement = tablet();

    let (laptop_side, rejected) =
      tokio::join!(connect_and_announce(&laptop_api, &offer, &announcement), async {
        settle(&mut [&mut alice]).await;
        let id = alice.client.link_requests()[0].id.clone();
        alice.client.reject_link_request(&id).await
      });
    rejected.unwrap();
    assert!(matches!(laptop_side, Err(CoreError::LinkDenied)));
    assert!(alice.client.link_requests().is_empty());
    // Nothing is left to read after a reconnect either.
    settle(&mut [&mut alice]).await;
    assert!(alice.client.link_requests().is_empty());
  }

  #[tokio::test]
  async fn a_request_the_new_device_gave_up_disappears() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let laptop_api = alice.api.clone();
    let offer = LinkOffer::generate();
    let announcement = tablet();

    let waiting = tokio::time::timeout(
      Duration::from_millis(200),
      connect_and_announce(&laptop_api, &offer, &announcement),
    );
    let (waited, seen) = tokio::join!(waiting, async {
      settle(&mut [&mut alice]).await;
      alice.client.link_requests().len()
    });
    assert!(waited.is_err(), "nobody answered the handshake");
    assert_eq!(seen, 1);
    settle(&mut [&mut alice]).await;
    assert!(alice.client.link_requests().is_empty());
  }

  #[tokio::test]
  async fn only_the_users_own_devices_see_the_request() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut mallory = Peer::enroll(&server, "Mallory").await;
    let laptop_api = alice.api.clone();
    let offer = LinkOffer::generate();
    let announcement = tablet();

    let waiting = tokio::time::timeout(
      Duration::from_millis(200),
      connect_and_announce(&laptop_api, &offer, &announcement),
    );
    let (_, (alices, mallorys)) = tokio::join!(waiting, async {
      settle(&mut [&mut alice, &mut mallory]).await;
      (
        alice.client.link_requests().len(),
        mallory.client.link_requests().len(),
      )
    });
    assert_eq!((alices, mallorys), (1, 0));
    assert!(matches!(
      mallory.client.reject_link_request(&offer_id(&offer)).await,
      Err(CoreError::Api(_))
    ));
  }

  fn offer_id(offer: &LinkOffer) -> String {
    offer.to_code().split(':').nth(1).unwrap().to_string()
  }
}

mod failures_are_visible {
  use teamcraft_core::api::{RendezvousConnection, ServerApi};
  use teamcraft_proto::RendezvousClientMessage;

  use super::*;

  #[tokio::test]
  async fn scanning_a_code_nobody_waits_for_fails_at_once() {
    // Issue #645: the new device's code expired (or it went to sleep) before the phone scanned it.
    let server = FakeServer::new();
    let alice = Peer::enroll(&server, "Alice").await;
    let code = LinkOffer::generate().to_code();

    let result = alice.client.start_link_approval(&code).await;

    assert!(
      matches!(result, Err(CoreError::LinkPeerGone)),
      "{:?}",
      result.err()
    );
  }

  #[tokio::test]
  async fn foreign_and_other_version_codes_say_what_is_wrong() {
    for foreign in ["", "https://example.com/menu", "tclink2:short:key"] {
      assert!(
        matches!(LinkOffer::parse(foreign), Err(CoreError::LinkCodeInvalid)),
        "{foreign}"
      );
    }
    let ours = LinkOffer::generate().to_code();
    let newer = ours.replacen("tclink2:", "tclink9:", 1);
    assert!(matches!(
      LinkOffer::parse(&newer),
      Err(CoreError::LinkVersionMismatch { theirs: 9, .. })
    ));
  }

  #[tokio::test(start_paused = true)]
  async fn a_new_device_that_stops_answering_times_out() {
    let server = FakeServer::new();
    let alice = Peer::enroll(&server, "Alice").await;
    let code = LinkOffer::generate().to_code();
    let rendezvous_id = code.split(':').nth(1).unwrap().to_owned();
    // The new device joined its relay and then went silent (asleep, offline).
    let mut silent = alice.api.connect_rendezvous().await.unwrap();
    silent
      .send_control(&RendezvousClientMessage::Join {
        token: alice.api.gateway_token().unwrap(),
        rendezvous_id,
      })
      .await
      .unwrap();

    let result = alice.client.start_link_approval(&code).await;

    assert!(
      matches!(result, Err(CoreError::LinkTimedOut)),
      "{:?}",
      result.err()
    );
    drop(silent);
  }
}
