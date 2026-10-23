//! The user's signed device list (security review A4, e2ee.md §3): this user's devices publish
//! it, and for everyone else it decides which devices may be added to groups or get secrets.

use serde::{Deserialize, Serialize};
use teamcraft_crypto::device_list::{DeviceList, DeviceListEntry, SignedDeviceList, VerifiedDeviceList};
use teamcraft_crypto::identity::{IdentityPublicKey, SignedDeviceCertificate};
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_proto::{Base64Bytes, DeviceId, DeviceResponse, PutDeviceListRequest, UserId};

use super::Client;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::trust::PinnedDirectory;

/// Key of the user's own device list (as the server lists it) in [`Table::SyncState`].
pub const OWN_DEVICES_KEY: &str = "own_devices";

/// This user's devices as the server last listed them, for the devices settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnDevicesRecord {
  pub devices: Vec<DeviceResponse>,
  /// This device, so the list can mark it (and not offer to sign it out).
  pub current: DeviceId,
  pub fetched_at_ms: i64,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Reads this user's devices from the server for the devices settings.
  pub async fn refresh_own_devices(&mut self) -> Result<(), CoreError> {
    let response = self.api.list_user_devices(self.account.user_id).await?;
    records::put_json(
      &self.store,
      Table::SyncState,
      OWN_DEVICES_KEY,
      &OwnDevicesRecord {
        devices: response.devices,
        current: self.account.device_id,
        fetched_at_ms: self.now_ms(),
      },
    )
  }

  /// Revokes one of this user's other devices (a lost laptop): the server forgets it, the user's
  /// device list drops it for good, and this device removes it from every group it shares with it.
  pub async fn revoke_device(&mut self, device: DeviceId) -> Result<(), CoreError> {
    if device == self.account.device_id {
      return Err(CoreError::Corrupt("a device can't revoke itself".into()));
    }
    self.api.revoke_device(device).await?;
    self.publish_device_list(&[device]).await?;
    let conversations: Vec<_> = self.cached_conversations()?.into_keys().collect();
    for conversation in conversations {
      if self.has_group(conversation)? && self.group_device_ids(conversation)?.contains(&device) {
        self
          .commit_change(
            conversation,
            super::groups::ChangeSource::Own(teamcraft_crypto::membership::MembershipAction::RemoveDevice {
              user_id: convert::user(self.account.user_id),
              device_id: convert::device(device),
            }),
          )
          .await?;
      }
    }
    Ok(())
  }

  /// Publishes this user's device list with this device in it and `revoked` dropped. The first
  /// list starts from the devices the server knows whose certificates the identity key signed.
  pub(crate) async fn publish_device_list(&mut self, revoked: &[DeviceId]) -> Result<(), CoreError> {
    let me = self.account.user_id;
    for _ in 0..2 {
      let maybe_current = self
        .fetch_device_list(me, &self.account.identity_public_key())
        .await?;
      let mut devices = match &maybe_current {
        Some(current) => current.devices().to_vec(),
        None => {
          self
            .certified_devices(me, &self.account.identity_public_key())
            .await?
        },
      };
      let own = DeviceListEntry {
        device_id: convert::device(self.account.device_id),
        signing_key: self.account.device.public_keys().signing_key,
      };
      // The server picks device IDs. One that already names another device (or a revoked one)
      // would make this device sign that device out of the list: refuse it.
      let is_taken = devices
        .iter()
        .any(|entry| entry.device_id == own.device_id && entry.signing_key != own.signing_key)
        || maybe_current
          .as_ref()
          .is_some_and(|current| current.is_revoked(own.device_id));
      if is_taken {
        return Err(CoreError::DeviceIdTaken(self.account.device_id));
      }
      devices.retain(|entry| entry.device_id != own.device_id);
      devices.push(own);
      let revoked: Vec<_> = revoked.iter().copied().map(convert::device).collect();
      let signed = DeviceList::next(convert::user(me), maybe_current.as_ref(), devices, &revoked)
        .sign(self.account.key_bag.identity())?;
      let request = PutDeviceListRequest {
        expected_sequence: maybe_current.as_ref().map_or(0, VerifiedDeviceList::sequence),
        sequence: signed.unverified_sequence(),
        signed_list: Base64Bytes::new(signed.to_bytes()?),
      };
      match self.api.put_device_list(&request).await {
        Ok(_) => return self.store_device_list(me, &signed),
        Err(error) if error.is_version_conflict() => continue,
        // The server doesn't keep device lists yet (crate README, server gaps).
        Err(error) if error.is_not_found() => {
          tracing::warn!("the server has no device list endpoint; the list wasn't published");
          return Ok(());
        },
        Err(error) => return Err(error.into()),
      }
    }
    Err(CoreError::GaveUp {
      attempts: 2,
      reason: "another device kept updating the device list".into(),
    })
  }

  /// Fetches the newest device lists of `users` (checked against their pinned identity keys and
  /// never older than the stored ones) and returns the directory with them.
  pub(crate) async fn trusted_directory(
    &mut self,
    users: impl IntoIterator<Item = UserId>,
  ) -> Result<PinnedDirectory, CoreError> {
    let users: Vec<UserId> = users.into_iter().collect();
    let directory = self.pin_identities(users.iter().copied()).await?;
    for user in &users {
      let Some(identity) = directory.pinned_identity(convert::user(*user)) else {
        continue;
      };
      if let Err(error) = self.fetch_device_list(*user, &identity).await {
        if error.is_transient() {
          return Err(error);
        }
        tracing::warn!(user_id = %user, %error, "refused a device list");
      }
    }
    self.pin_identities(users).await
  }

  /// The device lists stored for the directory's users, verified again against its keys.
  pub(crate) fn stored_device_lists(
    &self,
    directory: &PinnedDirectory,
    users: &[UserId],
  ) -> Vec<VerifiedDeviceList> {
    users
      .iter()
      .filter_map(|user| {
        let identity = directory.pinned_identity(convert::user(*user))?;
        self.stored_device_list(*user, &identity).ok().flatten()
      })
      .collect()
  }

  /// The user's list from the server if it is at least as new as the stored one (then stored),
  /// else the stored one. `None` if the user never published one.
  async fn fetch_device_list(
    &self,
    user: UserId,
    identity: &IdentityPublicKey,
  ) -> Result<Option<VerifiedDeviceList>, CoreError> {
    let maybe_stored = self.stored_device_list(user, identity)?;
    let response = match self.api.device_list(user).await {
      Ok(response) => response,
      Err(error) if error.is_not_found() => return Ok(maybe_stored),
      Err(error) => return Err(error.into()),
    };
    let signed = SignedDeviceList::from_bytes(response.signed_list.as_slice())?;
    let verified = signed.verify_newer_than(convert::user(user), identity, maybe_stored.as_ref())?;
    self.store_device_list(user, &signed)?;
    Ok(Some(verified))
  }

  fn stored_device_list(
    &self,
    user: UserId,
    identity: &IdentityPublicKey,
  ) -> Result<Option<VerifiedDeviceList>, CoreError> {
    let Some(bytes) = records::get_json::<Base64Bytes>(&self.store, Table::DeviceLists, &user.to_string())?
    else {
      return Ok(None);
    };
    Ok(Some(
      SignedDeviceList::from_bytes(bytes.as_slice())?.verify(convert::user(user), identity)?,
    ))
  }

  fn store_device_list(&self, user: UserId, signed: &SignedDeviceList) -> Result<(), CoreError> {
    records::put_json(
      &self.store,
      Table::DeviceLists,
      &user.to_string(),
      &Base64Bytes::new(signed.to_bytes()?),
    )
  }

  /// The user's devices whose registration certificate the identity key signed.
  async fn certified_devices(
    &self,
    user: UserId,
    identity: &IdentityPublicKey,
  ) -> Result<Vec<DeviceListEntry>, CoreError> {
    let mut entries = Vec::new();
    for device in self.api.list_user_devices(user).await?.devices {
      let Ok(certificate) = SignedDeviceCertificate::from_bytes(device.cross_signature.as_slice()) else {
        continue;
      };
      let Ok(verified) = certificate.verify(identity) else {
        continue;
      };
      entries.push(DeviceListEntry {
        device_id: convert::device(device.id),
        signing_key: *verified.signing_key(),
      });
    }
    Ok(entries)
  }
}
