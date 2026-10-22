//! Organization recovery from the client's side (e2ee.md §6.3.1): the owner sets it up, members
//! wrap their backup key to the org key, admins keep shares and recover a user together.
//!
//! The server has no endpoints for sealed shares and recovery requests yet (crate README), so the
//! sealed values these methods produce are handed over by the caller.

use rand_core::OsRng;
use teamcraft_crypto::backup::{BackupKeyWrapper, WrapMethod};
use teamcraft_crypto::identity::{DeviceAgreementPublicKey, SignedDeviceCertificate};
use teamcraft_crypto::org_recovery::{
  OrgRecoveryPublicKey, OrgRecoveryShare, OrgRecoveryWrapper, RecoveryRequestPublicKey, combine_shares,
  create_org_recovery,
};
use teamcraft_proto::{Base64Bytes, DeviceId, UserId};

use super::Client;
use super::enroll::{WRAPPED_KEYS_KEY, decode_wrapped_keys, encode_wrapped_keys};
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::trust::PinnedDirectory;

const SHARE_RECORD: &str = "org_recovery_share";

/// A share of the org secret sealed to one device of one admin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedShare {
  pub admin: UserId,
  pub device: DeviceId,
  pub sealed: Vec<u8>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Owners: creates the org recovery key, publishes it in the signed roster and returns one share
  /// per admin, sealed to each of the admin's verified devices. `threshold` admins are needed to
  /// recover (the policy's `admins_required`).
  pub async fn set_up_org_recovery(
    &mut self,
    admins: &[UserId],
    threshold: u8,
  ) -> Result<Vec<SealedShare>, CoreError> {
    let count = u8::try_from(admins.len()).map_err(|_| CoreError::Corrupt("too many admins".into()))?;
    let setup = create_org_recovery(threshold, count, &mut OsRng)
      .map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let mut sealed = Vec::new();
    for (admin, share) in admins.iter().zip(&setup.shares) {
      for (device, agreement_key) in self.verified_agreement_keys(*admin).await? {
        let bytes = share
          .seal_for_device(convert::user(*admin), &agreement_key)
          .map_err(|error| CoreError::Corrupt(error.to_string()))?;
        sealed.push(SealedShare {
          admin: *admin,
          device,
          sealed: bytes,
        });
      }
    }
    self.publish_org_recovery_key(setup.public_key.as_bytes()).await?;
    Ok(sealed)
  }

  /// Members: wraps the backup key to the org key from the verified roster and uploads it with the
  /// key bag (replacing a wrap to an older org key).
  pub async fn enable_org_recovery(&mut self) -> Result<(), CoreError> {
    self.sync_workspace().await?;
    let key = self
      .workspace()?
      .roster
      .maybe_org_recovery_key
      .ok_or_else(|| CoreError::Corrupt("the workspace has no org recovery key".into()))?;
    let key =
      <[u8; 32]>::try_from(key.as_slice()).map_err(|_| CoreError::Corrupt("bad org recovery key".into()))?;
    let wrapper = OrgRecoveryWrapper::for_wrapping(OrgRecoveryPublicKey::from_bytes(key));
    let wrapped = wrapper.wrap_key(
      convert::user(self.account.user_id),
      &self.account.backup_key(),
      &mut OsRng,
    )?;
    let current: Base64Bytes = match records::get_json(&self.store, Table::SyncState, WRAPPED_KEYS_KEY)? {
      Some(current) => current,
      None => self.api.key_bag().await?.wrapped_backup_keys,
    };
    let mut keys: Vec<_> = decode_wrapped_keys(current.as_slice())?
      .into_iter()
      .filter(|existing| existing.method() != WrapMethod::OrgRecovery)
      .collect();
    keys.push(wrapped);
    records::put_json(
      &self.store,
      Table::SyncState,
      WRAPPED_KEYS_KEY,
      &Base64Bytes::new(encode_wrapped_keys(&keys)?),
    )?;
    self.upload_key_bag().await
  }

  /// Admins: opens the share sealed to this device and keeps it in the encrypted local store.
  pub fn store_org_share(&self, sealed: &[u8]) -> Result<(), CoreError> {
    let share = OrgRecoveryShare::open_on_device(&self.account.device, sealed)
      .map_err(|error| CoreError::Corrupt(error.to_string()))?;
    records::put_json(
      &self.store,
      Table::Account,
      SHARE_RECORD,
      &Base64Bytes::new(share.expose_secret().to_vec()),
    )
  }

  /// Approving admins: seals this admin's share to the combining admin's device.
  pub async fn share_for_combiner(
    &self,
    combiner: UserId,
    combiner_device: DeviceId,
  ) -> Result<Vec<u8>, CoreError> {
    let share = self.own_share()?;
    let (_, agreement_key) = self
      .verified_agreement_keys(combiner)
      .await?
      .into_iter()
      .find(|(device, _)| *device == combiner_device)
      .ok_or_else(|| CoreError::Corrupt("the combining admin's device is not verified".into()))?;
    share
      .seal_for_device(convert::user(combiner), &agreement_key)
      .map_err(|error| CoreError::Corrupt(error.to_string()))
  }

  /// The combining admin: joins this admin's share with the other approvers' shares, opens `user`'s
  /// org-wrapped backup key and seals it to the recovering device's request key. The org secret
  /// exists only inside this call; rotate the org key afterwards (e2ee.md §6.3.1).
  pub fn recover_backup_key_for(
    &self,
    user: UserId,
    shares_from_approvers: &[Vec<u8>],
    user_wrapped_backup_keys: &[u8],
    request: [u8; 32],
  ) -> Result<Vec<u8>, CoreError> {
    let mut shares = vec![self.own_share()?];
    for sealed in shares_from_approvers {
      shares.push(
        OrgRecoveryShare::open_on_device(&self.account.device, sealed)
          .map_err(|error| CoreError::Corrupt(error.to_string()))?,
      );
    }
    let secret = combine_shares(&shares).map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let unwrapper =
      OrgRecoveryWrapper::for_unwrapping(secret).map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let crypto_user = convert::user(user);
    let wrapped = decode_wrapped_keys(user_wrapped_backup_keys)?
      .into_iter()
      .find(|wrapped| wrapped.method() == WrapMethod::OrgRecovery)
      .ok_or_else(|| CoreError::Corrupt("the user has no org-wrapped backup key".into()))?;
    let backup_key = unwrapper.unwrap_key(crypto_user, &wrapped)?;
    RecoveryRequestPublicKey::from_bytes(request)
      .seal_backup_key(crypto_user, &backup_key)
      .map_err(|error| CoreError::Corrupt(error.to_string()))
  }

  fn own_share(&self) -> Result<OrgRecoveryShare, CoreError> {
    let stored: Base64Bytes = records::get_json(&self.store, Table::Account, SHARE_RECORD)?
      .ok_or_else(|| CoreError::Corrupt("this device holds no org recovery share".into()))?;
    Ok(OrgRecoveryShare::from_bytes(stored.into_vec()))
  }

  /// The X25519 keys of `user`'s devices whose registration certificate verifies against the
  /// trusted identity key (roster or pinned).
  async fn verified_agreement_keys(
    &self,
    user: UserId,
  ) -> Result<Vec<(DeviceId, DeviceAgreementPublicKey)>, CoreError> {
    let directory: PinnedDirectory = self.pin_identities([user]).await?;
    let identity =
      teamcraft_crypto::membership::IdentityDirectory::pinned_identity(&directory, convert::user(user))
        .ok_or(CoreError::UnknownIdentity(user))?;
    let mut keys = Vec::new();
    for device in self.api.list_user_devices(user).await?.devices {
      let Ok(certificate) = SignedDeviceCertificate::from_bytes(device.cross_signature.as_slice()) else {
        continue;
      };
      let Ok(verified) = certificate.verify(&identity) else {
        continue;
      };
      // The registered public key starts with the signing key the certificate vouches for.
      if verified
        .ensure_signing_key(device.public_key.as_slice().get(..32).unwrap_or_default())
        .is_ok()
      {
        keys.push((device.id, *verified.agreement_key()));
      }
    }
    Ok(keys)
  }
}
