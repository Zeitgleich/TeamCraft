//! Replacing the backup key (e2ee.md §6.6, issue #1965).
//!
//! After a recovery kit was seen by others, or a device was lost, replacing the kit alone isn't
//! enough: a server that kept the old wrap list still opens the backup with the old kit. Rotation
//! draws a new backup key, encrypts the key bag with it and uploads it with a new wrap list: the new
//! kit, the org key (if the workspace has one) and one signed hand-off per other active device of
//! the user, so those devices switch to the new key on their next sync without linking again.
//! Passkey and legacy passphrase wraps are dropped (they wrap the old key and can't be re-made
//! without the user); the user adds passkeys again.
//!
//! Interruptions: the new key and its wraps are stored on this device before the upload. A start
//! that finds them finishes the rotation: if the server already has the bag under the new key the
//! device switches to it, otherwise it uploads again. The kit the user saved stays the one that
//! counts either way.

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::backup::{BackupKey, BackupKeyWrapper, RecoveryKeyWrapper, WrappedBackupKey};
use teamcraft_crypto::backup_handoff::{HandoffOpener, HandoffSealer};
use teamcraft_crypto::key_bag::{EncryptedKeyBag, KeyBag, KeyBagBinding, KeyBagError};
use teamcraft_crypto::recovery_key::RecoveryKey;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{Base64Bytes, PutKeyBagRequest};
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::Client;
use super::enroll::{WrapsOnMerge, decode_wrapped_keys, encode_wrapped_keys};
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// The rotation this device started and hasn't seen on the server yet.
const PENDING_ROTATION_KEY: &str = "pending_backup_key_rotation";

/// [`PENDING_ROTATION_KEY`] as stored (in the sealed local store, next to the account's own
/// backup key).
#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct PendingRotation {
  new_backup_secret: [u8; 32],
  /// The encoded wrap list that goes with the new key (public, but kept with it).
  wrapped_keys: Vec<u8>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Switches the account to a fresh backup key protected by `recovery_key`, the key of a new
  /// recovery kit the user saved and checked (e2ee.md §6.6). The old kit, a legacy passphrase and
  /// passkeys stop opening the backup; the user's other active devices receive the new key sealed
  /// to them and keep working.
  pub async fn rotate_backup_key_with_kit(&mut self, recovery_key: &RecoveryKey) -> Result<(), CoreError> {
    // Nothing another device added may be lost: merge the newest bag under the old key first.
    self.sync_account_data().await?;
    self.sync_workspace().await?;
    let new_secret = SymmetricKey::generate(&mut OsRng);
    let wraps = self.rotation_wraps(&new_secret, recovery_key).await?;
    self.update_account_data(|data, now| {
      data.maybe_recovery_key_set_at = Some(now);
      true
    })?;
    let pending = PendingRotation {
      new_backup_secret: *new_secret.expose_secret(),
      wrapped_keys: encode_wrapped_keys(&wraps)?,
    };
    records::put_json(&self.store, Table::SyncState, PENDING_ROTATION_KEY, &pending)?;
    tracing::info!(
      handoffs = wraps.len().saturating_sub(1),
      "rotating the backup key"
    );
    self.finish_rotation(&pending).await
  }

  /// Finishes a rotation an earlier run of this device started and couldn't upload (a crash, the
  /// network). Does nothing without one. Runs before every account data sync.
  pub(crate) async fn resume_backup_key_rotation(&mut self) -> Result<(), CoreError> {
    let Some(pending) =
      records::get_json::<PendingRotation>(&self.store, Table::SyncState, PENDING_ROTATION_KEY)?
    else {
      return Ok(());
    };
    tracing::info!("finishing an interrupted backup key rotation");
    self.finish_rotation(&pending).await
  }

  /// The backup key of a hand-off in `wrapped_keys` that another device of the user sealed to this
  /// one and signed with the identity key, if there is one (e2ee.md §6.6). The caller only adopts
  /// it if the key bag stored next to it opens.
  pub(crate) fn handed_off_backup_key(&self, wrapped_keys: &[u8]) -> Option<BackupKey> {
    let wraps = decode_wrapped_keys(wrapped_keys).ok()?;
    let opener = HandoffOpener::new(&self.account.device, self.account.identity_public_key());
    let user = convert::user(self.account.user_id);
    wraps
      .iter()
      .filter(|wrapped| opener.is_addressed_to_me(wrapped))
      .find_map(|wrapped| match opener.unwrap_key(user, wrapped) {
        Ok(key) => Some(key),
        Err(error) => {
          tracing::warn!(%error, "a backup key hand-off for this device was refused");
          None
        },
      })
  }

  /// Opens a key bag another device encrypted, under this device's backup key or, if another
  /// device rotated it, under the key it handed to this one. Switches to the handed-off key only
  /// once the bag opened with it and carries this account's identity.
  pub(crate) fn open_key_bag(
    &mut self,
    encrypted: &EncryptedKeyBag,
    wrapped_keys: &[u8],
  ) -> Result<(KeyBag, bool), CoreError> {
    let user = convert::user(self.account.user_id);
    match encrypted.decrypt(user, &self.account.backup_key(), wrapped_keys) {
      Ok(bag) => Ok((bag, false)),
      Err(KeyBagError::DecryptionFailed) => {
        let new_key = self
          .handed_off_backup_key(wrapped_keys)
          .ok_or(KeyBagError::DecryptionFailed)?;
        let bag = encrypted.decrypt(user, &new_key, wrapped_keys)?;
        if bag.identity().public_key() != self.account.identity_public_key() {
          return Err(CoreError::IdentityMismatch);
        }
        self.account.backup_secret = new_key.into_key();
        records::save_account(&self.store, &self.account)?;
        tracing::info!("switched to the backup key another device of the user rotated to");
        Ok((bag, true))
      },
      Err(error) => Err(error.into()),
    }
  }

  /// The new wrap list: the kit, the org key, a hand-off to each other active device.
  async fn rotation_wraps(
    &mut self,
    new_secret: &SymmetricKey,
    recovery_key: &RecoveryKey,
  ) -> Result<Vec<WrappedBackupKey>, CoreError> {
    let user = convert::user(self.account.user_id);
    let new_key = BackupKey::from_key(new_secret.duplicate());
    let mut wraps = vec![RecoveryKeyWrapper::new(recovery_key).wrap_key(user, &new_key, &mut OsRng)?];
    if let Some(org) = self.org_recovery_wrap_for(&new_key)? {
      wraps.push(org);
    }
    wraps.extend(self.handoff_wraps(&new_key).await?);
    Ok(wraps)
  }

  /// `new_key` sealed to each other active device of the user whose certificate verifies against
  /// the identity key and which the signed device list names.
  pub(crate) async fn handoff_wraps(
    &mut self,
    new_key: &BackupKey,
  ) -> Result<Vec<WrappedBackupKey>, CoreError> {
    let user = convert::user(self.account.user_id);
    let me = self.account.device_id;
    let mut wraps = Vec::new();
    for (device, agreement_key) in self.verified_agreement_keys(self.account.user_id).await? {
      if device == me {
        continue;
      }
      let sealer = HandoffSealer::new(
        self.account.key_bag.identity(),
        convert::device(device),
        agreement_key,
      );
      wraps.push(sealer.wrap_key(user, new_key, &mut OsRng)?);
    }
    Ok(wraps)
  }

  /// Uploads the bag under the pending key, then switches to it. A newer bag of another device
  /// (under the old key) is merged first; a bag already under the new key means an earlier
  /// attempt's upload landed.
  async fn finish_rotation(&mut self, pending: &PendingRotation) -> Result<(), CoreError> {
    let new_key = BackupKey::from_key(SymmetricKey::from_bytes(pending.new_backup_secret));
    let user = convert::user(self.account.user_id);
    for _ in 0..3 {
      let remote = match self.api.key_bag().await {
        Ok(remote) => Some(remote),
        Err(error) if error.is_not_found() => None,
        Err(error) => return Err(error.into()),
      };
      if let Some(remote) = &remote {
        let encrypted = EncryptedKeyBag::from_bytes(remote.encrypted_key_bag.as_slice())?;
        if encrypted
          .decrypt(user, &new_key, remote.wrapped_backup_keys.as_slice())
          .is_ok()
        {
          return self.commit_rotation(pending, remote.version, encrypted.sequence());
        }
        if remote.version > self.sync.key_bag_version {
          // The pending wraps are uploaded, whatever this stores.
          self.merge_key_bag(
            remote.encrypted_key_bag.as_slice(),
            remote.version,
            remote.wrapped_backup_keys.as_slice(),
            WrapsOnMerge::Adopt,
          )?;
        }
      }
      let sequence = self.sync.key_bag_sequence + 1;
      let encrypted = self.account.key_bag.encrypt(
        &new_key,
        KeyBagBinding {
          sequence,
          wrapped_keys: &pending.wrapped_keys,
        },
        &mut OsRng,
      )?;
      let request = PutKeyBagRequest {
        expected_version: self.sync.key_bag_version,
        encrypted_key_bag: Base64Bytes::new(encrypted.to_bytes()?),
        wrapped_backup_keys: Base64Bytes::new(pending.wrapped_keys.clone()),
      };
      match self.api.put_key_bag(&request).await {
        Ok(stored) => return self.commit_rotation(pending, stored.version, sequence),
        // Another device uploaded meanwhile: merge it and try again.
        Err(error) if error.is_version_conflict() => continue,
        Err(error) => return Err(error.into()),
      }
    }
    Err(CoreError::Corrupt(
      "the key bag kept changing during the backup key rotation".into(),
    ))
  }

  /// The server holds the bag under the new key: this device switches to it.
  fn commit_rotation(
    &mut self,
    pending: &PendingRotation,
    version: u64,
    sequence: u64,
  ) -> Result<(), CoreError> {
    self.account.backup_secret = SymmetricKey::from_bytes(pending.new_backup_secret);
    self.store_wrapped_keys_blob(&pending.wrapped_keys)?;
    records::save_account(&self.store, &self.account)?;
    self.sync.key_bag_version = version;
    self.sync.key_bag_sequence = self.sync.key_bag_sequence.max(sequence);
    self.persist_sync()?;
    self.store.delete(Table::SyncState, PENDING_ROTATION_KEY)?;
    tracing::info!("the backup key rotation is finished");
    Ok(())
  }
}
