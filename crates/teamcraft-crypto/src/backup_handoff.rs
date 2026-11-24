//! Handing a rotated backup key to the user's other devices (e2ee.md §6.6).
//!
//! When a device replaces the backup key (a kit was seen by others, a device was lost), the user's
//! other devices still hold the old one and could no longer open the key bag. The rotating device
//! therefore adds one wrap per active device of the user ([`WrapMethod::DeviceHandoff`]): the new
//! backup key sealed with HPKE to that device's X25519 agreement key, and the whole wrap signed by
//! the user's identity key.
//!
//! The signature is what makes this safe against the server: it knows every device's public
//! agreement key and could seal a backup key of its own choosing. A device only takes a hand-off
//! whose signature verifies against the identity key it already holds, and only if the key bag
//! stored next to it then opens (the caller's check), so a replayed older hand-off is no use either.

use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize};

use crate::backup::{BackupError, BackupKey, BackupKeyWrapper, WrapMethod, WrappedBackupKey, wrap_aad};
use crate::domain;
use crate::identity::{
  DeviceAgreementPublicKey, DeviceKeyPair, Ed25519Signature, IdentityKeyPair, IdentityPublicKey,
};
use crate::ids::{DeviceId, UserId};
use crate::org_recovery::{hpke_open, hpke_seal};
use crate::secret::SymmetricKey;

/// Seals the backup key to one device of the user, signed with the user's identity key.
pub struct HandoffSealer<'a> {
  identity: &'a IdentityKeyPair,
  device_id: DeviceId,
  agreement_key: DeviceAgreementPublicKey,
}

/// Opens a hand-off on the device it was sealed to, after checking the identity's signature.
pub struct HandoffOpener<'a> {
  device: &'a DeviceKeyPair,
  identity: IdentityPublicKey,
}

/// Which device a hand-off is for. Authenticated as the HPKE associated data and signed.
#[derive(Clone, Copy, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
struct HandoffTarget {
  device_id: DeviceId,
  agreement_key: DeviceAgreementPublicKey,
}

/// The public parameters of a hand-off wrap: its target and the identity key's signature over the
/// target and the sealed key.
#[derive(TlsSerialize, TlsDeserializeBytes, TlsSize)]
struct HandoffParameters {
  target: HandoffTarget,
  signature: Ed25519Signature,
}

/// What the identity key signs.
#[derive(TlsSerialize, TlsSize)]
struct HandoffStatement<'a> {
  user_id: UserId,
  target: HandoffTarget,
  sealed: &'a [u8],
}

impl<'a> HandoffSealer<'a> {
  /// `agreement_key` must come from a certificate of the device that verified against
  /// `identity`'s public key, and the device must be active in the user's signed device list.
  pub fn new(
    identity: &'a IdentityKeyPair,
    device_id: DeviceId,
    agreement_key: DeviceAgreementPublicKey,
  ) -> Self {
    Self {
      identity,
      device_id,
      agreement_key,
    }
  }
}

impl BackupKeyWrapper for HandoffSealer<'_> {
  fn method(&self) -> WrapMethod {
    WrapMethod::DeviceHandoff
  }

  fn wrap_key(
    &self,
    user_id: UserId,
    backup_key: &BackupKey,
    _rng: &mut dyn rand_core::CryptoRngCore,
  ) -> Result<WrappedBackupKey, BackupError> {
    let target = HandoffTarget {
      device_id: self.device_id,
      agreement_key: self.agreement_key,
    };
    let sealed = hpke_seal(
      self.agreement_key.as_bytes(),
      &handoff_info(user_id, target.device_id),
      &target_aad(user_id, target)?,
      backup_key.key().expose_secret(),
    )
    .map_err(|_| BackupError::EncryptionFailed)?;
    let statement = HandoffStatement {
      user_id,
      target,
      sealed: &sealed,
    }
    .tls_serialize_detached()?;
    let parameters = HandoffParameters {
      target,
      signature: self
        .identity
        .sign_labeled(domain::BACKUP_KEY_HANDOFF_SIGNATURE, &statement),
    }
    .tls_serialize_detached()?;
    Ok(WrappedBackupKey::from_parts(
      WrapMethod::DeviceHandoff,
      parameters,
      sealed,
    ))
  }

  fn unwrap_key(&self, _user_id: UserId, _wrapped: &WrappedBackupKey) -> Result<BackupKey, BackupError> {
    // Only the target device can open a hand-off; the sealer has no private key.
    Err(BackupError::MissingPrivateKey)
  }
}

impl std::fmt::Debug for HandoffSealer<'_> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("HandoffSealer")
      .field("device_id", &self.device_id)
      .finish_non_exhaustive()
  }
}

impl<'a> HandoffOpener<'a> {
  /// `identity` is the user's identity public key as this device holds it (from its own key bag),
  /// never one the server supplied.
  pub fn new(device: &'a DeviceKeyPair, identity: IdentityPublicKey) -> Self {
    Self { device, identity }
  }

  /// Whether `wrapped` is a hand-off addressed to this device (its ID and agreement key). Says
  /// nothing about its signature.
  pub fn is_addressed_to_me(&self, wrapped: &WrappedBackupKey) -> bool {
    wrapped.method() == WrapMethod::DeviceHandoff
      && HandoffParameters::tls_deserialize_exact_bytes(wrapped.parameters())
        .is_ok_and(|parameters| parameters.target == self.my_target())
  }

  fn my_target(&self) -> HandoffTarget {
    HandoffTarget {
      device_id: self.device.device_id(),
      agreement_key: self.device.public_keys().agreement_key,
    }
  }
}

impl BackupKeyWrapper for HandoffOpener<'_> {
  fn method(&self) -> WrapMethod {
    WrapMethod::DeviceHandoff
  }

  fn wrap_key(
    &self,
    _user_id: UserId,
    _backup_key: &BackupKey,
    _rng: &mut dyn rand_core::CryptoRngCore,
  ) -> Result<WrappedBackupKey, BackupError> {
    // Sealing needs the identity key pair: `HandoffSealer`.
    Err(BackupError::EncryptionFailed)
  }

  fn unwrap_key(&self, user_id: UserId, wrapped: &WrappedBackupKey) -> Result<BackupKey, BackupError> {
    if wrapped.method() != WrapMethod::DeviceHandoff {
      return Err(BackupError::WrongMethod {
        expected: WrapMethod::DeviceHandoff,
        actual: wrapped.method(),
      });
    }
    if wrapped.version() != 1 {
      return Err(BackupError::UnsupportedVersion(wrapped.version()));
    }
    let parameters = HandoffParameters::tls_deserialize_exact_bytes(wrapped.parameters())?;
    if user_id != self.device.user_id() || parameters.target != self.my_target() {
      return Err(BackupError::HandoffNotTrusted);
    }
    let statement = HandoffStatement {
      user_id,
      target: parameters.target,
      sealed: wrapped.ciphertext(),
    }
    .tls_serialize_detached()?;
    self
      .identity
      .verify_labeled(
        domain::BACKUP_KEY_HANDOFF_SIGNATURE,
        &statement,
        &parameters.signature,
      )
      .map_err(|_| BackupError::HandoffNotTrusted)?;
    let (_, agreement) = self.device.to_secrets();
    let opened = hpke_open(
      agreement.expose_secret(),
      &handoff_info(user_id, parameters.target.device_id),
      &target_aad(user_id, parameters.target)?,
      wrapped.ciphertext(),
    )
    .map_err(|_| BackupError::UnwrapFailed)?;
    let key = SymmetricKey::from_slice(opened.expose_secret()).ok_or(BackupError::UnwrapFailed)?;
    Ok(BackupKey::from_key(key))
  }
}

impl std::fmt::Debug for HandoffOpener<'_> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("HandoffOpener")
      .field("device_id", &self.device.device_id())
      .finish_non_exhaustive()
  }
}

fn handoff_info(user_id: UserId, device_id: DeviceId) -> Vec<u8> {
  let mut body = user_id.get().to_be_bytes().to_vec();
  body.extend_from_slice(&device_id.get().to_be_bytes());
  domain::labeled(domain::BACKUP_KEY_HANDOFF, &body)
}

/// The usual wrap AAD (user, method, version) over the target, so a hand-off can't be moved to
/// another user or relabelled as another method.
fn target_aad(user_id: UserId, target: HandoffTarget) -> Result<Vec<u8>, BackupError> {
  wrap_aad(
    user_id,
    WrapMethod::DeviceHandoff,
    &target.tls_serialize_detached()?,
  )
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use subtle::ConstantTimeEq;

  use super::*;

  const ALICE: UserId = UserId::new(1);
  const LAPTOP: DeviceId = DeviceId::new(10);
  const PHONE: DeviceId = DeviceId::new(11);

  #[test]
  fn the_addressed_device_opens_the_hand_off() {
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let phone = DeviceKeyPair::generate(ALICE, PHONE, &mut OsRng);
    let backup_key = BackupKey::generate(&mut OsRng);
    let wrapped = seal_to(&identity, &phone, &backup_key);
    let decoded = WrappedBackupKey::from_bytes(&wrapped.to_bytes().unwrap()).unwrap();

    let opener = HandoffOpener::new(&phone, identity.public_key());
    assert!(opener.is_addressed_to_me(&decoded));
    let opened = opener.unwrap_key(ALICE, &decoded).unwrap();
    assert!(bool::from(opened.key().ct_eq(backup_key.key())));
  }

  mod rejection {
    use super::*;

    #[test]
    fn another_device_cannot_open_it() {
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let phone = DeviceKeyPair::generate(ALICE, PHONE, &mut OsRng);
      let laptop = DeviceKeyPair::generate(ALICE, LAPTOP, &mut OsRng);
      let wrapped = seal_to(&identity, &phone, &BackupKey::generate(&mut OsRng));

      let opener = HandoffOpener::new(&laptop, identity.public_key());
      assert!(!opener.is_addressed_to_me(&wrapped));
      assert!(matches!(
        opener.unwrap_key(ALICE, &wrapped),
        Err(BackupError::HandoffNotTrusted)
      ));
    }

    #[test]
    fn a_hand_off_the_server_sealed_is_refused() {
      // The server knows the phone's public agreement key and seals a key of its choosing, signed
      // with an identity key of its own.
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let server = IdentityKeyPair::generate(&mut OsRng);
      let phone = DeviceKeyPair::generate(ALICE, PHONE, &mut OsRng);
      let forged = seal_to(&server, &phone, &BackupKey::generate(&mut OsRng));

      assert!(matches!(
        HandoffOpener::new(&phone, identity.public_key()).unwrap_key(ALICE, &forged),
        Err(BackupError::HandoffNotTrusted)
      ));
    }

    #[test]
    fn tampered_or_moved_hand_offs_are_refused() {
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let phone = DeviceKeyPair::generate(ALICE, PHONE, &mut OsRng);
      let wrapped = seal_to(&identity, &phone, &BackupKey::generate(&mut OsRng));
      let opener = HandoffOpener::new(&phone, identity.public_key());

      let mut ciphertext = wrapped.ciphertext().to_vec();
      let last = ciphertext.len() - 1;
      ciphertext[last] ^= 1;
      let tampered = WrappedBackupKey::from_parts(
        WrapMethod::DeviceHandoff,
        wrapped.parameters().to_vec(),
        ciphertext,
      );
      assert!(matches!(
        opener.unwrap_key(ALICE, &tampered),
        Err(BackupError::HandoffNotTrusted)
      ));

      // Another user's device with the same IDs.
      let bob_phone = DeviceKeyPair::generate(UserId::new(2), PHONE, &mut OsRng);
      assert!(
        HandoffOpener::new(&bob_phone, identity.public_key())
          .unwrap_key(UserId::new(2), &wrapped)
          .is_err()
      );

      let mut parameters = wrapped.parameters().to_vec();
      parameters[0] ^= 1;
      let moved = WrappedBackupKey::from_parts(
        WrapMethod::DeviceHandoff,
        parameters,
        wrapped.ciphertext().to_vec(),
      );
      assert!(opener.unwrap_key(ALICE, &moved).is_err());
    }
  }

  fn seal_to(identity: &IdentityKeyPair, device: &DeviceKeyPair, backup_key: &BackupKey) -> WrappedBackupKey {
    HandoffSealer::new(identity, device.device_id(), device.public_keys().agreement_key)
      .wrap_key(ALICE, backup_key, &mut OsRng)
      .unwrap()
  }
}
