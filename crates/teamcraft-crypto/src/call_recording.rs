//! Signed recording notices of a call (e2ee.md §8.3, issue #216).
//!
//! A device that records a call says so to everybody in it with a [`RecordingNotice`]: "in call C,
//! device D is recording (or stopped), notice number N". It signs the notice with its **DAVE
//! signature key of that call** (ECDSA P-256), the key its call key binding
//! ([`crate::call_binding`]) ties to the device certificate and the one the other members checked
//! before they let it into the DAVE group. Members verify a notice with that device's DAVE leaf
//! key, so a server that relays notices can neither forge one nor end a recording on screen.
//!
//! The signature covers `len(label) || label || notice` with its own label (`domain`), so a notice
//! never verifies as an MLS signature of the same key, nor the other way round.

use std::fmt;

use p256::ecdsa::signature::{Signer as _, Verifier as _};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize};
use zeroize::Zeroizing;

use crate::call_binding::CallId;
use crate::domain::{CALL_RECORDING_NOTICE, labeled};
use crate::ids::DeviceId;

/// Version of the [`RecordingNotice`] encoding.
const RECORDING_NOTICE_VERSION: u8 = 1;
/// Wire value of [`RecordingState::Recording`].
const STATE_RECORDING: u8 = 1;
/// Wire value of [`RecordingState::Stopped`].
const STATE_STOPPED: u8 = 0;
/// Length of a fixed-size (r || s) ECDSA P-256 signature.
const SIGNATURE_BYTES: usize = 64;

/// "In this call, this device is recording (or stopped); this is its notice number `sequence`."
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct RecordingNotice {
  version: u8,
  call_id: CallId,
  device_id: DeviceId,
  /// Grows with every notice of the device in this call; a verifier takes only a higher one.
  sequence: u64,
  state: u8,
}

/// Whether a device records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingState {
  Recording,
  Stopped,
}

/// A notice with its signature. Untrusted until [`SignedRecordingNotice::verify`].
#[derive(Clone, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SignedRecordingNotice {
  notice: RecordingNotice,
  /// Fixed-size (r || s) ECDSA P-256 signature. A fixed array, not a length-prefixed vector:
  /// a truncated vector trips a debug assertion in `tls_codec` instead of returning an error.
  signature: [u8; SIGNATURE_BYTES],
}

/// Signs notices with this device's DAVE signature key of one call.
pub struct RecordingNoticeSigner {
  key: SigningKey,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordingNoticeError {
  #[error("unsupported recording notice version {0}")]
  UnsupportedVersion(u8),
  #[error("unknown recording state {0}")]
  UnknownState(u8),
  #[error("the recording notice is for another call")]
  WrongCall,
  #[error("the recording notice names another device")]
  WrongDevice,
  #[error("the recording notice signature is invalid")]
  InvalidSignature,
  #[error("the DAVE key is not a P-256 key")]
  InvalidKey,
  #[error("encoding failed: {0}")]
  Encoding(#[from] tls_codec::Error),
}

impl RecordingState {
  fn to_wire(self) -> u8 {
    match self {
      Self::Recording => STATE_RECORDING,
      Self::Stopped => STATE_STOPPED,
    }
  }

  fn from_wire(value: u8) -> Result<Self, RecordingNoticeError> {
    match value {
      STATE_RECORDING => Ok(Self::Recording),
      STATE_STOPPED => Ok(Self::Stopped),
      other => Err(RecordingNoticeError::UnknownState(other)),
    }
  }
}

impl RecordingNotice {
  pub fn call_id(&self) -> CallId {
    self.call_id
  }

  pub fn device_id(&self) -> DeviceId {
    self.device_id
  }

  pub fn sequence(&self) -> u64 {
    self.sequence
  }

  /// The state; a notice with an unknown one fails [`SignedRecordingNotice::verify`] and
  /// [`SignedRecordingNotice::from_bytes`].
  pub fn state(&self) -> RecordingState {
    // Invariant: only `new` and `from_bytes` create notices, and both refuse unknown states.
    RecordingState::from_wire(self.state).unwrap_or(RecordingState::Stopped)
  }
}

impl RecordingNoticeSigner {
  /// From the raw P-256 scalar of the device's DAVE signature key (what `davey` generates). The
  /// bytes are only read; the caller keeps and wipes its copy.
  pub fn from_dave_private_key(private_key: &[u8]) -> Result<Self, RecordingNoticeError> {
    let key = SigningKey::from_slice(private_key).map_err(|_| RecordingNoticeError::InvalidKey)?;
    Ok(Self { key })
  }

  /// The SEC1 public key notices verify with: the DAVE leaf key of this device.
  pub fn public_key(&self) -> Vec<u8> {
    self
      .key
      .verifying_key()
      .to_encoded_point(false)
      .as_bytes()
      .to_vec()
  }

  /// Signs notice number `sequence` of `device_id` in `call_id`.
  pub fn sign(
    &self,
    call_id: CallId,
    device_id: DeviceId,
    sequence: u64,
    state: RecordingState,
  ) -> Result<SignedRecordingNotice, RecordingNoticeError> {
    let notice = RecordingNotice {
      version: RECORDING_NOTICE_VERSION,
      call_id,
      device_id,
      sequence,
      state: state.to_wire(),
    };
    let message = Zeroizing::new(labeled(CALL_RECORDING_NOTICE, &notice.tls_serialize_detached()?));
    let signature: Signature = self.key.sign(&message);
    Ok(SignedRecordingNotice {
      notice,
      signature: signature.to_bytes().into(),
    })
  }
}

impl SignedRecordingNotice {
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, RecordingNoticeError> {
    let signed = Self::tls_deserialize_exact_bytes(bytes)?;
    RecordingState::from_wire(signed.notice.state)?;
    Ok(signed)
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, RecordingNoticeError> {
    Ok(self.tls_serialize_detached()?)
  }

  /// The claimed, not yet verified content: what an app shows until the sender's key is known
  /// (only ever "recording", never a stop; e2ee.md §8.3).
  pub fn unverified(&self) -> &RecordingNotice {
    &self.notice
  }

  /// Checks the notice against the sender's DAVE leaf key (SEC1 P-256) in this call.
  pub fn verify(
    &self,
    dave_public_key: &[u8],
    call_id: CallId,
    device_id: DeviceId,
  ) -> Result<&RecordingNotice, RecordingNoticeError> {
    let notice = &self.notice;
    if notice.version != RECORDING_NOTICE_VERSION {
      return Err(RecordingNoticeError::UnsupportedVersion(notice.version));
    }
    let key = VerifyingKey::from_sec1_bytes(dave_public_key).map_err(|_| RecordingNoticeError::InvalidKey)?;
    let signature =
      Signature::from_slice(&self.signature).map_err(|_| RecordingNoticeError::InvalidSignature)?;
    let message = labeled(CALL_RECORDING_NOTICE, &notice.tls_serialize_detached()?);
    key
      .verify(&message, &signature)
      .map_err(|_| RecordingNoticeError::InvalidSignature)?;
    if notice.call_id != call_id {
      return Err(RecordingNoticeError::WrongCall);
    }
    if notice.device_id != device_id {
      return Err(RecordingNoticeError::WrongDevice);
    }
    RecordingState::from_wire(notice.state)?;
    Ok(notice)
  }
}

impl fmt::Debug for SignedRecordingNotice {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SignedRecordingNotice")
      .field("notice", &self.notice)
      .finish_non_exhaustive()
  }
}

impl fmt::Debug for RecordingNoticeSigner {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("RecordingNoticeSigner([redacted])")
  }
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  const CALL: CallId = CallId::new(500);
  const LAPTOP: DeviceId = DeviceId::new(10);
  const PHONE: DeviceId = DeviceId::new(11);

  mod accepted {
    use super::*;

    #[test]
    fn a_notice_round_trips_and_verifies_with_the_dave_key() {
      let signer = signer();
      for (sequence, state) in [(1, RecordingState::Recording), (2, RecordingState::Stopped)] {
        let signed = signer.sign(CALL, LAPTOP, sequence, state).unwrap();
        let decoded = SignedRecordingNotice::from_bytes(&signed.to_bytes().unwrap()).unwrap();
        let notice = decoded.verify(&signer.public_key(), CALL, LAPTOP).unwrap();
        assert_eq!(notice.state(), state);
        assert_eq!(notice.sequence(), sequence);
        assert_eq!((notice.call_id(), notice.device_id()), (CALL, LAPTOP));
      }
    }

    #[test]
    fn a_notice_stays_small() {
      let bytes = signer()
        .sign(CALL, LAPTOP, u64::MAX, RecordingState::Recording)
        .unwrap()
        .to_bytes()
        .unwrap();
      assert!(bytes.len() <= 100, "{} bytes", bytes.len());
    }
  }

  mod refused {
    use super::*;

    #[test]
    fn another_key_another_call_or_another_device_fails() {
      let signer = signer();
      let signed = signer.sign(CALL, LAPTOP, 1, RecordingState::Stopped).unwrap();
      assert!(matches!(
        signed.verify(&self::signer().public_key(), CALL, LAPTOP),
        Err(RecordingNoticeError::InvalidSignature)
      ));
      assert!(matches!(
        signed.verify(&signer.public_key(), CallId::new(501), LAPTOP),
        Err(RecordingNoticeError::WrongCall)
      ));
      assert!(matches!(
        signed.verify(&signer.public_key(), CALL, PHONE),
        Err(RecordingNoticeError::WrongDevice)
      ));
    }

    #[test]
    fn a_tampered_state_or_sequence_fails() {
      let signer = signer();
      let signed = signer.sign(CALL, LAPTOP, 7, RecordingState::Recording).unwrap();
      let mut stopped = signed.clone();
      stopped.notice.state = STATE_STOPPED;
      assert!(matches!(
        stopped.verify(&signer.public_key(), CALL, LAPTOP),
        Err(RecordingNoticeError::InvalidSignature)
      ));
      let mut later = signed;
      later.notice.sequence = 8;
      assert!(matches!(
        later.verify(&signer.public_key(), CALL, LAPTOP),
        Err(RecordingNoticeError::InvalidSignature)
      ));
    }

    #[test]
    fn malformed_bytes_and_keys_are_errors_not_panics() {
      let signer = signer();
      let bytes = signer
        .sign(CALL, LAPTOP, 1, RecordingState::Recording)
        .unwrap()
        .to_bytes()
        .unwrap();
      assert!(SignedRecordingNotice::from_bytes(&[]).is_err());
      assert!(SignedRecordingNotice::from_bytes(&bytes[..bytes.len() - 1]).is_err());
      let mut trailing = bytes.clone();
      trailing.push(0);
      assert!(SignedRecordingNotice::from_bytes(&trailing).is_err());
      let signed = SignedRecordingNotice::from_bytes(&bytes).unwrap();
      assert!(matches!(
        signed.verify(&[4; 10], CALL, LAPTOP),
        Err(RecordingNoticeError::InvalidKey)
      ));
      assert!(RecordingNoticeSigner::from_dave_private_key(&[0; 32]).is_err());
    }

    #[test]
    fn an_unknown_state_or_version_is_refused() {
      let signer = signer();
      let mut signed = signer.sign(CALL, LAPTOP, 1, RecordingState::Recording).unwrap();
      signed.notice.state = 9;
      let bytes = signed.to_bytes().unwrap();
      assert!(matches!(
        SignedRecordingNotice::from_bytes(&bytes),
        Err(RecordingNoticeError::UnknownState(9))
      ));
      let mut newer = signer.sign(CALL, LAPTOP, 1, RecordingState::Recording).unwrap();
      newer.notice.version = 2;
      assert!(matches!(
        newer.verify(&signer.public_key(), CALL, LAPTOP),
        Err(RecordingNoticeError::UnsupportedVersion(2))
      ));
    }
  }

  fn signer() -> RecordingNoticeSigner {
    let key = SigningKey::random(&mut OsRng);
    RecordingNoticeSigner::from_dave_private_key(&key.to_bytes()).unwrap()
  }
}
