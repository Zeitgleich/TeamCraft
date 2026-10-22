//! Organization recovery from the client's side (e2ee.md §6.3.1): the owner sets it up, members
//! wrap their backup key to the org key, admins keep shares and recover a user together.
//!
//! The server has no endpoints for sealed shares and recovery requests yet (crate README), so the
//! sealed values these methods produce are handed over by the caller.

use rand_core::OsRng;
use teamcraft_crypto::backup::{BackupKeyWrapper, WrapMethod, WrappedBackupKey};
use teamcraft_crypto::identity::{DeviceAgreementPublicKey, SignedDeviceCertificate};
use teamcraft_crypto::org_recovery::{
  ConfirmedRecoveryRequest, OrgRecoveryPublicKey, OrgRecoveryShare, OrgRecoveryWrapper, RecoveryRequest,
  combine_shares, create_org_recovery,
};
use teamcraft_proto::{Base64Bytes, DeviceId, UserId};

use super::Client;
use super::enroll::{WrapsOnMerge, decode_wrapped_keys};
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::trust::PinnedDirectory;

const SHARE_RECORD: &str = "org_recovery_share";

/// The request the server registered as `id` for `user`, as admins' clients and the recovering
/// device both build it (the server's request ID is bound into every seal of the recovery).
pub fn recovery_request(
  id: teamcraft_proto::RecoveryRequestId,
  user: UserId,
  key: teamcraft_crypto::org_recovery::RecoveryRequestPublicKey,
) -> RecoveryRequest {
  use teamcraft_proto::Id as _;
  RecoveryRequest::for_server_id(id.snowflake().get().unsigned_abs(), convert::user(user), key)
}

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
    // Custodians come from the signed roster, so their identity keys do too (security review B6).
    self.sync_workspace().await?;
    let workspace = self.workspace()?;
    if let Some(stranger) = admins.iter().find(|admin| !workspace.is_admin(**admin)) {
      return Err(CoreError::NotAnAdmin(*stranger));
    }
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
    let wrapped = self
      .org_recovery_wrap()?
      .ok_or_else(|| CoreError::Corrupt("the workspace has no org recovery key".into()))?;
    let mut keys: Vec<_> = self
      .current_wrapped_keys()
      .await?
      .into_iter()
      .filter(|existing| existing.method() != WrapMethod::OrgRecovery)
      .collect();
    keys.push(wrapped);
    self.store_wrapped_keys(&keys)?;
    self.upload_key_bag_keeping(WrapsOnMerge::KeepOwn).await
  }

  /// The backup key wrapped to the org key of the verified roster, if the workspace has one.
  pub(crate) fn org_recovery_wrap(&self) -> Result<Option<WrappedBackupKey>, CoreError> {
    let Some(key) = self.workspace()?.roster.maybe_org_recovery_key else {
      return Ok(None);
    };
    let key =
      <[u8; 32]>::try_from(key.as_slice()).map_err(|_| CoreError::Corrupt("bad org recovery key".into()))?;
    let wrapper = OrgRecoveryWrapper::for_wrapping(OrgRecoveryPublicKey::from_bytes(key));
    Ok(Some(wrapper.wrap_key(
      convert::user(self.account.user_id),
      &self.account.backup_key(),
      &mut OsRng,
    )?))
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

  /// Owners, after a recovery: the combining admin knows the org secret now, so a new org key and
  /// new shares replace it; members' clients wrap their backup keys to the new key on their next
  /// sync (security review B4).
  pub async fn rotate_org_recovery(
    &mut self,
    admins: &[UserId],
    threshold: u8,
  ) -> Result<Vec<SealedShare>, CoreError> {
    self.set_up_org_recovery(admins, threshold).await
  }

  /// Members: if the roster's org key changed since this device wrapped the backup key, wraps it to
  /// the new key (old wraps are dropped). A no-op without org recovery.
  pub async fn rewrap_for_current_org_key(&mut self) -> Result<(), CoreError> {
    let Some(current) = self.workspace()?.roster.maybe_org_recovery_key else {
      return Ok(());
    };
    let maybe_org_wrap = self
      .current_wrapped_keys()
      .await?
      .into_iter()
      .find(|wrapped| wrapped.method() == WrapMethod::OrgRecovery);
    match maybe_org_wrap {
      Some(wrap) if wrap.parameters() != current.as_slice() => self.enable_org_recovery().await,
      _ => Ok(()),
    }
  }

  /// Approving admins: seals this admin's share to the combining admin's device, for this request
  /// only. The combiner must be an admin of the signed roster (security review B4).
  pub async fn share_for_combiner(
    &mut self,
    combiner: UserId,
    combiner_device: DeviceId,
    request: &RecoveryRequest,
  ) -> Result<Vec<u8>, CoreError> {
    self.sync_workspace().await?;
    if !self.workspace()?.is_admin(combiner) {
      return Err(CoreError::NotAnAdmin(combiner));
    }
    let share = self.own_share()?;
    let (_, agreement_key) = self
      .verified_agreement_keys(combiner)
      .await?
      .into_iter()
      .find(|(device, _)| *device == combiner_device)
      .ok_or_else(|| CoreError::Corrupt("the combining admin's device is not verified".into()))?;
    share
      .seal_for_request(&agreement_key, request)
      .map_err(|error| CoreError::Corrupt(error.to_string()))
  }

  /// The combining admin: joins this admin's share with the other approvers' shares, opens the
  /// user's org-wrapped backup key and seals it to the recovering device's request, which the admin
  /// confirmed by comparing its verification code with the user (security review B3). The org
  /// secret exists only inside this call; an owner rotates the org key afterwards
  /// ([`Self::rotate_org_recovery`]).
  pub fn recover_backup_key_for(
    &self,
    request: &ConfirmedRecoveryRequest,
    shares_from_approvers: &[Vec<u8>],
    user_wrapped_backup_keys: &[u8],
  ) -> Result<Vec<u8>, CoreError> {
    let mut shares = vec![self.own_share()?];
    for sealed in shares_from_approvers {
      shares.push(
        OrgRecoveryShare::open_for_request(&self.account.device, request.request(), sealed)
          .map_err(|error| CoreError::Corrupt(error.to_string()))?,
      );
    }
    let secret = combine_shares(&shares).map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let unwrapper =
      OrgRecoveryWrapper::for_unwrapping(secret).map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let user = request.request().user_id();
    let wrapped = decode_wrapped_keys(user_wrapped_backup_keys)?
      .into_iter()
      .find(|wrapped| wrapped.method() == WrapMethod::OrgRecovery)
      .ok_or_else(|| CoreError::Corrupt("the user has no org-wrapped backup key".into()))?;
    let backup_key = unwrapper.unwrap_key(user, &wrapped)?;
    request
      .seal_backup_key(&backup_key)
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
    &mut self,
    user: UserId,
  ) -> Result<Vec<(DeviceId, DeviceAgreementPublicKey)>, CoreError> {
    let directory: PinnedDirectory = self.trusted_directory([user]).await?;
    let identity =
      teamcraft_crypto::membership::IdentityDirectory::pinned_identity(&directory, convert::user(user))
        .ok_or(CoreError::UnknownIdentity(user))?;
    let maybe_list = directory.device_list(convert::user(user));
    let mut keys = Vec::new();
    for device in self.api.list_user_devices(user).await?.devices {
      let Ok(certificate) = SignedDeviceCertificate::from_bytes(device.cross_signature.as_slice()) else {
        continue;
      };
      let Ok(verified) = certificate.verify(&identity) else {
        continue;
      };
      // The registered public key starts with the signing key the certificate vouches for.
      // Registration certificates carry no device ID; the signed device list binds the server's
      // ID to the key and leaves out revoked devices (security review A12, B6).
      let is_listed = maybe_list.is_none_or(|list| {
        list.devices().iter().any(|entry| {
          entry.device_id == convert::device(device.id) && entry.signing_key == *verified.signing_key()
        })
      });
      if is_listed
        && verified
          .ensure_signing_key(device.public_key.as_slice().get(..32).unwrap_or_default())
          .is_ok()
      {
        keys.push((device.id, *verified.agreement_key()));
      }
    }
    Ok(keys)
  }
}
