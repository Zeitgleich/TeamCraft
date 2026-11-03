//! The user's signed device list (security review A4, e2ee.md §3): this user's devices publish
//! it, and for everyone else it decides which devices may be added to groups or get secrets.

use serde::{Deserialize, Serialize};
use teamcraft_crypto::device_list::{DeviceList, DeviceListEntry, SignedDeviceList, VerifiedDeviceList};
use teamcraft_crypto::identity::{IdentityPublicKey, SignedDeviceCertificate};
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_proto::{
  Base64Bytes, DeviceId, DevicePushHealth, DeviceResponse, PutDeviceListRequest, TestPushOutcome,
  UpdateDeviceRequest, UserId,
};

use super::Client;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::trust::PinnedDirectory;

/// Key of the user's own device list (as the server lists it) in [`Table::SyncState`].
pub const OWN_DEVICES_KEY: &str = "own_devices";

/// What the user's identity key says about one of their devices (issue #409, e2ee.md §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnDeviceCheck {
  /// Its certificate is the user's and the signed device list names it with that key.
  Verified,
  /// Its certificate is the user's, but the signed device list doesn't name it (yet).
  Pending,
  /// Nothing the user's identity key signed vouches for it.
  Unverified,
}

/// This user's devices as the server last listed them, for the devices settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnDevicesRecord {
  pub devices: Vec<DeviceResponse>,
  /// This device, so the list can mark it (and not offer to sign it out).
  pub current: DeviceId,
  pub fetched_at_ms: i64,
  /// What this device verified of each listed device (issue #409); empty in records of older
  /// versions, which then show no verification state.
  #[serde(default)]
  pub verification: Vec<(DeviceId, OwnDeviceCheck)>,
  /// Push service and delivery counts of each device; `None` when the server doesn't tell (an
  /// older server).
  #[serde(rename = "push", default)]
  pub maybe_push: Option<Vec<DevicePushHealth>>,
  /// How the push service took the last test push to each device, kept across refreshes.
  #[serde(default)]
  pub tests: Vec<OwnDeviceTest>,
}

/// A test push the user sent to one of their devices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnDeviceTest {
  pub device: DeviceId,
  pub outcome: TestPushOutcome,
  /// When it was sent, by this device's clock.
  pub sent_at_ms: i64,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Reads this user's devices from the server for the devices settings, checks each against the
  /// user's signed device list (e2ee.md §3) and asks for their push state.
  pub async fn refresh_own_devices(&mut self) -> Result<(), CoreError> {
    let me = self.account.user_id;
    let response = self.api.list_user_devices(me).await?;
    let identity = self.account.identity_public_key();
    let maybe_list = match self.fetch_device_list(me, &identity).await {
      Ok(maybe_list) => maybe_list,
      Err(error) if error.is_transient() => return Err(error),
      Err(error) => {
        tracing::warn!(%error, "refused this user's device list; checking against the stored one");
        self.stored_device_list(me, &identity)?
      },
    };
    let verification = response
      .devices
      .iter()
      .map(|device| {
        let state = own_device_verification(device, me, &identity, maybe_list.as_ref());
        (device.id, state)
      })
      .collect();
    let maybe_push = match self.api.my_push_health().await {
      Ok(health) => Some(health.devices),
      // A server before issue #409.
      Err(error) if error.is_not_found() => None,
      Err(error) => return Err(error.into()),
    };
    let tests = self
      .own_devices_record()?
      .map(|previous| previous.tests)
      .unwrap_or_default()
      .into_iter()
      .filter(|test| response.devices.iter().any(|device| device.id == test.device))
      .collect();
    self.store_own_devices(&OwnDevicesRecord {
      devices: response.devices,
      current: self.account.device_id,
      fetched_at_ms: self.now_ms(),
      verification,
      maybe_push,
      tests,
    })
  }

  /// Renames one of this user's devices, then reads the list again.
  pub async fn rename_own_device(&mut self, device: DeviceId, name: &str) -> Result<(), CoreError> {
    self
      .api
      .update_device(
        device,
        &UpdateDeviceRequest {
          name: name.trim().to_owned(),
        },
      )
      .await?;
    self.refresh_own_devices().await
  }

  /// Asks the server for a test push to one of this user's devices and keeps how the push
  /// service took it for the device list; then reads the list again (the last push time moved).
  pub async fn test_push_own_device(&mut self, device: DeviceId) -> Result<(), CoreError> {
    let response = self.api.send_test_push(device).await?;
    if let Some(mut record) = self.own_devices_record()? {
      record.tests.retain(|test| test.device != device);
      record.tests.push(OwnDeviceTest {
        device,
        outcome: response.outcome,
        sent_at_ms: self.now_ms(),
      });
      self.store_own_devices(&record)?;
    }
    self.refresh_own_devices().await
  }

  fn own_devices_record(&self) -> Result<Option<OwnDevicesRecord>, CoreError> {
    records::get_json(&self.store, Table::SyncState, OWN_DEVICES_KEY)
  }

  fn store_own_devices(&self, record: &OwnDevicesRecord) -> Result<(), CoreError> {
    records::put_json(&self.store, Table::SyncState, OWN_DEVICES_KEY, record)
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
  pub(crate) async fn fetch_device_list(
    &mut self,
    user: UserId,
    identity: &IdentityPublicKey,
  ) -> Result<Option<VerifiedDeviceList>, CoreError> {
    let maybe_stored = self.stored_device_list(user, identity)?;
    let read = match self
      .read_ahead
      .own_device_list
      .take()
      .filter(|_| user == self.user_id())
    {
      Some(read) => read,
      None => self.api.device_list(user).await,
    };
    let Some(response) = read? else {
      return Ok(maybe_stored);
    };
    let signed = SignedDeviceList::from_bytes(response.signed_list.as_slice())?;
    let verified = signed.verify_newer_than(convert::user(user), identity, maybe_stored.as_ref())?;
    self.store_device_list(user, &signed)?;
    Ok(Some(verified))
  }

  pub(crate) fn stored_device_list(
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

/// What the user's identity key says about one of their devices the server lists: verified when
/// its certificate is theirs and the signed device list names it with that certificate's signing
/// key, pending when only the certificate is (still being set up), unverified otherwise. Never the
/// server's word.
///
/// The server keeps the certificate a device registered with, which names the provisional device
/// ID (e2ee.md §3.2): the list ties the server's ID to the key.
pub(crate) fn own_device_verification(
  device: &DeviceResponse,
  user: UserId,
  identity: &IdentityPublicKey,
  maybe_list: Option<&VerifiedDeviceList>,
) -> OwnDeviceCheck {
  let Ok(certificate) = SignedDeviceCertificate::from_bytes(device.cross_signature.as_slice()) else {
    return OwnDeviceCheck::Unverified;
  };
  let Ok(verified) = certificate.verify(identity) else {
    return OwnDeviceCheck::Unverified;
  };
  if verified.user_id() != convert::user(user) {
    return OwnDeviceCheck::Unverified;
  }
  let device_id = convert::device(device.id);
  if verified.device_id() != device_id && verified.device_id() != super::enroll::provisional_device_id() {
    // The final certificate of another device, presented for this one.
    return OwnDeviceCheck::Unverified;
  }
  let signing_key = verified.signing_key();
  let Some(list) = maybe_list else {
    return OwnDeviceCheck::Pending;
  };
  let entries = list.devices();
  if entries
    .iter()
    .any(|entry| entry.device_id == device_id && entry.signing_key == *signing_key)
  {
    return OwnDeviceCheck::Verified;
  }
  // Revoked, or the key of a listed device under another ID (a copied certificate).
  let is_copied = entries.iter().any(|entry| entry.signing_key == *signing_key);
  if list.is_revoked(device_id) || is_copied {
    return OwnDeviceCheck::Unverified;
  }
  OwnDeviceCheck::Pending
}
