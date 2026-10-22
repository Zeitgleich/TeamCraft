//! The user's own call backgrounds: encrypted on the server, their keys in the key bag, so they
//! follow the user to their other devices.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, link_device};
use teamcraft_core::api::fake::FakeServer;

#[tokio::test]
async fn a_background_reaches_the_users_other_device_and_is_deleted_everywhere() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;
  let bob = Peer::enroll(&server, "Bob").await;

  let picture = b"JPEG of the user's own background".to_vec();
  let id = phone
    .client
    .add_background("Garden".into(), picture.clone())
    .await
    .unwrap();
  assert_eq!(phone.client.backgrounds()[0].name, "Garden");
  phone.client.sync_account_data().await.unwrap();

  laptop.sync_account_data().await.unwrap();
  let listed = laptop.backgrounds();
  assert_eq!(listed.len(), 1);
  assert_eq!((listed[0].id, listed[0].size_bytes), (id, picture.len() as u64));
  let fetched = laptop.background_picture(id).await.unwrap();
  assert_eq!(fetched.expose_secret(), picture.as_slice());

  // Bob's devices know nothing about it, and the server won't hand him the object.
  assert!(bob.client.backgrounds().is_empty());
  assert!(bob.client.background_picture(id).await.is_err());

  laptop.delete_background(id).await.unwrap();
  laptop.sync_account_data().await.unwrap();
  phone.client.sync_account_data().await.unwrap();
  assert!(phone.client.backgrounds().is_empty());
  assert!(phone.client.background_picture(id).await.is_err());
  // Deleting again (the other device's turn) is quiet.
  phone.client.delete_background(id).await.unwrap();
}

#[tokio::test]
async fn the_other_device_takes_a_new_background_when_the_key_bag_changes() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;
  let (mut laptop_gateway, _) = laptop.connect_gateway().await.unwrap();

  let id = phone
    .client
    .add_background("Desk".into(), b"picture".to_vec())
    .await
    .unwrap();
  phone.client.sync_account_data().await.unwrap();

  // No explicit sync: the gateway's key bag event makes the laptop fetch it.
  laptop.pump(&mut laptop_gateway).await.unwrap();
  assert_eq!(
    laptop.backgrounds().first().map(|background| background.id),
    Some(id)
  );
}
