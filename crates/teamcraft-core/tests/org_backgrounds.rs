//! Organization call backgrounds: owners list them, with their hashes, in the signed workspace
//! policy; other people's devices only use a picture whose hash matches (e2ee.md §11.2).
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Peer, backend_of};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::CallBackgroundMode;

#[tokio::test]
async fn members_get_the_signed_pictures_and_refuse_swapped_ones() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;

  let picture = b"JPEG of the company lobby".to_vec();
  let lobby = alice
    .client
    .add_org_background("Lobby".into(), picture.clone())
    .await
    .unwrap();
  let garden = alice
    .client
    .add_org_background("Garden".into(), b"JPEG of the garden".to_vec())
    .await
    .unwrap();
  alice
    .client
    .set_org_background_rule(
      CallBackgroundMode::Required,
      Some(garden),
      false,
      vec![garden, lobby],
    )
    .await
    .unwrap();

  bob.client.sync_workspace().await.unwrap();
  let view = backend_of(&bob).org_backgrounds();
  assert_eq!(view.mode, CallBackgroundMode::Required);
  assert!(view.applies_to_me);
  assert_eq!(
    view
      .images
      .iter()
      .map(|image| image.name.as_str())
      .collect::<Vec<_>>(),
    ["Garden", "Lobby"]
  );
  assert_eq!(view.maybe_default, Some(garden));
  assert_eq!(bob.client.org_background_picture(lobby).await.unwrap(), picture);

  // A server that swaps the stored picture is caught by the hash in the signed policy.
  server.swap_org_background(lobby, b"something else entirely".to_vec());
  assert!(bob.client.org_background_picture(lobby).await.is_err());

  // Removing the default clears it, and the picture is gone for everybody.
  alice.client.delete_org_background(garden).await.unwrap();
  bob.client.sync_workspace().await.unwrap();
  let view = backend_of(&bob).org_backgrounds();
  assert_eq!(view.images.len(), 1);
  assert_eq!(view.maybe_default, None);
  assert!(bob.client.org_background_picture(garden).await.is_err());
}

#[tokio::test]
async fn the_order_must_name_every_picture_once() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let lobby = alice
    .client
    .add_org_background("Lobby".into(), b"picture".to_vec())
    .await
    .unwrap();
  assert!(
    alice
      .client
      .set_org_background_rule(CallBackgroundMode::OrgOnly, None, true, vec![])
      .await
      .is_err()
  );
  assert!(
    alice
      .client
      .set_org_background_rule(CallBackgroundMode::OrgOnly, None, true, vec![lobby, lobby])
      .await
      .is_err()
  );
  alice
    .client
    .set_org_background_rule(CallBackgroundMode::OrgOnly, Some(lobby), true, vec![lobby])
    .await
    .unwrap();
  assert_eq!(
    alice.client.org_background_policy().unwrap().mode,
    CallBackgroundMode::OrgOnly
  );
}
