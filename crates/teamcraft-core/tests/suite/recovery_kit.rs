//! The recovery kit (e2ee.md §6.5): a new account protected by a generated recovery key, recovery
//! with the key (also typed into a passphrase field), replacing a legacy passphrase without
//! disturbing the user's other devices, the desktop's kept backup key, and the workspace policy
//! that switches passphrases off.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]
// The backup state the settings read is part of the UI adapter.
#![cfg(feature = "ui")]

use crate::common;

use std::sync::Arc;

use common::{PASSPHRASE, Peer, TestClient, clock, options, settings};
use rand_core::OsRng;
use teamcraft_client_api::security::RecoveryMethod;
use teamcraft_core::api::fake::{FakeServer, FakeServerApi};
use teamcraft_core::store::MemoryStore;
use teamcraft_core::ui_snapshot::key_backup_state;
use teamcraft_core::workspace::RosterRole;
use teamcraft_core::{CoreError, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use teamcraft_crypto::backup::{BackupError, WrapMethod, decode_wrapped_key_list};
use teamcraft_crypto::recovery_key::RecoveryKey;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::{RecoveryPolicy, WorkspacePolicy};

#[tokio::test]
async fn a_new_account_is_protected_by_the_kit_and_recovers_with_it() {
  let server = FakeServer::new();
  let api = server.add_user("Alice");
  let kit = RecoveryKey::generate(&mut OsRng);
  let first = enroll(&api, with_key(&kit)).await.unwrap();

  let stored = server.key_bag_of(api.user_id()).unwrap();
  let methods: Vec<WrapMethod> = decode_wrapped_key_list(stored.wrapped_backup_keys.as_slice())
    .unwrap()
    .iter()
    .map(|wrap| wrap.method())
    .collect();
  assert_eq!(methods, [WrapMethod::RecoveryKey], "no passphrase anywhere");
  let state = key_backup_state(first.store()).unwrap().unwrap();
  assert_eq!(state.recovery, [RecoveryMethod::RecoveryKey]);
  assert!(state.maybe_recovery_key_set_at.is_some());
  assert_eq!(state.maybe_passphrase_set_at, None);

  // All devices lost: the key as the user types it from the printed kit.
  let typed = RecoveryKey::parse(&kit.to_display().to_lowercase().replace('-', " ")).unwrap();
  let recovered = enroll(&api, with_key(&typed)).await.unwrap();
  assert_eq!(recovered.identity_fingerprint(), first.identity_fingerprint());
}

#[tokio::test]
async fn another_key_or_a_passphrase_does_not_open_a_kit_protected_backup() {
  let server = FakeServer::new();
  let api = server.add_user("Alice");
  enroll(&api, with_key(&RecoveryKey::generate(&mut OsRng)))
    .await
    .unwrap();

  let wrong_key = enroll(&api, with_key(&RecoveryKey::generate(&mut OsRng))).await;
  assert!(
    matches!(wrong_key, Err(CoreError::Backup(BackupError::UnwrapFailed))),
    "{:?}",
    wrong_key.err()
  );
  // No passphrase wrap exists to try.
  let passphrase = enroll(&api, options(PASSPHRASE)).await;
  assert!(
    matches!(passphrase, Err(CoreError::NotEnrolled)),
    "{:?}",
    passphrase.err()
  );
}

#[tokio::test]
async fn the_key_typed_into_a_passphrase_field_recovers_too() {
  let server = FakeServer::new();
  let api = server.add_user("Alice");
  let kit = RecoveryKey::generate(&mut OsRng);
  let first = enroll(&api, with_key(&kit)).await.unwrap();
  // An app with only a passphrase field (the phone apps until they get the kit).
  let typed = options(&kit.to_display());
  let recovered = enroll(&api, typed).await.unwrap();
  assert_eq!(recovered.identity_fingerprint(), first.identity_fingerprint());
}

#[tokio::test]
async fn replacing_the_passphrase_keeps_other_devices_working() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let api = phone.api.clone();
  let mut laptop = enroll(&api, options(PASSPHRASE)).await.unwrap();

  let kit = RecoveryKey::generate(&mut OsRng);
  phone.client.set_recovery_key(&kit).await.unwrap();
  let state = key_backup_state(phone.client.store()).unwrap().unwrap();
  assert_eq!(state.recovery, [RecoveryMethod::RecoveryKey]);
  assert!(!state.has_pending_upload);

  // The laptop adopts the new wraps with the next bag and keeps uploading its own changes.
  laptop.sync_account_data().await.unwrap();
  let on_laptop = key_backup_state(laptop.store()).unwrap().unwrap();
  assert_eq!(on_laptop.recovery, [RecoveryMethod::RecoveryKey]);
  assert_eq!(
    on_laptop.maybe_recovery_key_set_at,
    state.maybe_recovery_key_set_at
  );
  laptop.upload_key_bag().await.unwrap();

  // The passphrase stopped opening the backup, the kit opens it.
  assert!(enroll(&api, options(PASSPHRASE)).await.is_err());
  let recovered = enroll(&api, with_key(&kit)).await.unwrap();
  assert_eq!(
    recovered.identity_fingerprint(),
    phone.client.identity_fingerprint()
  );
}

#[tokio::test]
async fn a_new_kit_replaces_the_old_one() {
  let server = FakeServer::new();
  let api = server.add_user("Alice");
  let old_kit = RecoveryKey::generate(&mut OsRng);
  let mut device = enroll(&api, with_key(&old_kit)).await.unwrap();
  let new_kit = RecoveryKey::generate(&mut OsRng);
  device.set_recovery_key(&new_kit).await.unwrap();

  let wraps = decode_wrapped_key_list(
    server
      .key_bag_of(api.user_id())
      .unwrap()
      .wrapped_backup_keys
      .as_slice(),
  )
  .unwrap();
  assert_eq!(wraps.len(), 1, "one kit at a time");
  assert!(enroll(&api, with_key(&old_kit)).await.is_err());
  enroll(&api, with_key(&new_kit)).await.unwrap();
}

#[tokio::test]
async fn the_kept_backup_key_restores_the_account_without_the_kit() {
  let server = FakeServer::new();
  let api = server.add_user("Alice");
  let first = enroll(&api, with_key(&RecoveryKey::generate(&mut OsRng)))
    .await
    .unwrap();
  // What the desktop app keeps in the OS keychain.
  let kept = first.backup_secret();
  let restored = enroll(&api, kept_key(kept)).await.unwrap();
  assert_eq!(restored.identity_fingerprint(), first.identity_fingerprint());

  let stale = enroll(&api, kept_key(SymmetricKey::generate(&mut OsRng))).await;
  assert!(stale.is_err(), "a key that doesn't open the bag restores nothing");
}

#[tokio::test]
async fn only_a_kit_or_a_passphrase_creates_an_identity() {
  let server = FakeServer::new();
  let api = server.add_user("Alice");
  let result = enroll(&api, kept_key(SymmetricKey::generate(&mut OsRng))).await;
  assert!(
    matches!(result, Err(CoreError::IdentityMissing)),
    "{:?}",
    result.err()
  );
}

#[tokio::test]
async fn the_policy_can_switch_passphrases_off() {
  let server = FakeServer::new();
  let mut owner = Peer::enroll(&server, "Owner").await;
  let owner_id = owner.client.user_id();
  owner
    .client
    .publish_roster(&[(owner_id, RosterRole::Owner)])
    .await
    .unwrap();
  assert!(
    key_backup_state(owner.client.store())
      .unwrap()
      .unwrap()
      .is_passphrase_allowed
  );

  owner
    .client
    .publish_policy(WorkspacePolicy {
      recovery: RecoveryPolicy {
        is_passphrase_allowed: false,
      },
      ..WorkspacePolicy::default()
    })
    .await
    .unwrap();
  owner.client.sync_workspace().await.unwrap();
  let state = key_backup_state(owner.client.store()).unwrap().unwrap();
  assert!(!state.is_passphrase_allowed);
  let change = owner
    .client
    .upload_key_bag_with(
      &SecretBytes::new(b"a brand new passphrase".to_vec()),
      options(PASSPHRASE).argon2,
    )
    .await;
  assert!(
    matches!(change, Err(CoreError::PassphraseNotAllowed)),
    "{change:?}"
  );
  // The existing passphrase still opens the backup; the kit replaces it.
  owner
    .client
    .set_recovery_key(&RecoveryKey::generate(&mut OsRng))
    .await
    .unwrap();
}

async fn enroll(api: &FakeServerApi, options: EnrollmentOptions) -> Result<TestClient, CoreError> {
  PendingEnrollment::new()
    .complete(
      api.clone(),
      Arc::new(MemoryStore::new()),
      clock(),
      settings(),
      options,
    )
    .await
}

fn with_key(recovery_key: &RecoveryKey) -> EnrollmentOptions {
  EnrollmentOptions {
    secret: EnrollmentSecret::RecoveryKey(recovery_key.duplicate()),
    ..options(PASSPHRASE)
  }
}

fn kept_key(backup_secret: SymmetricKey) -> EnrollmentOptions {
  EnrollmentOptions {
    secret: EnrollmentSecret::KeptBackupKey(backup_secret),
    ..options(PASSPHRASE)
  }
}
