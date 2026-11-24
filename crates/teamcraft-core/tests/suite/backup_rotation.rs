//! Passkeys as a recovery factor and replacing the backup key (e2ee.md §6.2.1, §6.6, issue #1965):
//! a passkey recovers the backup and a wrong one doesn't; a rotation retires the old kit, hands the
//! new key to the user's other devices (and only with the identity key's signature), and finishes
//! after an interrupted upload.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{PASSPHRASE, TestClient, clock, options, settings};
use rand_core::OsRng;
use teamcraft_core::api::fake::{FakeServer, FakeServerApi, KeyBagUploadFailure};
use teamcraft_core::store::MemoryStore;
use teamcraft_core::{
  CoreError, EnrollmentOptions, EnrollmentSecret, PendingEnrollment, passkey_credentials,
};
use teamcraft_crypto::backup::{
  BackupError, PasskeyCredential, WrapMethod, WrappedBackupKey, decode_wrapped_key_list,
  encode_wrapped_key_list,
};
use teamcraft_crypto::recovery_key::RecoveryKey;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::Base64Bytes;

/// A change only the laptop made, to find in the bags after the rotation.
const MARKER_MS: i64 = 1_234_567;

mod passkey {
  use super::*;

  #[tokio::test]
  async fn a_passkey_recovers_the_backup_and_a_wrong_one_does_not() {
    let server = FakeServer::new();
    let api = server.add_user("Alice");
    let mut device = enroll(&api, with_key(&RecoveryKey::generate(&mut OsRng)))
      .await
      .unwrap();
    let credential = PasskeyCredential::generate(b"credential".to_vec(), &mut OsRng).unwrap();
    device
      .add_passkey(credential.clone(), &prf_output(1))
      .await
      .unwrap();
    assert_eq!(
      device.passkeys().await.unwrap(),
      std::slice::from_ref(&credential)
    );

    // A device without keys learns which passkey to ask for, and recovers with it.
    assert_eq!(
      passkey_credentials(&api).await.unwrap(),
      std::slice::from_ref(&credential)
    );
    let recovered = enroll(&api, with_passkey(&credential, 1)).await.unwrap();
    assert_eq!(recovered.identity_fingerprint(), device.identity_fingerprint());

    let wrong_output = enroll(&api, with_passkey(&credential, 2)).await;
    assert!(
      matches!(wrong_output, Err(CoreError::Backup(BackupError::UnwrapFailed))),
      "{:?}",
      wrong_output.err()
    );
    let other = PasskeyCredential::generate(b"another passkey".to_vec(), &mut OsRng).unwrap();
    assert!(matches!(
      enroll(&api, with_passkey(&other, 1)).await,
      Err(CoreError::NotEnrolled)
    ));
  }

  #[tokio::test]
  async fn a_removed_passkey_stops_opening_the_backup() {
    let server = FakeServer::new();
    let api = server.add_user("Alice");
    let kit = RecoveryKey::generate(&mut OsRng);
    let mut device = enroll(&api, with_key(&kit)).await.unwrap();
    let credential = PasskeyCredential::generate(b"credential".to_vec(), &mut OsRng).unwrap();
    device
      .add_passkey(credential.clone(), &prf_output(1))
      .await
      .unwrap();

    device.remove_passkey(credential.credential_id()).await.unwrap();
    assert!(passkey_credentials(&api).await.unwrap().is_empty());
    assert!(matches!(
      enroll(&api, with_passkey(&credential, 1)).await,
      Err(CoreError::NotEnrolled)
    ));
    // The kit is untouched.
    enroll(&api, with_key(&kit)).await.unwrap();
  }
}

mod rotation {
  use super::*;

  #[tokio::test]
  async fn retires_the_old_kit_and_keeps_the_other_device_working() {
    let server = FakeServer::new();
    let api = server.add_user("Alice");
    let old_kit = RecoveryKey::generate(&mut OsRng);
    let (mut phone, mut laptop) = two_devices(&api, &old_kit).await;
    let credential = PasskeyCredential::generate(b"credential".to_vec(), &mut OsRng).unwrap();
    phone.add_passkey(credential, &prf_output(1)).await.unwrap();

    let new_kit = RecoveryKey::generate(&mut OsRng);
    phone.rotate_backup_key_with_kit(&new_kit).await.unwrap();

    // The old kit and the passkey (they wrap the old key) are gone; one hand-off, to the laptop.
    assert_eq!(
      methods(&server, &api),
      [WrapMethod::RecoveryKey, WrapMethod::DeviceHandoff]
    );
    assert!(enroll(&api, with_key(&old_kit)).await.is_err());
    // The laptop, which held the old key, switches to the new one on its next sync.
    laptop.sync_account_data().await.unwrap();
    assert_eq!(
      laptop.backup_secret().expose_secret(),
      phone.backup_secret().expose_secret()
    );
    laptop.upload_key_bag().await.unwrap();
    phone.sync_account_data().await.unwrap();
    let recovered = enroll(&api, with_key(&new_kit)).await.unwrap();
    assert_eq!(recovered.identity_fingerprint(), phone.identity_fingerprint());
  }

  #[tokio::test]
  async fn a_hand_off_without_the_identity_signature_is_refused() {
    let server = FakeServer::new();
    let api = server.add_user("Alice");
    let (mut phone, mut laptop) = two_devices(&api, &RecoveryKey::generate(&mut OsRng)).await;
    let old_secret = laptop.backup_secret();
    phone
      .rotate_backup_key_with_kit(&RecoveryKey::generate(&mut OsRng))
      .await
      .unwrap();

    // The server tampers with the signature of the laptop's hand-off.
    let mut stored = server.key_bag_of(api.user_id()).unwrap();
    let wraps: Vec<WrappedBackupKey> = decode_wrapped_key_list(stored.wrapped_backup_keys.as_slice())
      .unwrap()
      .into_iter()
      .map(|wrapped| {
        if wrapped.method() != WrapMethod::DeviceHandoff {
          return wrapped;
        }
        let mut bytes = wrapped.to_bytes().unwrap();
        // version, method, a two-byte length, device ID (8), agreement key (32), then the signature.
        bytes[2 + 2 + 8 + 32 + 5] ^= 1;
        WrappedBackupKey::from_bytes(&bytes).unwrap()
      })
      .collect();
    stored.wrapped_backup_keys = Base64Bytes::new(encode_wrapped_key_list(&wraps).unwrap());
    server.serve_key_bag(api.user_id(), stored);

    assert!(laptop.sync_account_data().await.is_err());
    assert_eq!(
      laptop.backup_secret().expose_secret(),
      old_secret.expose_secret(),
      "the laptop kept its key"
    );
  }

  #[tokio::test]
  async fn an_upload_that_never_arrived_is_made_again() {
    let server = FakeServer::new();
    let api = server.add_user("Alice");
    let old_kit = RecoveryKey::generate(&mut OsRng);
    let mut device = enroll(&api, with_key(&old_kit)).await.unwrap();
    let new_kit = RecoveryKey::generate(&mut OsRng);

    server.fail_next_key_bag_upload(KeyBagUploadFailure::BeforeStoring);
    assert!(device.rotate_backup_key_with_kit(&new_kit).await.is_err());
    // Nothing changed on the server yet: the old kit still opens the backup.
    enroll(&api, with_key(&old_kit)).await.unwrap();

    // The next sync finishes the rotation with the kit the user already saved.
    device.sync_account_data().await.unwrap();
    assert!(enroll(&api, with_key(&old_kit)).await.is_err());
    enroll(&api, with_key(&new_kit)).await.unwrap();
    device.upload_key_bag().await.unwrap();
  }

  #[tokio::test]
  async fn an_upload_whose_answer_was_lost_counts_as_done() {
    let server = FakeServer::new();
    let api = server.add_user("Alice");
    let (mut phone, mut laptop) = two_devices(&api, &RecoveryKey::generate(&mut OsRng)).await;
    let new_kit = RecoveryKey::generate(&mut OsRng);

    server.fail_next_key_bag_upload(KeyBagUploadFailure::ResponseLost);
    assert!(phone.rotate_backup_key_with_kit(&new_kit).await.is_err());

    // The phone finds its own bag on the server and switches to the new key.
    phone.sync_account_data().await.unwrap();
    enroll(&api, with_key(&new_kit)).await.unwrap();
    phone.upload_key_bag().await.unwrap();
    laptop.sync_account_data().await.unwrap();
    assert_eq!(
      laptop.backup_secret().expose_secret(),
      phone.backup_secret().expose_secret()
    );
  }

  #[tokio::test]
  async fn changes_of_another_device_made_meanwhile_are_kept() {
    let server = FakeServer::new();
    let api = server.add_user("Alice");
    let (mut phone, mut laptop) = two_devices(&api, &RecoveryKey::generate(&mut OsRng)).await;
    let before = server.key_bag_of(api.user_id()).unwrap().version;
    // The laptop uploads under the old key while the phone's rotation upload is lost.
    server.fail_next_key_bag_upload(KeyBagUploadFailure::BeforeStoring);
    let new_kit = RecoveryKey::generate(&mut OsRng);
    assert!(phone.rotate_backup_key_with_kit(&new_kit).await.is_err());
    laptop
      .update_account_data(|data, _| {
        data.maybe_recovery_passphrase_set_at = Some(MARKER_MS);
        true
      })
      .unwrap();
    laptop.upload_key_bag().await.unwrap();
    assert!(server.key_bag_of(api.user_id()).unwrap().version > before);

    // The phone merges the laptop's bag (old key) before it uploads under the new one.
    phone.sync_account_data().await.unwrap();
    assert_eq!(
      phone.account_data().maybe_recovery_passphrase_set_at,
      Some(MARKER_MS)
    );
    laptop.sync_account_data().await.unwrap();
    assert_eq!(
      laptop.backup_secret().expose_secret(),
      phone.backup_secret().expose_secret()
    );
    let recovered = enroll(&api, with_key(&new_kit)).await.unwrap();
    assert_eq!(
      recovered.account_data().maybe_recovery_passphrase_set_at,
      Some(MARKER_MS)
    );
  }
}

/// Two devices of one account: the first enrolled with `kit`, the second recovered with it, both
/// in the signed device list.
async fn two_devices(api: &FakeServerApi, kit: &RecoveryKey) -> (TestClient, TestClient) {
  let mut first = enroll(api, with_key(kit)).await.unwrap();
  first.finish_enrollment().await.unwrap();
  let mut second = enroll(api, with_key(kit)).await.unwrap();
  second.finish_enrollment().await.unwrap();
  first.sync_account_data().await.unwrap();
  (first, second)
}

fn methods(server: &FakeServer, api: &FakeServerApi) -> Vec<WrapMethod> {
  decode_wrapped_key_list(
    server
      .key_bag_of(api.user_id())
      .unwrap()
      .wrapped_backup_keys
      .as_slice(),
  )
  .unwrap()
  .iter()
  .map(WrappedBackupKey::method)
  .collect()
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

fn with_passkey(credential: &PasskeyCredential, output: u8) -> EnrollmentOptions {
  EnrollmentOptions {
    secret: EnrollmentSecret::Passkey {
      credential: credential.clone(),
      prf_output: prf_output(output),
    },
    ..options(PASSPHRASE)
  }
}

fn prf_output(byte: u8) -> SecretBytes {
  SecretBytes::new(vec![byte; 32])
}
