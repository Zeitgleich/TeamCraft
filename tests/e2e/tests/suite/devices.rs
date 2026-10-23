//! Linking a second device through the rendezvous relay: it gets the account's identity and, via a
//! welcome from an existing device, the channels' keys.

use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use teamcraft_core::api::HttpServerApi;
use teamcraft_core::client::NewDeviceLink;
use teamcraft_core::clock::SystemClock;
use teamcraft_core::store::MemoryStore;
use teamcraft_core::{ClientSettings, LinkOffer, PendingEnrollment};
use teamcraft_e2e::peer::enrollment_options;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_linked_device_shares_the_identity_and_joins_the_channels() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let space = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
  let channel = channel_with(&mut alice, space, "general", &mut [&mut carol]).await;

  // Carol links her laptop: the phone scans the laptop's code, both show the same short code, the
  // phone approves, and the laptop receives the account secrets over the end-to-end encrypted relay.
  let laptop_api = HttpServerApi::new(stack.base_url.clone(), carol.tokens.clone());
  let offer = LinkOffer::generate();
  let code = offer.to_code();
  let (laptop_side, phone_side) = tokio::join!(
    NewDeviceLink::connect(&laptop_api, &offer),
    carol.client.start_link_approval(&code)
  );
  let (laptop_side, phone_side) = (laptop_side.unwrap(), phone_side.unwrap());
  assert_eq!(laptop_side.short_code(), phone_side.short_code());
  // The phone reports success only once the laptop confirmed the transfer: both run at once.
  let (approved, received) = tokio::join!(
    carol.client.complete_link_approval(phone_side),
    laptop_side.receive_secrets()
  );
  approved.unwrap();
  let secrets = received.unwrap();

  let options = teamcraft_core::EnrollmentOptions {
    secret: teamcraft_core::EnrollmentSecret::Linked(secrets),
    ..enrollment_options("carol linked laptop")
  };
  let mut laptop = PendingEnrollment::new()
    .complete(
      laptop_api,
      Arc::new(MemoryStore::new()),
      Box::new(SystemClock),
      ClientSettings::default(),
      options,
    )
    .await
    .unwrap();
  assert_eq!(laptop.identity_fingerprint(), carol.client.identity_fingerprint());
  let (mut laptop_gateway, _) = laptop.connect_gateway().await.unwrap();

  let devices = stack.get(&carol.tokens, "/devices").await.expect(StatusCode::OK);
  let active = devices.body["devices"]
    .as_array()
    .unwrap()
    .iter()
    .filter(|device| device["is_revoked"] != true)
    .count();
  assert_eq!(active, 2, "{}", devices.body);

  // An existing device of the account adds the laptop to the channel (the first-ranked device acts
  // after a short delay), then the laptop reads what is sent afterwards.
  let deadline = Instant::now() + Duration::from_secs(60);
  while !laptop.has_group(channel).unwrap() {
    assert!(
      Instant::now() < deadline,
      "the laptop was never added to the channel"
    );
    alice.pump().await;
    carol.pump().await;
    carol.client.apply_pending_changes().await.unwrap();
    laptop.pump(&mut laptop_gateway).await.unwrap();
    laptop.sync_welcomes().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
  alice
    .client
    .send_text(channel, None, "for the new laptop")
    .await
    .unwrap();
  let deadline = Instant::now() + Duration::from_secs(30);
  loop {
    laptop.pump(&mut laptop_gateway).await.unwrap();
    let texts: Vec<String> = teamcraft_core::store::LocalStore::messages(
      laptop.store().as_ref(),
      channel,
      teamcraft_core::store::MessageFilter::TopLevel,
    )
    .unwrap()
    .into_iter()
    .filter_map(|message| match message.content {
      teamcraft_core::store::MessageContent::Text { body, .. } => Some(body),
      _ => None,
    })
    .collect();
    if texts.contains(&"for the new laptop".to_owned()) {
      break;
    }
    assert!(
      Instant::now() < deadline,
      "the laptop never decrypted the message"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
  }

  stack.stop().await;
}
