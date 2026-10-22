//! Organization recovery end to end (e2ee.md §6.3.1) through the fake server. The hand-over of
//! sealed shares and the user's wrapped backup keys stands in for server endpoints that don't exist
//! yet.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use common::{Peer, clock, options, settings, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::MemoryStore;
use teamcraft_core::workspace::RosterRole;
use teamcraft_core::{OrgRecoveryUnlock, PendingEnrollment};
use teamcraft_crypto::org_recovery::RecoveryRequestKey;

#[tokio::test]
async fn two_admins_recover_a_user_who_lost_everything() {
  let server = FakeServer::new();
  let mut owner = Peer::enroll(&server, "Owner").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let mut dave = Peer::enroll(&server, "Dave").await;
  let (owner_id, bob_id, carol_id) = (
    owner.client.user_id(),
    bob.client.user_id(),
    carol.client.user_id(),
  );
  owner
    .client
    .publish_roster(&[
      (owner_id, RosterRole::Owner),
      (bob_id, RosterRole::Admin),
      (carol_id, RosterRole::Admin),
    ])
    .await
    .unwrap();

  // Setup: 2-of-2 between Bob and Carol; each admin keeps the share sealed to their device.
  let sealed_shares = owner
    .client
    .set_up_org_recovery(&[bob_id, carol_id], 2)
    .await
    .unwrap();
  for sealed in &sealed_shares {
    let admin = if sealed.admin == bob_id { &bob } else { &carol };
    admin.client.store_org_share(&sealed.sealed).unwrap();
  }
  settle(&mut [&mut owner, &mut bob, &mut carol, &mut dave]).await;

  // Dave opts in and has some history.
  dave.client.enable_org_recovery().await.unwrap();
  let channel = owner
    .client
    .create_channel(owner.space().await, "history")
    .await
    .unwrap();
  owner
    .client
    .add_member(channel, dave.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut owner, &mut dave]).await;
  owner
    .client
    .send_text(channel, None, "before Dave lost his phone")
    .await
    .unwrap();
  settle(&mut [&mut owner, &mut dave]).await;
  dave.client.upload_key_bag().await.unwrap();

  // Dave lost his phone and his passphrase. His new device asks for recovery.
  let request = RecoveryRequestKey::generate(&mut rand_core::OsRng);
  let from_bob = bob
    .client
    .share_for_combiner(carol_id, carol.client.device_id())
    .await
    .unwrap();
  let wrapped = server.wrapped_backup_keys_of(dave.client.user_id()).unwrap();
  let sealed_backup_key = carol
    .client
    .recover_backup_key_for(
      dave.client.user_id(),
      &[from_bob],
      &wrapped,
      *request.public_key().as_bytes(),
    )
    .unwrap();

  let mut recovery_options = options("the passphrase Dave forgot");
  recovery_options.maybe_org_recovery = Some(OrgRecoveryUnlock {
    request,
    sealed_backup_key,
  });
  let mut new_device = PendingEnrollment::new()
    .complete(
      dave.api.clone(),
      Arc::new(MemoryStore::new()),
      clock(),
      settings(),
      recovery_options,
    )
    .await
    .unwrap();
  assert_eq!(
    new_device.identity_fingerprint(),
    dave.client.identity_fingerprint()
  );

  // With the recovered key bag the new device reads Dave's history once it is in the group.
  owner.client.add_missing_devices(channel).await.unwrap();
  new_device.sync_welcomes().await.unwrap();
  new_device.load_older_messages(channel).await.unwrap();
  let texts: Vec<String> = teamcraft_core::store::LocalStore::messages(
    new_device.store().as_ref(),
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
  assert_eq!(texts, vec!["before Dave lost his phone"]);
}

#[tokio::test]
async fn one_admin_alone_cannot_recover() {
  let server = FakeServer::new();
  let mut owner = Peer::enroll(&server, "Owner").await;
  let bob = Peer::enroll(&server, "Bob").await;
  let carol = Peer::enroll(&server, "Carol").await;
  let mut dave = Peer::enroll(&server, "Dave").await;
  let (owner_id, bob_id, carol_id) = (
    owner.client.user_id(),
    bob.client.user_id(),
    carol.client.user_id(),
  );
  owner
    .client
    .publish_roster(&[
      (owner_id, RosterRole::Owner),
      (bob_id, RosterRole::Admin),
      (carol_id, RosterRole::Admin),
    ])
    .await
    .unwrap();
  for sealed in owner
    .client
    .set_up_org_recovery(&[bob_id, carol_id], 2)
    .await
    .unwrap()
  {
    let admin = if sealed.admin == bob_id { &bob } else { &carol };
    admin.client.store_org_share(&sealed.sealed).unwrap();
  }
  dave.client.enable_org_recovery().await.unwrap();

  let request = RecoveryRequestKey::generate(&mut rand_core::OsRng);
  let wrapped = server.wrapped_backup_keys_of(dave.client.user_id()).unwrap();
  // Carol tries without Bob's share.
  assert!(
    carol
      .client
      .recover_backup_key_for(
        dave.client.user_id(),
        &[],
        &wrapped,
        *request.public_key().as_bytes()
      )
      .is_err()
  );
  // A share sealed to Bob's device is useless to Carol.
  let bob_to_himself = bob
    .client
    .share_for_combiner(bob_id, bob.client.device_id())
    .await
    .unwrap();
  assert!(
    carol
      .client
      .recover_backup_key_for(
        dave.client.user_id(),
        &[bob_to_himself],
        &wrapped,
        *request.public_key().as_bytes()
      )
      .is_err()
  );
}
