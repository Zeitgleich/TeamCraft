//! Sealed in-call signals: reactions and raised hands (issue #2324, e2ee.md §8.5).
//!
//! A device tells everybody else in a call that it reacted or raised its hand. The signal goes
//! through the SFU to every other device, sealed under a key only members of the call's current
//! DAVE epoch hold: the MLS exporter secret of [`CALL_SIGNAL_EXPORTER_LABEL`], expanded with
//! HKDF-SHA256. The associated data binds a signal to its call, epoch and sender, so the SFU alone
//! can't read it, forge one, move it into another call or epoch, or pass one device's signal off as
//! another's. It learns only that a device sent something, and how large it is.
//!
//! Signals are not signed: every member of the epoch holds the same key. A member working with a
//! malicious SFU (which sets the sender's device ID it relays) can therefore make a signal look like
//! another member's. Accepted for reactions and raised hands, which carry no content.
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

/// The MLS exporter label the DAVE group exports the call signal secret with (empty context).
pub const CALL_SIGNAL_EXPORTER_LABEL: &str = domain::CALL_SIGNAL_EXPORTER;
/// How many bytes to export.
pub const CALL_SIGNAL_EXPORTER_LEN: usize = SYMMETRIC_KEY_LEN;
/// Version of the sealed signal format.
const SIGNAL_VERSION: u8 = 1;
const EPOCH_LEN: usize = 8;
const HEADER_LEN: usize = 1 + EPOCH_LEN + NONCE_LEN;

/// The signal key of one DAVE epoch. Holds no secret in its `Debug` output.
pub struct CallSignalKey {
  epoch: u64,
  signal_key: SymmetricKey,
}

/// Who a signal is from, in which call. Bound into the seal as associated data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignalContext {
  pub call_id: CallId,
  pub from: DeviceId,
}

/// Sealing or opening a signal failed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CallSignalError {
  #[error("the exported secret has {0} bytes, expected {CALL_SIGNAL_EXPORTER_LEN}")]
  ExporterLength(usize),
  #[error("the sealed signal is truncated")]
  Truncated,
  #[error("unknown sealed signal version {0}")]
  UnknownVersion(u8),
  #[error("the signal is sealed for epoch {sealed}, this device holds epoch {held}")]
  OtherEpoch { sealed: u64, held: u64 },
  /// Wrong key, tampered bytes or another context: deliberately not told apart.
  #[error("the signal doesn't open")]
  Unreadable,
}

impl CallSignalKey {
  /// The key of `epoch` from the DAVE group's exporter secret of [`CALL_SIGNAL_EXPORTER_LABEL`].
  pub fn from_exporter(epoch: u64, exported: &[u8]) -> Result<Self, CallSignalError> {
    if exported.len() != CALL_SIGNAL_EXPORTER_LEN {
      return Err(CallSignalError::ExporterLength(exported.len()));
    }
    let hkdf = Hkdf::<Sha256>::new(None, exported);
    let mut output = Zeroizing::new([0u8; SYMMETRIC_KEY_LEN]);
    // Invariant: 32 bytes is far below HKDF-SHA256's 8160-byte output limit.
    if hkdf
      .expand(domain::CALL_SIGNAL_KEY.as_bytes(), output.as_mut())
      .is_err()
    {
      unreachable!("HKDF output length is valid");
    }
    Ok(Self {
      epoch,
      signal_key: SymmetricKey::from_bytes(*output),
    })
  }

  /// The DAVE epoch this key belongs to.
  pub fn epoch(&self) -> u64 {
    self.epoch
  }

  /// Seals `plaintext` (the encoded signal) from `context.from` to the call.
  pub fn seal(
    &self,
    context: &SignalContext,
    plaintext: &[u8],
    rng: &mut (impl CryptoRngCore + ?Sized),
  ) -> Result<Vec<u8>, CallSignalError> {
    let aad = associated_data(context, self.epoch);
    let (nonce, ciphertext) =
      aead::seal(&self.signal_key, &aad, plaintext, rng).map_err(|_| CallSignalError::Unreadable)?;
    let mut sealed = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    sealed.push(SIGNAL_VERSION);
    sealed.extend_from_slice(&self.epoch.to_be_bytes());
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
  }

  /// Opens a signal sealed for this key's epoch; `context` names who sent it.
  pub fn open(&self, context: &SignalContext, sealed: &[u8]) -> Result<SecretBytes, CallSignalError> {
    let sealed_epoch = sealed_epoch(sealed)?;
    if sealed_epoch != self.epoch {
      return Err(CallSignalError::OtherEpoch {
        sealed: sealed_epoch,
        held: self.epoch,
      });
    }
    let (nonce_bytes, ciphertext) = sealed[1 + EPOCH_LEN..].split_at(NONCE_LEN);
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(nonce_bytes);
    let aad = associated_data(context, self.epoch);
    aead::open(&self.signal_key, &nonce, &aad, ciphertext).map_err(|_| CallSignalError::Unreadable)
  }
}

impl fmt::Debug for CallSignalKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("CallSignalKey")
      .field("epoch", &self.epoch)
      .field("signal_key", &"[redacted]")
      .finish()
  }
}

/// The epoch a signal says it is sealed for, without opening it: a device that doesn't hold that
/// epoch yet keeps it for a moment instead of failing on it.
pub fn sealed_epoch(sealed: &[u8]) -> Result<u64, CallSignalError> {
  if sealed.len() < HEADER_LEN + TAG_LEN {
    return Err(CallSignalError::Truncated);
  }
  if sealed[0] != SIGNAL_VERSION {
    return Err(CallSignalError::UnknownVersion(sealed[0]));
  }
  let mut epoch = [0u8; EPOCH_LEN];
  epoch.copy_from_slice(&sealed[1..1 + EPOCH_LEN]);
  Ok(u64::from_be_bytes(epoch))
}

fn associated_data(context: &SignalContext, epoch: u64) -> Vec<u8> {
  let mut body = Vec::with_capacity(1 + 3 * 8);
  body.push(SIGNAL_VERSION);
  body.extend_from_slice(&context.call_id.get().to_be_bytes());
  body.extend_from_slice(&epoch.to_be_bytes());
  body.extend_from_slice(&context.from.get().to_be_bytes());
  domain::labeled(domain::CALL_SIGNAL, &body)
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  const EXPORTED: [u8; 32] = [9; 32];
  const SIGNAL: &[u8] = br#"{"seq":1,"signal":{"type":"hand","is_raised":true}}"#;

  #[test]
  fn a_signal_opens_for_every_member() {
    let key = key(3);
    let sealed = key.seal(&context(), SIGNAL, &mut OsRng).unwrap();
    assert_eq!(sealed_epoch(&sealed), Ok(3));
    assert_eq!(key.open(&context(), &sealed).unwrap().expose_secret(), SIGNAL);
  }

  #[test]
  fn the_signal_key_is_derived_not_exported_and_not_the_direct_path_key() {
    let first = key(1);
    assert!(first.signal_key.is_same_key(&key(1).signal_key));
    assert_ne!(first.signal_key.expose_secret(), &EXPORTED);
    assert!(!format!("{first:?}").contains("09"));
    let offer_key = crate::direct_path::DirectPathKey::from_exporter(1, &EXPORTED).unwrap();
    let offer_context = crate::direct_path::OfferContext {
      call_id: context().call_id,
      from: context().from,
      to: DeviceId::new(20),
    };
    // Even from the same exported bytes, an offer and a signal are sealed apart.
    let offer = offer_key.seal(&offer_context, SIGNAL, &mut OsRng).unwrap();
    assert_eq!(
      first.open(&context(), &offer).unwrap_err(),
      CallSignalError::Unreadable
    );
  }

  mod rejection {
    use super::*;

    #[test]
    fn another_epochs_key_or_secret_does_not_open_it() {
      let sealed = key(3).seal(&context(), SIGNAL, &mut OsRng).unwrap();
      assert_eq!(
        key(4).open(&context(), &sealed).unwrap_err(),
        CallSignalError::OtherEpoch { sealed: 3, held: 4 }
      );
      let other = CallSignalKey::from_exporter(3, &[8; 32]).unwrap();
      assert_eq!(
        other.open(&context(), &sealed).unwrap_err(),
        CallSignalError::Unreadable
      );
    }

    #[test]
    fn a_signal_passed_off_as_another_senders_or_calls_does_not_open() {
      let key = key(3);
      let sealed = key.seal(&context(), SIGNAL, &mut OsRng).unwrap();
      let other_sender = SignalContext {
        from: DeviceId::new(11),
        ..context()
      };
      assert_eq!(
        key.open(&other_sender, &sealed).unwrap_err(),
        CallSignalError::Unreadable
      );
      let other_call = SignalContext {
        call_id: CallId::new(2),
        ..context()
      };
      assert_eq!(
        key.open(&other_call, &sealed).unwrap_err(),
        CallSignalError::Unreadable
      );
    }

    #[test]
    fn a_relabelled_epoch_does_not_open() {
      let mut sealed = key(3).seal(&context(), SIGNAL, &mut OsRng).unwrap();
      sealed[1..9].copy_from_slice(&4u64.to_be_bytes());
      assert_eq!(
        key(4).open(&context(), &sealed).unwrap_err(),
        CallSignalError::Unreadable
      );
    }

    #[test]
    fn tampered_truncated_and_unknown_signals_are_refused() {
      let key = key(3);
      let sealed = key.seal(&context(), SIGNAL, &mut OsRng).unwrap();
      let mut tampered = sealed.clone();
      let last = tampered.len() - 1;
      tampered[last] ^= 1;
      assert_eq!(
        key.open(&context(), &tampered).unwrap_err(),
        CallSignalError::Unreadable
      );
      assert_eq!(
        key
          .open(&context(), &sealed[..HEADER_LEN + TAG_LEN - 1])
          .unwrap_err(),
        CallSignalError::Truncated
      );
      assert_eq!(sealed_epoch(&[]), Err(CallSignalError::Truncated));
      let mut newer = sealed;
      newer[0] = 2;
      assert_eq!(
        key.open(&context(), &newer).unwrap_err(),
        CallSignalError::UnknownVersion(2)
      );
    }

    #[test]
    fn a_wrong_secret_length_is_refused() {
      assert_eq!(
        CallSignalKey::from_exporter(3, &[0; 16]).unwrap_err(),
        CallSignalError::ExporterLength(16)
      );
    }
  }

  fn key(epoch: u64) -> CallSignalKey {
    CallSignalKey::from_exporter(epoch, &EXPORTED).unwrap()
  }

  fn context() -> SignalContext {
    SignalContext {
      call_id: CallId::new(1),
      from: DeviceId::new(10),
    }
  }
}
