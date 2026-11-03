//! Re-founding the notes after a recovery with no other device (issue #910, e2ee.md §4.6): only
//! when the user's own signed device list names no other device, never on the server's word.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{Peer, link_device, settings, settle};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::{ClientSettings, GroupAccess};
use teamcraft_proto::{ConversationId, ListMlsMessagesQuery};

#[tokio::test]
async fn a_recovered_device_founds_the_notes_anew_once_the_lost_devices_are_revoked() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll_with_settings(&server, "Alice", notes_settings()).await;
  let notes = notes_of(&mut phone).await;
  phone.client.send_text(notes, None, "buy milk").await.unwrap();
  // A second device joins the notes, so the old group is past epoch 0.
  let mut tablet = link_device(&mut phone).await;
  tablet.full_sync().await.unwrap();
  // The phone's group of one has no information on the server yet: it adds the tablet.
  phone.client.full_sync().await.unwrap();
  phone.client.apply_pending_changes().await.unwrap();
  tablet.full_sync().await.unwrap();
  assert_eq!(tablet.group_access(notes).unwrap(), GroupAccess::Member);
  let old_epoch = server_epoch(&phone, notes).await;
  assert!(old_epoch > 0);

  // Both are lost; the user recovers on a laptop. The phone and the tablet are still listed.
  let mut laptop = Peer::enroll_with(phone.api.clone(), "Alice laptop", notes_settings()).await;
  laptop.client.full_sync().await.unwrap();
  assert_eq!(laptop.texts(notes), ["buy milk"]);
  assert_eq!(
    laptop.client.group_access(notes).unwrap(),
    GroupAccess::WaitingForDevice
  );

  laptop
    .client
    .revoke_device(phone.client.device_id())
    .await
    .unwrap();
  laptop.client.revoke_device(tablet.device_id()).await.unwrap();
  laptop.client.full_sync().await.unwrap();
  assert_eq!(laptop.client.group_access(notes).unwrap(), GroupAccess::Member);
  assert_eq!(
    laptop
      .client
      .group_device_ids(notes)
      .unwrap()
      .into_iter()
      .collect::<Vec<_>>(),
    [laptop.client.device_id()]
  );
  assert_eq!(server_epoch(&laptop, notes).await, old_epoch + 1);
  assert_eq!(laptop.client.mls_epoch(notes).unwrap(), Some(old_epoch + 1));

  laptop.client.send_text(notes, None, "eggs").await.unwrap();
  settle(&mut [&mut laptop]).await;
  assert_eq!(laptop.texts(notes), ["buy milk", "eggs"]);
  let new_key = laptop.content_key_of(notes, "eggs").await;
  assert_ne!(
    new_key,
    laptop.content_key_of(notes, "buy milk").await,
    "a fresh key"
  );

  // A device linked to the laptop later joins the new group and reads everything.
  let mut desktop = link_device(&mut laptop).await;
  desktop.full_sync().await.unwrap();
  assert_eq!(desktop.group_access(notes).unwrap(), GroupAccess::Member);
}

#[tokio::test]
async fn notes_whose_other_device_is_still_listed_are_not_founded_anew() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll_with_settings(&server, "Alice", notes_settings()).await;
  let notes = notes_of(&mut phone).await;
  phone.client.send_text(notes, None, "buy milk").await.unwrap();
  let epoch = server_epoch(&phone, notes).await;

  let mut laptop = Peer::enroll_with(phone.api.clone(), "Alice laptop", notes_settings()).await;
  laptop.client.full_sync().await.unwrap();
  assert_eq!(
    laptop.client.group_access(notes).unwrap(),
    GroupAccess::WaitingForDevice
  );
  assert_eq!(server_epoch(&laptop, notes).await, epoch, "nothing committed");

  // The phone is alive: it comes back and adds the laptop to the one group.
  phone.client.full_sync().await.unwrap();
  phone.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut phone, &mut laptop]).await;
  assert_eq!(laptop.client.group_access(notes).unwrap(), GroupAccess::Member);
  assert!(
    laptop
      .client
      .group_device_ids(notes)
      .unwrap()
      .contains(&phone.client.device_id())
  );
  laptop.client.send_text(notes, None, "eggs").await.unwrap();
  settle(&mut [&mut phone, &mut laptop]).await;
  assert_eq!(phone.texts(notes), ["buy milk", "eggs"]);
}

#[tokio::test]
async fn a_server_that_drops_the_other_device_on_its_own_does_not_make_a_device_found_anew() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll_with_settings(&server, "Alice", notes_settings()).await;
  let notes = notes_of(&mut phone).await;
  let mut laptop = Peer::enroll_with(phone.api.clone(), "Alice laptop", notes_settings()).await;
  // The server claims the phone is gone, without the user's signed device list saying so.
  server.revoke_device(phone.client.user_id(), phone.client.device_id());
  laptop.client.full_sync().await.unwrap();
  assert_ne!(laptop.client.group_access(notes).unwrap(), GroupAccess::Member);
  assert_eq!(laptop.client.mls_epoch(notes).unwrap(), None);
}

#[tokio::test]
async fn a_made_up_epoch_beyond_the_limit_founds_nothing() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll_with_settings(&server, "Alice", notes_settings()).await;
  let notes = notes_of(&mut phone).await;
  let limited = ClientSettings {
    max_refound_epoch: 3,
    ..notes_settings()
  };
  let mut laptop = Peer::enroll_with(phone.api.clone(), "Alice laptop", limited).await;
  laptop
    .client
    .revoke_device(phone.client.device_id())
    .await
    .unwrap();
  server.bump_epoch(notes, 4);
  laptop.client.full_sync().await.unwrap();
  assert_eq!(laptop.client.mls_epoch(notes).unwrap(), None);
  assert_eq!(server_epoch(&laptop, notes).await, 4);
}

#[tokio::test]
async fn a_failed_attempt_is_tried_again_only_after_the_rejoin_retry() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll_with_settings(&server, "Alice", notes_settings()).await;
  let notes = notes_of(&mut phone).await;
  let laptop_clock = Arc::new(ManualClock::new(1_800_000_000_000));
  let mut laptop = Peer::enroll_on_clock(
    phone.api.clone(),
    "Alice laptop",
    notes_settings(),
    Box::new(laptop_clock.clone()),
  )
  .await;
  laptop
    .client
    .revoke_device(phone.client.device_id())
    .await
    .unwrap();
  let epoch = server_epoch(&laptop, notes).await;

  // The commit is lost (as it would be refused): nothing is founded, and the next sync right
  // after doesn't redo the work.
  server.drop_next_commit(laptop.client.user_id());
  let _ = laptop.client.full_sync().await;
  assert_eq!(laptop.client.mls_epoch(notes).unwrap(), None);
  laptop.client.full_sync().await.unwrap();
  assert_eq!(laptop.client.mls_epoch(notes).unwrap(), None, "held back");
  assert_eq!(server_epoch(&laptop, notes).await, epoch);

  let retry = notes_settings().rejoin_retry;
  laptop_clock.advance(i64::try_from(retry.as_millis()).unwrap());
  laptop.client.full_sync().await.unwrap();
  assert_eq!(laptop.client.group_access(notes).unwrap(), GroupAccess::Member);
  assert_eq!(server_epoch(&laptop, notes).await, epoch + 1);
}

fn notes_settings() -> ClientSettings {
  settings().with_notes_to_self()
}

/// The user's notes to self, as the sync made them.
async fn notes_of(peer: &mut Peer) -> ConversationId {
  let me = peer.client.user_id();
  peer.client.open_dm(me).await.unwrap()
}

async fn server_epoch(peer: &Peer, conversation: ConversationId) -> u64 {
  let query = ListMlsMessagesQuery {
    maybe_after: None,
    maybe_limit: Some(1),
  };
  peer
    .api
    .list_mls_messages(conversation, &query)
    .await
    .unwrap()
    .current_epoch
}
