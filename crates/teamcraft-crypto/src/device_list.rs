//! The user's signed device list (security review A4, e2ee.md §3).
//!
//! A device certificate says "this key belongs to the user" and never expires, so it can't take a
//! lost device back. The device list can: the user's identity key signs the set of active devices
//! and the devices revoked so far, with a sequence number that only grows. Other clients add a
//! device to a group, accept a commit that adds it, or seal a secret to it only if the newest list
//! they have seen names it, and they refuse lists older than one they saw.

use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize};

use crate::domain;
use crate::identity::{
  DeviceSigningPublicKey, Ed25519Signature, IdentityError, IdentityKeyPair, IdentityPublicKey, VerifiedDevice,
};
use crate::ids::{DeviceId, UserId};

const DEVICE_LIST_VERSION: u8 = 1;

/// The active and revoked devices of one user, as the user's identity key states them.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct DeviceList {
  version: u8,
  user_id: UserId,
  sequence: u64,
  devices: Vec<DeviceListEntry>,
  revoked: Vec<DeviceId>,
}

/// One active device: its ID and the signing key its certificate vouches for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct DeviceListEntry {
  pub device_id: DeviceId,
  pub signing_key: DeviceSigningPublicKey,
}

/// A device list with the identity key's signature, as stored on the server.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SignedDeviceList {
  list: DeviceList,
  signature: Ed25519Signature,
}

/// A device list whose signature was checked against the user's pinned identity key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedDeviceList {
  list: DeviceList,
}

#[derive(Debug, thiserror::Error)]
pub enum DeviceListError {
  #[error("unsupported device list version {0}")]
  UnsupportedVersion(u8),
  #[error("device list is for user {actual}, expected {expected}")]
  WrongUser { expected: UserId, actual: UserId },
  #[error("device list signature is invalid: {0}")]
  InvalidSignature(IdentityError),
  #[error("device list {offered} is older than {seen}, seen before")]
  Rollback { seen: u64, offered: u64 },
  #[error("device {0} is both active and revoked")]
  Contradiction(DeviceId),
  #[error("encoding failed: {0}")]
  Encoding(#[from] tls_codec::Error),
}

impl DeviceList {
  /// The first list of a user, or the next one after `previous` (its sequence plus one, revoked
  /// devices kept).
  pub fn next(
    user_id: UserId,
    maybe_previous: Option<&VerifiedDeviceList>,
    devices: Vec<DeviceListEntry>,
    newly_revoked: &[DeviceId],
  ) -> Self {
    let mut revoked: Vec<DeviceId> = maybe_previous
      .map(|previous| previous.list.revoked.clone())
      .unwrap_or_default();
    for device in newly_revoked {
      if !revoked.contains(device) {
        revoked.push(*device);
      }
    }
    let devices = devices
      .into_iter()
      .filter(|entry| !revoked.contains(&entry.device_id))
      .collect();
    Self {
      version: DEVICE_LIST_VERSION,
      user_id,
      sequence: maybe_previous.map_or(1, |previous| previous.list.sequence + 1),
      devices,
      revoked,
    }
  }

  /// Signs the list with the user's identity key (every device of the user holds it).
  pub fn sign(self, identity: &IdentityKeyPair) -> Result<SignedDeviceList, DeviceListError> {
    let signature = identity.sign_labeled(domain::DEVICE_LIST, &self.tls_serialize_detached()?);
    Ok(SignedDeviceList {
      list: self,
      signature,
    })
  }
}

impl SignedDeviceList {
  pub fn to_bytes(&self) -> Result<Vec<u8>, DeviceListError> {
    Ok(self.tls_serialize_detached()?)
  }

  pub fn from_bytes(bytes: &[u8]) -> Result<Self, DeviceListError> {
    Ok(Self::tls_deserialize_exact_bytes(bytes)?)
  }

  /// The claimed sequence number, before verification.
  pub fn unverified_sequence(&self) -> u64 {
    self.list.sequence
  }

  /// Checks the signature against `user`'s pinned identity key.
  pub fn verify(
    &self,
    user: UserId,
    identity: &IdentityPublicKey,
  ) -> Result<VerifiedDeviceList, DeviceListError> {
    let list = &self.list;
    if list.version != DEVICE_LIST_VERSION {
      return Err(DeviceListError::UnsupportedVersion(list.version));
    }
    if list.user_id != user {
      return Err(DeviceListError::WrongUser {
        expected: user,
        actual: list.user_id,
      });
    }
    identity
      .verify_labeled(
        domain::DEVICE_LIST,
        &list.tls_serialize_detached()?,
        &self.signature,
      )
      .map_err(DeviceListError::InvalidSignature)?;
    if let Some(entry) = list
      .devices
      .iter()
      .find(|entry| list.revoked.contains(&entry.device_id))
    {
      return Err(DeviceListError::Contradiction(entry.device_id));
    }
    Ok(VerifiedDeviceList { list: list.clone() })
  }

  /// Like [`Self::verify`], and refuses a list older than the one this client saw last.
  pub fn verify_newer_than(
    &self,
    user: UserId,
    identity: &IdentityPublicKey,
    maybe_seen: Option<&VerifiedDeviceList>,
  ) -> Result<VerifiedDeviceList, DeviceListError> {
    let verified = self.verify(user, identity)?;
    if let Some(seen) = maybe_seen
      && verified.sequence() < seen.sequence()
    {
      return Err(DeviceListError::Rollback {
        seen: seen.sequence(),
        offered: verified.sequence(),
      });
    }
    Ok(verified)
  }
}

impl VerifiedDeviceList {
  pub fn user_id(&self) -> UserId {
    self.list.user_id
  }

  pub fn sequence(&self) -> u64 {
    self.list.sequence
  }

  pub fn devices(&self) -> &[DeviceListEntry] {
    &self.list.devices
  }

  pub fn is_revoked(&self, device: DeviceId) -> bool {
    self.list.revoked.contains(&device)
  }

  /// Whether the list names this device with exactly this key.
  pub fn is_active(&self, device: &VerifiedDevice) -> bool {
    device.user_id() == self.list.user_id
      && self
        .list
        .devices
        .iter()
        .any(|entry| entry.device_id == device.device_id() && entry.signing_key == *device.signing_key())
  }
}

#[cfg(test)]
mod tests {
  use crate::membership::tests::TestUser;

  use super::*;

  #[test]
  fn lists_name_active_devices_and_keep_revoked_ones() {
    let alice = TestUser::new(1);
    let laptop = alice.new_device(101);
    let first = DeviceList::next(alice.user_id, None, vec![entry(&alice), entry(&laptop)], &[])
      .sign(&alice.identity)
      .unwrap();
    let decoded = SignedDeviceList::from_bytes(&first.to_bytes().unwrap()).unwrap();
    let verified = decoded
      .verify(alice.user_id, &alice.identity.public_key())
      .unwrap();
    assert_eq!(verified.sequence(), 1);
    assert!(verified.is_active(&verified_device(&laptop)));

    // The laptop is lost: the next list drops it and remembers it as revoked.
    let second = DeviceList::next(
      alice.user_id,
      Some(&verified),
      vec![entry(&alice), entry(&laptop)],
      &[laptop.device.device_id()],
    )
    .sign(&alice.identity)
    .unwrap()
    .verify_newer_than(alice.user_id, &alice.identity.public_key(), Some(&verified))
    .unwrap();
    assert_eq!(second.sequence(), 2);
    assert!(!second.is_active(&verified_device(&laptop)));
    assert!(second.is_revoked(laptop.device.device_id()));
    assert!(second.is_active(&verified_device(&alice)));
  }

  mod rejection {
    use super::*;

    #[test]
    fn rejects_older_lists_and_other_signers() {
      let alice = TestUser::new(1);
      let first = DeviceList::next(alice.user_id, None, vec![entry(&alice)], &[])
        .sign(&alice.identity)
        .unwrap();
      let verified = first.verify(alice.user_id, &alice.identity.public_key()).unwrap();
      let second = DeviceList::next(alice.user_id, Some(&verified), vec![entry(&alice)], &[])
        .sign(&alice.identity)
        .unwrap()
        .verify(alice.user_id, &alice.identity.public_key())
        .unwrap();
      assert!(matches!(
        first.verify_newer_than(alice.user_id, &alice.identity.public_key(), Some(&second)),
        Err(DeviceListError::Rollback { seen: 2, offered: 1 })
      ));

      // The server signs a list for Alice with a key of its own.
      let server = TestUser::new(1);
      let forged = DeviceList::next(alice.user_id, None, vec![entry(&server)], &[])
        .sign(&server.identity)
        .unwrap();
      assert!(matches!(
        forged.verify(alice.user_id, &alice.identity.public_key()),
        Err(DeviceListError::InvalidSignature(_))
      ));
      assert!(matches!(
        first.verify(UserId::new(2), &alice.identity.public_key()),
        Err(DeviceListError::WrongUser { .. })
      ));
      assert!(SignedDeviceList::from_bytes(&[1, 2, 3]).is_err());
    }

    #[test]
    fn a_revoked_device_never_comes_back() {
      let alice = TestUser::new(1);
      let laptop = alice.new_device(101);
      let first = DeviceList::next(
        alice.user_id,
        None,
        vec![entry(&alice)],
        &[laptop.device.device_id()],
      )
      .sign(&alice.identity)
      .unwrap()
      .verify(alice.user_id, &alice.identity.public_key())
      .unwrap();
      let next = DeviceList::next(
        alice.user_id,
        Some(&first),
        vec![entry(&alice), entry(&laptop)],
        &[],
      );
      assert!(
        next
          .devices
          .iter()
          .all(|e| e.device_id != laptop.device.device_id())
      );

      // A hand-made list that names a revoked device as active doesn't verify.
      let mut contradictory = next.clone();
      contradictory.devices.push(entry(&laptop));
      assert!(matches!(
        contradictory
          .sign(&alice.identity)
          .unwrap()
          .verify(alice.user_id, &alice.identity.public_key()),
        Err(DeviceListError::Contradiction(_))
      ));
    }
  }

  fn entry(user: &TestUser) -> DeviceListEntry {
    DeviceListEntry {
      device_id: user.device.device_id(),
      signing_key: user.device.public_keys().signing_key,
    }
  }

  fn verified_device(user: &TestUser) -> VerifiedDevice {
    user.certificate.verify(&user.identity.public_key()).unwrap()
  }
}
