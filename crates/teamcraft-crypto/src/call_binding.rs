//! Binding a call's DAVE key to a device identity (e2ee.md §8).
//!
//! DAVE v1 signs MLS with P-256 keys, while TeamCraft device keys are Ed25519, so the device key
//! can't be the DAVE credential key itself. Instead every device generates a fresh P-256 key per
//! call and signs a [`CallKeyBinding`] with its device key: "for call C, user U's device D uses DAVE
//! key K until T". Members verify the binding against the device's certificate
//! ([`VerifiedDevice`]) and accept a DAVE member only if its MLS leaf signature key is K. The SFU
//! (the DAVE external sender) can then propose members, but not invent or swap their keys.

use std::fmt;

use subtle::ConstantTimeEq;
use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize, VLBytes};

use crate::identity::{DeviceKeyPair, Ed25519Signature, IdentityError, VerifiedDevice};
use crate::ids::{DeviceId, UserId};

/// Version of the [`CallKeyBinding`] encoding.
const CALL_KEY_BINDING_VERSION: u8 = 1;
/// Domain separation label of the binding signature (see `domain`).
const CALL_KEY_BINDING_LABEL: &str = "TeamCraft-v1 call key binding";
/// Bindings valid for longer than this are rejected even if signed: they are only needed while a
/// device joins.
pub const MAX_CALL_KEY_BINDING_VALIDITY_SECS: u64 = 3600;

/// A call (one DAVE group); the server's Snowflake call id.
#[derive(
  Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, TlsSerialize, TlsDeserializeBytes, TlsSize,
)]
pub struct CallId(u64);

/// "For this call, this device uses this DAVE signature key until `not_after`."
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct CallKeyBinding {
  version: u8,
  pub call_id: CallId,
  pub user_id: UserId,
  pub device_id: DeviceId,
  /// SEC1-encoded P-256 public key (the DAVE MLS signature key).
  dave_public_key: VLBytes,
  /// Unix seconds after which the binding is no longer accepted.
  pub not_after: u64,
}

/// A binding with the device key's signature. Untrusted until [`SignedCallKeyBinding::verify`].
#[derive(Clone, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SignedCallKeyBinding {
  binding: CallKeyBinding,
  signature: Ed25519Signature,
}

/// A DAVE key whose binding was verified. Only [`SignedCallKeyBinding::verify`] creates it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedCallKey {
  device_id: DeviceId,
  dave_public_key: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum CallBindingError {
  #[error("unsupported call key binding version {0}")]
  UnsupportedVersion(u8),
  #[error("call key binding is for another call")]
  WrongCall,
  #[error("call key binding names another user or device than the certificate")]
  WrongDevice,
  #[error("call key binding expired")]
  Expired,
  #[error("call key binding is valid for longer than allowed")]
  ValidityTooLong,
  #[error("call key binding signature is invalid")]
  InvalidSignature,
  #[error("the DAVE member's key is not the bound key")]
  KeyMismatch,
  #[error("encoding failed: {0}")]
  Encoding(#[from] tls_codec::Error),
}

impl CallId {
  pub const fn new(value: u64) -> Self {
    Self(value)
  }

  pub const fn get(self) -> u64 {
    self.0
  }
}

impl DeviceKeyPair {
  /// Signs this device's DAVE key for one call. Use a fresh P-256 key for every call.
  pub fn sign_call_key_binding(
    &self,
    call_id: CallId,
    dave_public_key: &[u8],
    not_after: u64,
  ) -> Result<SignedCallKeyBinding, CallBindingError> {
    let binding = CallKeyBinding {
      version: CALL_KEY_BINDING_VERSION,
      call_id,
      user_id: self.user_id(),
      device_id: self.device_id(),
      dave_public_key: VLBytes::new(dave_public_key.to_vec()),
      not_after,
    };
    let signature = self.sign_labeled(CALL_KEY_BINDING_LABEL, &binding.tls_serialize_detached()?);
    Ok(SignedCallKeyBinding { binding, signature })
  }
}

impl SignedCallKeyBinding {
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, CallBindingError> {
    Ok(Self::tls_deserialize_exact_bytes(bytes)?)
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, CallBindingError> {
    Ok(self.tls_serialize_detached()?)
  }

  /// The claimed, not yet verified content; use it only to look up the device to verify against.
  pub fn unverified(&self) -> &CallKeyBinding {
    &self.binding
  }

  /// Checks the binding against the device's verified certificate, the call and the time.
  pub fn verify(
    &self,
    device: &VerifiedDevice,
    call_id: CallId,
    now_unix_secs: u64,
  ) -> Result<VerifiedCallKey, CallBindingError> {
    let binding = &self.binding;
    if binding.version != CALL_KEY_BINDING_VERSION {
      return Err(CallBindingError::UnsupportedVersion(binding.version));
    }
    device
      .signing_key()
      .verify_labeled(
        CALL_KEY_BINDING_LABEL,
        &binding.tls_serialize_detached()?,
        &self.signature,
      )
      .map_err(|error| match error {
        IdentityError::Encoding(encoding) => CallBindingError::Encoding(encoding),
        _ => CallBindingError::InvalidSignature,
      })?;
    if binding.user_id != device.user_id() || binding.device_id != device.device_id() {
      return Err(CallBindingError::WrongDevice);
    }
    if binding.call_id != call_id {
      return Err(CallBindingError::WrongCall);
    }
    if binding.not_after < now_unix_secs {
      return Err(CallBindingError::Expired);
    }
    if binding.not_after - now_unix_secs > MAX_CALL_KEY_BINDING_VALIDITY_SECS {
      return Err(CallBindingError::ValidityTooLong);
    }
    Ok(VerifiedCallKey {
      device_id: binding.device_id,
      dave_public_key: binding.dave_public_key.as_slice().to_vec(),
    })
  }
}

impl VerifiedCallKey {
  pub fn device_id(&self) -> DeviceId {
    self.device_id
  }

  pub fn dave_public_key(&self) -> &[u8] {
    &self.dave_public_key
  }

  /// Fails unless `leaf_signature_key` (from the member's MLS leaf) is the bound key.
  pub fn ensure_matches(&self, leaf_signature_key: &[u8]) -> Result<(), CallBindingError> {
    if bool::from(self.dave_public_key.as_slice().ct_eq(leaf_signature_key)) {
      Ok(())
    } else {
      Err(CallBindingError::KeyMismatch)
    }
  }
}

impl fmt::Debug for SignedCallKeyBinding {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SignedCallKeyBinding")
      .field("binding", &self.binding)
      .finish_non_exhaustive()
  }
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;
  use crate::identity::IdentityKeyPair;

  const ALICE: UserId = UserId::new(1);
  const LAPTOP: DeviceId = DeviceId::new(10);
  const PHONE: DeviceId = DeviceId::new(11);
  const CALL: CallId = CallId::new(500);
  const NOW: u64 = 1_800_000_000;
  /// 65-byte uncompressed SEC1 point shape; contents don't matter for the binding.
  const DAVE_KEY: [u8; 65] = [4; 65];

  mod accepted {
    use super::*;

    #[test]
    fn valid_binding_round_trips_and_matches_its_key() {
      let (laptop, verified_laptop, _) = devices();
      let signed = laptop.sign_call_key_binding(CALL, &DAVE_KEY, NOW + 600).unwrap();
      let decoded = SignedCallKeyBinding::from_bytes(&signed.to_bytes().unwrap()).unwrap();
      let key = decoded.verify(&verified_laptop, CALL, NOW).unwrap();
      assert_eq!(key.device_id(), LAPTOP);
      assert_eq!(key.dave_public_key(), DAVE_KEY);
      key.ensure_matches(&DAVE_KEY).unwrap();
    }
  }

  mod rejected {
    use super::*;

    #[test]
    fn wrong_call() {
      let (laptop, verified_laptop, _) = devices();
      let signed = laptop.sign_call_key_binding(CALL, &DAVE_KEY, NOW + 600).unwrap();
      assert!(matches!(
        signed.verify(&verified_laptop, CallId::new(501), NOW),
        Err(CallBindingError::WrongCall)
      ));
    }

    #[test]
    fn wrong_device() {
      let (laptop, _, verified_phone) = devices();
      let signed = laptop.sign_call_key_binding(CALL, &DAVE_KEY, NOW + 600).unwrap();
      // Signed by the laptop's key, presented as the phone: the signature doesn't verify.
      assert!(signed.verify(&verified_phone, CALL, NOW).is_err());
    }

    #[test]
    fn expired_and_too_long() {
      let (laptop, verified_laptop, _) = devices();
      let expired = laptop.sign_call_key_binding(CALL, &DAVE_KEY, NOW - 1).unwrap();
      assert!(matches!(
        expired.verify(&verified_laptop, CALL, NOW),
        Err(CallBindingError::Expired)
      ));
      let forever = laptop.sign_call_key_binding(CALL, &DAVE_KEY, u64::MAX).unwrap();
      assert!(matches!(
        forever.verify(&verified_laptop, CALL, NOW),
        Err(CallBindingError::ValidityTooLong)
      ));
    }

    #[test]
    fn mismatched_leaf_key() {
      let (laptop, verified_laptop, _) = devices();
      let key = laptop
        .sign_call_key_binding(CALL, &DAVE_KEY, NOW + 600)
        .unwrap()
        .verify(&verified_laptop, CALL, NOW)
        .unwrap();
      let mut other = DAVE_KEY;
      other[64] ^= 1;
      assert!(matches!(
        key.ensure_matches(&other),
        Err(CallBindingError::KeyMismatch)
      ));
    }

    #[test]
    fn tampered_binding() {
      let (laptop, verified_laptop, _) = devices();
      let mut bytes = laptop
        .sign_call_key_binding(CALL, &DAVE_KEY, NOW + 600)
        .unwrap()
        .to_bytes()
        .unwrap();
      // Flip a byte of the DAVE key inside the signed part.
      bytes[30] ^= 0x01;
      let tampered = SignedCallKeyBinding::from_bytes(&bytes).unwrap();
      assert!(matches!(
        tampered.verify(&verified_laptop, CALL, NOW),
        Err(CallBindingError::InvalidSignature)
      ));
      assert!(SignedCallKeyBinding::from_bytes(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn label_is_domain_separated() {
      assert!(CALL_KEY_BINDING_LABEL.starts_with("TeamCraft-v1 "));
      assert!(CALL_KEY_BINDING_LABEL.len() < 64);
    }
  }

  /// Alice's laptop key pair plus verified certificates of her laptop and phone.
  fn devices() -> (DeviceKeyPair, VerifiedDevice, VerifiedDevice) {
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let laptop = DeviceKeyPair::generate(ALICE, LAPTOP, &mut OsRng);
    let phone = DeviceKeyPair::generate(ALICE, PHONE, &mut OsRng);
    let verify = |device: &DeviceKeyPair| {
      identity
        .certify_device(ALICE, &device.public_keys())
        .unwrap()
        .verify(&identity.public_key())
        .unwrap()
    };
    let verified_laptop = verify(&laptop);
    let verified_phone = verify(&phone);
    (laptop, verified_laptop, verified_phone)
  }
}
