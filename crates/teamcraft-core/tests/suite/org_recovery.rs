//! Organization recovery end to end (e2ee.md §6.3.1) through the fake server. The hand-over of
//! sealed shares and the user's wrapped backup keys stands in for server endpoints that don't exist
//! yet.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{PASSPHRASE, Peer, clock, options, settings, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::MemoryStore;
use teamcraft_core::workspace::RosterRole;
use teamcraft_core::{EnrollmentOptions, EnrollmentSecret, OrgRecoveryUnlock, PendingEnrollment};
use teamcraft_crypto::org_recovery::{RecoveryRequest, RecoveryRequestKey};
use teamcraft_crypto::secret::SecretBytes;

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
  // Changing the passphrase keeps the org wrap.
  dave
    .client
    .upload_key_bag_with(
      &SecretBytes::new(b"a changed passphrase".to_vec()),
      options(PASSPHRASE).argon2,
    )
    .await
    .unwrap();

  // Dave lost his phone and his passphrase. His new device asks for recovery and shows a code,
  // which Dave reads to Carol on the phone; she enters it (security review B3).
  let key = RecoveryRequestKey::generate(&mut rand_core::OsRng);
  let request = RecoveryRequest::new(crypto_user(dave.client.user_id()), &key, &mut rand_core::OsRng);
  let code_read_out = request.verification_code();
  let from_bob = bob
    .client
    .share_for_combiner(carol_id, carol.client.device_id(), &request)
    .await
    .unwrap();
  let wrapped = server.wrapped_backup_keys_of(dave.client.user_id()).unwrap();
  let confirmed = request.clone().confirm(&code_read_out).unwrap();
  let sealed_backup_key = carol
    .client
    .recover_backup_key_for(&confirmed, &[from_bob], &wrapped)
    .unwrap();

  let recovery_options = EnrollmentOptions {
    secret: EnrollmentSecret::OrgRecovery {
      unlock: OrgRecoveryUnlock {
        key,
        request,
        sealed_backup_key,
      },
      new_passphrase: SecretBytes::new(b"Dave's new passphrase".to_vec()),
    },
    ..options(PASSPHRASE)
  };
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

  // The admins saw the old backup key, so the account moved to a new one (security review B5):
  // the old passphrase no longer opens the key bag, the new one does.
  assert_ne!(
    server.wrapped_backup_keys_of(dave.client.user_id()).unwrap(),
    wrapped
  );
  let with_old = PendingEnrollment::new()
    .complete(
      dave.api.clone(),
      Arc::new(MemoryStore::new()),
      clock(),
      settings(),
      options(PASSPHRASE),
    )
    .await;
  assert!(with_old.is_err());
  let with_new = PendingEnrollment::new()
    .complete(
      dave.api.clone(),
      Arc::new(MemoryStore::new()),
      clock(),
      settings(),
      options("Dave's new passphrase"),
    )
    .await
    .unwrap();
  assert_eq!(
    with_new.identity_fingerprint(),
    dave.client.identity_fingerprint()
  );
}

#[tokio::test]
async fn one_admin_alone_cannot_recover() {
  let server = FakeServer::new();
  let mut owner = Peer::enroll(&server, "Owner").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
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

  let key = RecoveryRequestKey::generate(&mut rand_core::OsRng);
  let request = RecoveryRequest::new(crypto_user(dave.client.user_id()), &key, &mut rand_core::OsRng);
  let confirmed = request.clone().confirm(&request.verification_code()).unwrap();
  let wrapped = server.wrapped_backup_keys_of(dave.client.user_id()).unwrap();
  // Carol tries without Bob's share.
  assert!(
    carol
      .client
      .recover_backup_key_for(&confirmed, &[], &wrapped)
      .is_err()
  );
  // A share sealed to Bob's device is useless to Carol.
  let bob_to_himself = bob
    .client
    .share_for_combiner(bob_id, bob.client.device_id(), &request)
    .await
    .unwrap();
  assert!(
    carol
      .client
      .recover_backup_key_for(&confirmed, &[bob_to_himself], &wrapped)
      .is_err()
  );
  // Shares go only to roster admins, and custodians must be roster admins (B4, B6).
  assert!(matches!(
    bob
      .client
      .share_for_combiner(dave.client.user_id(), dave.client.device_id(), &request)
      .await,
    Err(teamcraft_core::CoreError::NotAnAdmin(_))
  ));
  assert!(matches!(
    owner
      .client
      .set_up_org_recovery(&[bob_id, dave.client.user_id()], 2)
      .await,
    Err(teamcraft_core::CoreError::NotAnAdmin(_))
  ));

  // After a recovery the owner rotates the org key; members wrap to the new key on their next sync
  // and the old wrap is gone (B4).
  let before = server.wrapped_backup_keys_of(dave.client.user_id()).unwrap();
  owner
    .client
    .rotate_org_recovery(&[bob_id, carol_id], 2)
    .await
    .unwrap();
  dave.client.sync_workspace().await.unwrap();
  dave.client.rewrap_for_current_org_key().await.unwrap();
  let after = server.wrapped_backup_keys_of(dave.client.user_id()).unwrap();
  assert_ne!(before, after);
  dave.client.rewrap_for_current_org_key().await.unwrap();
  assert_eq!(
    server.wrapped_backup_keys_of(dave.client.user_id()).unwrap(),
    after,
    "nothing to do once wrapped to the current key"
  );
}

fn crypto_user(user: teamcraft_proto::UserId) -> teamcraft_crypto::ids::UserId {
  use teamcraft_proto::Id as _;
  teamcraft_crypto::ids::UserId::new(user.snowflake().get().unsigned_abs())
}
