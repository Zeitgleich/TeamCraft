//! Sealed direct path offers of a 1:1 call (issue #127, e2ee.md §8.4).
//!
//! Two devices of a call exchange their direct path candidates through the SFU. The offers are
//! sealed under a key only members of the call's current DAVE epoch hold: the MLS exporter secret
//! of [`DIRECT_PATH_EXPORTER_LABEL`], expanded with HKDF-SHA256. The associated data binds an offer
//! to its call, epoch, sender and receiver, so the SFU can't read, forge, replay into another call or
//! epoch, or turn an offer around to its sender.
//!
//! Wire format: version (1 byte) ‖ epoch (8 bytes, big endian) ‖ nonce (24 bytes) ‖ ciphertext.

use std::fmt;

use hkdf::Hkdf;
use rand_core::CryptoRngCore;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::aead::{self, NONCE_LEN, TAG_LEN};
use crate::call_binding::CallId;
use crate::domain;
use crate::ids::DeviceId;
use crate::secret::{SYMMETRIC_KEY_LEN, SecretBytes, SymmetricKey};

/// The MLS exporter label the DAVE group exports the direct path secret with (empty context).
pub const DIRECT_PATH_EXPORTER_LABEL: &str = domain::DIRECT_PATH_EXPORTER;
/// How many bytes to export.
pub const DIRECT_PATH_EXPORTER_LEN: usize = SYMMETRIC_KEY_LEN;
/// Version of the sealed offer format.
const OFFER_VERSION: u8 = 1;
const EPOCH_LEN: usize = 8;
const HEADER_LEN: usize = 1 + EPOCH_LEN + NONCE_LEN;

/// The offer key of one DAVE epoch. Holds no secret in its `Debug` output.
pub struct DirectPathKey {
  epoch: u64,
  offer_key: SymmetricKey,
}

/// Who an offer is from and for, in which call. Bound into the seal as associated data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfferContext {
  pub call_id: CallId,
  pub from: DeviceId,
  pub to: DeviceId,
}

/// Sealing or opening an offer failed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DirectPathError {
  #[error("the exported secret has {0} bytes, expected {DIRECT_PATH_EXPORTER_LEN}")]
  ExporterLength(usize),
  #[error("an offer can't be from and for the same device")]
  SameDevice,
  #[error("the sealed offer is truncated")]
  Truncated,
  #[error("unknown sealed offer version {0}")]
  UnknownVersion(u8),
  #[error("the offer is sealed for epoch {sealed}, this device holds epoch {held}")]
  OtherEpoch { sealed: u64, held: u64 },
  /// Wrong key, tampered bytes or another context: deliberately not told apart.
  #[error("the offer doesn't open")]
  Unreadable,
}

impl DirectPathKey {
  /// The key of `epoch` from the DAVE group's exporter secret of [`DIRECT_PATH_EXPORTER_LABEL`].
  pub fn from_exporter(epoch: u64, exported: &[u8]) -> Result<Self, DirectPathError> {
    if exported.len() != DIRECT_PATH_EXPORTER_LEN {
      return Err(DirectPathError::ExporterLength(exported.len()));
    }
    let hkdf = Hkdf::<Sha256>::new(None, exported);
    let mut output = Zeroizing::new([0u8; SYMMETRIC_KEY_LEN]);
    // Invariant: 32 bytes is far below HKDF-SHA256's 8160-byte output limit.
    if hkdf
      .expand(domain::DIRECT_PATH_OFFER_KEY.as_bytes(), output.as_mut())
      .is_err()
    {
      unreachable!("HKDF output length is valid");
    }
    Ok(Self {
      epoch,
      offer_key: SymmetricKey::from_bytes(*output),
    })
  }

  /// The DAVE epoch this key belongs to.
  pub fn epoch(&self) -> u64 {
    self.epoch
  }

  /// Seals `plaintext` (the encoded offer) from `context.from` to `context.to`.
  pub fn seal(
    &self,
    context: &OfferContext,
    plaintext: &[u8],
    rng: &mut (impl CryptoRngCore + ?Sized),
  ) -> Result<Vec<u8>, DirectPathError> {
    if context.from == context.to {
      return Err(DirectPathError::SameDevice);
    }
    let aad = associated_data(context, self.epoch);
    let (nonce, ciphertext) =
      aead::seal(&self.offer_key, &aad, plaintext, rng).map_err(|_| DirectPathError::Unreadable)?;
    let mut sealed = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    sealed.push(OFFER_VERSION);
    sealed.extend_from_slice(&self.epoch.to_be_bytes());
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
  }

  /// Opens an offer sealed for this key's epoch; `context` names who sent it and this device.
  pub fn open(&self, context: &OfferContext, sealed: &[u8]) -> Result<SecretBytes, DirectPathError> {
    if context.from == context.to {
      return Err(DirectPathError::SameDevice);
    }
    let sealed_epoch = sealed_epoch(sealed)?;
    if sealed_epoch != self.epoch {
      return Err(DirectPathError::OtherEpoch {
        sealed: sealed_epoch,
        held: self.epoch,
      });
    }
    let (nonce_bytes, ciphertext) = sealed[1 + EPOCH_LEN..].split_at(NONCE_LEN);
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(nonce_bytes);
    let aad = associated_data(context, self.epoch);
    aead::open(&self.offer_key, &nonce, &aad, ciphertext).map_err(|_| DirectPathError::Unreadable)
  }
}

impl fmt::Debug for DirectPathKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("DirectPathKey")
      .field("epoch", &self.epoch)
      .field("offer_key", &"[redacted]")
      .finish()
  }
}

/// The epoch an offer says it is sealed for, without opening it: a device that holds another epoch
/// waits for the next offer instead of failing on this one.
pub fn sealed_epoch(sealed: &[u8]) -> Result<u64, DirectPathError> {
  if sealed.len() < HEADER_LEN + TAG_LEN {
    return Err(DirectPathError::Truncated);
  }
  if sealed[0] != OFFER_VERSION {
    return Err(DirectPathError::UnknownVersion(sealed[0]));
  }
  let mut epoch = [0u8; EPOCH_LEN];
  epoch.copy_from_slice(&sealed[1..1 + EPOCH_LEN]);
  Ok(u64::from_be_bytes(epoch))
}

fn associated_data(context: &OfferContext, epoch: u64) -> Vec<u8> {
  let mut body = Vec::with_capacity(1 + 4 * 8);
  body.push(OFFER_VERSION);
  body.extend_from_slice(&context.call_id.get().to_be_bytes());
  body.extend_from_slice(&epoch.to_be_bytes());
  body.extend_from_slice(&context.from.get().to_be_bytes());
  body.extend_from_slice(&context.to.get().to_be_bytes());
  domain::labeled(domain::DIRECT_PATH_OFFER, &body)
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  const EXPORTED: [u8; 32] = [7; 32];
  const OFFER: &[u8] = br#"{"candidates":[]}"#;

  #[test]
  fn an_offer_opens_for_its_receiver() {
    let key = key(3);
    let sealed = key.seal(&context(), OFFER, &mut OsRng).unwrap();
    assert_eq!(sealed_epoch(&sealed), Ok(3));
    assert_eq!(key.open(&context(), &sealed).unwrap().expose_secret(), OFFER);
  }

  #[test]
  fn the_offer_key_is_not_the_exported_secret_and_is_deterministic() {
    let first = key(1);
    let second = key(1);
    assert!(first.offer_key.is_same_key(&second.offer_key));
    assert_ne!(first.offer_key.expose_secret(), &EXPORTED);
    assert!(!format!("{first:?}").contains("07"));
  }

  mod rejection {
    use super::*;

    #[test]
    fn another_epochs_key_or_secret_does_not_open_it() {
      let sealed = key(3).seal(&context(), OFFER, &mut OsRng).unwrap();
      assert_eq!(
        key(4).open(&context(), &sealed).unwrap_err(),
        DirectPathError::OtherEpoch { sealed: 3, held: 4 }
      );
      let other = DirectPathKey::from_exporter(3, &[8; 32]).unwrap();
      assert_eq!(
        other.open(&context(), &sealed).unwrap_err(),
        DirectPathError::Unreadable
      );
    }

    #[test]
    fn a_turned_around_or_redirected_offer_does_not_open() {
      let key = key(3);
      let sealed = key.seal(&context(), OFFER, &mut OsRng).unwrap();
      let reversed = OfferContext {
        from: context().to,
        to: context().from,
        ..context()
      };
      assert_eq!(
        key.open(&reversed, &sealed).unwrap_err(),
        DirectPathError::Unreadable
      );
      let other_receiver = OfferContext {
        to: DeviceId::new(99),
        ..context()
      };
      assert_eq!(
        key.open(&other_receiver, &sealed).unwrap_err(),
        DirectPathError::Unreadable
      );
      let other_call = OfferContext {
        call_id: CallId::new(2),
        ..context()
      };
      assert_eq!(
        key.open(&other_call, &sealed).unwrap_err(),
        DirectPathError::Unreadable
      );
    }

    #[test]
    fn a_relabelled_epoch_does_not_open() {
      // The SFU rewrites the header's epoch: the AEAD covers the epoch, so it fails.
      let mut sealed = key(3).seal(&context(), OFFER, &mut OsRng).unwrap();
      sealed[1..9].copy_from_slice(&4u64.to_be_bytes());
      assert_eq!(
        key(4).open(&context(), &sealed).unwrap_err(),
        DirectPathError::Unreadable
      );
    }

    #[test]
    fn tampered_truncated_and_unknown_offers_are_refused() {
      let key = key(3);
      let sealed = key.seal(&context(), OFFER, &mut OsRng).unwrap();
      let mut tampered = sealed.clone();
      let last = tampered.len() - 1;
      tampered[last] ^= 1;
      assert_eq!(
        key.open(&context(), &tampered).unwrap_err(),
        DirectPathError::Unreadable
      );
      assert_eq!(
        key
          .open(&context(), &sealed[..HEADER_LEN + TAG_LEN - 1])
          .unwrap_err(),
        DirectPathError::Truncated
      );
      assert_eq!(sealed_epoch(&[]), Err(DirectPathError::Truncated));
      let mut newer = sealed;
      newer[0] = 2;
      assert_eq!(
        key.open(&context(), &newer).unwrap_err(),
        DirectPathError::UnknownVersion(2)
      );
    }

    #[test]
    fn same_device_and_wrong_secret_length_are_refused() {
      let key = key(3);
      let own = OfferContext {
        to: context().from,
        ..context()
      };
      assert_eq!(
        key.seal(&own, OFFER, &mut OsRng).unwrap_err(),
        DirectPathError::SameDevice
      );
      assert_eq!(
        DirectPathKey::from_exporter(3, &[0; 16]).unwrap_err(),
        DirectPathError::ExporterLength(16)
      );
    }
  }

  fn key(epoch: u64) -> DirectPathKey {
    DirectPathKey::from_exporter(epoch, &EXPORTED).unwrap()
  }

  fn context() -> OfferContext {
    OfferContext {
      call_id: CallId::new(1),
      from: DeviceId::new(10),
      to: DeviceId::new(20),
    }
  }
}
