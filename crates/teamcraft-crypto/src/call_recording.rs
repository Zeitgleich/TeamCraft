//! Signed recording notices and consents of a call (e2ee.md §8.3, issues #216 and #2115).
//!
//! A device that records a call says so to everybody in it with a [`RecordingNotice`]: "in call C,
//! device D is recording take T (or asks to, or stopped), notice number N". It signs the notice with
//! its **DAVE signature key of that call** (ECDSA P-256), the key its call key binding
//! ([`crate::call_binding`]) ties to the device certificate and the one the other members checked
//! before they let it into the DAVE group. Members verify a notice with that device's DAVE leaf
//! key, so a server that relays notices can neither forge one nor end a recording on screen.
//!
//! Where the recording asks for consent (`all_consent`, #2115), every other device answers with a
//! [`RecordingConsent`]: "in call C, device P allows (or declines) take T of recorder R, answer
//! number N", signed with P's own DAVE key of the call. The recorder mixes in only the devices
//! whose signed "allow" for this very take it verified, so neither the server nor the recorder can
//! make up a consent: a consent names the call, the recorder and the take, and verifies only with
//! the key of the device that gave it.
//!
//! Each signature covers `len(label) || label || body` with its own label (`domain`), so a notice
//! never verifies as a consent or an MLS signature of the same key, nor the other way round.

use std::fmt;

use p256::ecdsa::signature::{Signer as _, Verifier as _};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize};
use zeroize::Zeroizing;

use crate::call_binding::CallId;
use crate::domain::{CALL_RECORDING_CONSENT, CALL_RECORDING_NOTICE, labeled};
use crate::ids::DeviceId;

/// Version of the [`RecordingNotice`] encoding. Version 2 added the take and the consent flag
/// (#2115); a version 1 notice is refused.
const RECORDING_NOTICE_VERSION: u8 = 2;
/// Version of the [`RecordingConsent`] encoding.
const RECORDING_CONSENT_VERSION: u8 = 1;
/// Wire value of [`RecordingState::Stopped`].
const STATE_STOPPED: u8 = 0;
/// Wire value of [`RecordingState::Recording`].
const STATE_RECORDING: u8 = 1;
/// Wire value of [`RecordingState::Requesting`].
const STATE_REQUESTING: u8 = 2;
/// Wire value of [`ConsentAnswer::Decline`].
const ANSWER_DECLINE: u8 = 0;
/// Wire value of [`ConsentAnswer::Allow`].
const ANSWER_ALLOW: u8 = 1;
/// Length of a fixed-size (r || s) ECDSA P-256 signature.
const SIGNATURE_BYTES: usize = 64;

/// "In this call, this device is recording take `take` (or asks to, or stopped); this is its
/// notice number `sequence`."
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct RecordingNotice {
  version: u8,
  call_id: CallId,
  device_id: DeviceId,
  /// Grows with every notice of the device in this call; a verifier takes only a higher one.
  sequence: u64,
  /// The recording this notice is about: the number of its first notice. Consents name it.
  take: u64,
  /// 1 when the recording asks every other device for consent and records only who allowed it.
  asks_consent: u8,
  state: u8,
}

/// Whether a device records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingState {
  Recording,
  Stopped,
  /// Asks the others for consent and records nothing yet (#2115).
  Requesting,
}

/// What a notice says about the device's recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingAnnouncement {
  /// The recording: the number of its first notice. A new recording, a new take.
  pub take: u64,
  /// It asks every other device for consent and records only who allowed it.
  pub asks_consent: bool,
  pub state: RecordingState,
}

/// A notice with its signature. Untrusted until [`SignedRecordingNotice::verify`].
#[derive(Clone, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SignedRecordingNotice {
  notice: RecordingNotice,
  /// Fixed-size (r || s) ECDSA P-256 signature. A fixed array, not a length-prefixed vector:
  /// a truncated vector trips a debug assertion in `tls_codec` instead of returning an error.
  signature: [u8; SIGNATURE_BYTES],
}

/// "In this call, this device allows (or declines) take `take` of `recorder_device_id`; this is
/// its answer number `sequence`."
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct RecordingConsent {
  version: u8,
  call_id: CallId,
  /// The device that answers, and whose voice it is about.
  device_id: DeviceId,
  recorder_device_id: DeviceId,
  take: u64,
  /// Grows with every answer of the device; a verifier takes only a higher one, so a withdrawn
  /// consent can't be replayed back in.
  sequence: u64,
  answer: u8,
}

/// A participant's answer to a recording that asks for consent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsentAnswer {
  /// The device's voice may be in the recording.
  Allow,
  /// It stays out of the recording.
  Decline,
}

/// A consent with its signature. Untrusted until [`SignedRecordingConsent::verify`].
#[derive(Clone, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SignedRecordingConsent {
  consent: RecordingConsent,
  signature: [u8; SIGNATURE_BYTES],
}

/// Signs notices and consents with this device's DAVE signature key of one call.
pub struct RecordingNoticeSigner {
  key: SigningKey,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordingNoticeError {
  #[error("unsupported recording notice version {0}")]
  UnsupportedVersion(u8),
  #[error("unknown recording state {0}")]
  UnknownState(u8),
  #[error("unknown consent answer {0}")]
  UnknownAnswer(u8),
  #[error("the consent flag is neither 0 nor 1")]
  InvalidFlag,
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
      Self::Requesting => STATE_REQUESTING,
    }
  }

  fn from_wire(value: u8) -> Result<Self, RecordingNoticeError> {
    match value {
      STATE_RECORDING => Ok(Self::Recording),
      STATE_STOPPED => Ok(Self::Stopped),
      STATE_REQUESTING => Ok(Self::Requesting),
      other => Err(RecordingNoticeError::UnknownState(other)),
    }
  }
}

impl ConsentAnswer {
  fn to_wire(self) -> u8 {
    match self {
      Self::Allow => ANSWER_ALLOW,
      Self::Decline => ANSWER_DECLINE,
    }
  }

  fn from_wire(value: u8) -> Result<Self, RecordingNoticeError> {
    match value {
      ANSWER_ALLOW => Ok(Self::Allow),
      ANSWER_DECLINE => Ok(Self::Decline),
      other => Err(RecordingNoticeError::UnknownAnswer(other)),
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
    // Invariant: only `sign` and `from_bytes` create notices, and both refuse unknown states.
    RecordingState::from_wire(self.state).unwrap_or(RecordingState::Stopped)
  }

  /// The take, the consent flag and the state.
  pub fn announcement(&self) -> RecordingAnnouncement {
    RecordingAnnouncement {
      take: self.take,
      asks_consent: self.asks_consent == 1,
      state: self.state(),
    }
  }

  fn check_values(&self) -> Result<(), RecordingNoticeError> {
    if self.version != RECORDING_NOTICE_VERSION {
      return Err(RecordingNoticeError::UnsupportedVersion(self.version));
    }
    RecordingState::from_wire(self.state)?;
    if self.asks_consent > 1 {
      return Err(RecordingNoticeError::InvalidFlag);
    }
    Ok(())
  }
}

impl RecordingConsent {
  pub fn call_id(&self) -> CallId {
    self.call_id
  }

  pub fn device_id(&self) -> DeviceId {
    self.device_id
  }

  pub fn recorder_device_id(&self) -> DeviceId {
    self.recorder_device_id
  }

  pub fn take(&self) -> u64 {
    self.take
  }

  pub fn sequence(&self) -> u64 {
    self.sequence
  }

  pub fn answer(&self) -> ConsentAnswer {
    // Invariant: only `sign_consent` and `from_bytes` create consents, and both refuse unknown
    // answers. Declining is the safe reading anyway.
    ConsentAnswer::from_wire(self.answer).unwrap_or(ConsentAnswer::Decline)
  }

  fn check_values(&self) -> Result<(), RecordingNoticeError> {
    if self.version != RECORDING_CONSENT_VERSION {
      return Err(RecordingNoticeError::UnsupportedVersion(self.version));
    }
    ConsentAnswer::from_wire(self.answer)?;
    Ok(())
  }
}

impl RecordingNoticeSigner {
  /// From the raw P-256 scalar of the device's DAVE signature key (what `davey` generates). The
  /// bytes are only read; the caller keeps and wipes its copy.
  pub fn from_dave_private_key(private_key: &[u8]) -> Result<Self, RecordingNoticeError> {
    let key = SigningKey::from_slice(private_key).map_err(|_| RecordingNoticeError::InvalidKey)?;
    Ok(Self { key })
  }

  /// The SEC1 public key notices and consents verify with: the DAVE leaf key of this device.
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
    announcement: RecordingAnnouncement,
  ) -> Result<SignedRecordingNotice, RecordingNoticeError> {
    let notice = RecordingNotice {
      version: RECORDING_NOTICE_VERSION,
      call_id,
      device_id,
      sequence,
      take: announcement.take,
      asks_consent: u8::from(announcement.asks_consent),
      state: announcement.state.to_wire(),
    };
    let signature = self.signature(CALL_RECORDING_NOTICE, &notice.tls_serialize_detached()?);
    Ok(SignedRecordingNotice { notice, signature })
  }

  /// Signs answer number `sequence` of `device_id` (this device) to take `take` of
  /// `recorder_device_id` in `call_id`.
  pub fn sign_consent(
    &self,
    call_id: CallId,
    device_id: DeviceId,
    recorder_device_id: DeviceId,
    take: u64,
    sequence: u64,
    answer: ConsentAnswer,
  ) -> Result<SignedRecordingConsent, RecordingNoticeError> {
    let consent = RecordingConsent {
      version: RECORDING_CONSENT_VERSION,
      call_id,
      device_id,
      recorder_device_id,
      take,
      sequence,
      answer: answer.to_wire(),
    };
    let signature = self.signature(CALL_RECORDING_CONSENT, &consent.tls_serialize_detached()?);
    Ok(SignedRecordingConsent { consent, signature })
  }

  fn signature(&self, label: &str, body: &[u8]) -> [u8; SIGNATURE_BYTES] {
    let message = Zeroizing::new(labeled(label, body));
    let signature: Signature = self.key.sign(&message);
    signature.to_bytes().into()
  }
}

impl SignedRecordingNotice {
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, RecordingNoticeError> {
    let signed = Self::tls_deserialize_exact_bytes(bytes)?;
    signed.notice.check_values()?;
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
    notice.check_values()?;
    verify_signature(
      dave_public_key,
      CALL_RECORDING_NOTICE,
      &notice.tls_serialize_detached()?,
      &self.signature,
    )?;
    if notice.call_id != call_id {
      return Err(RecordingNoticeError::WrongCall);
    }
    if notice.device_id != device_id {
      return Err(RecordingNoticeError::WrongDevice);
    }
    Ok(notice)
  }
}

impl SignedRecordingConsent {
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, RecordingNoticeError> {
    let signed = Self::tls_deserialize_exact_bytes(bytes)?;
    signed.consent.check_values()?;
    Ok(signed)
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, RecordingNoticeError> {
    Ok(self.tls_serialize_detached()?)
  }

  /// The claimed, not yet verified content: only to find the answering device's key. Nothing is
  /// mixed in or shown on an unverified consent.
  pub fn unverified(&self) -> &RecordingConsent {
    &self.consent
  }

  /// Checks the consent against the answering device's DAVE leaf key (SEC1 P-256) in this call.
  /// The recorder and the take are the caller's to compare.
  pub fn verify(
    &self,
    dave_public_key: &[u8],
    call_id: CallId,
    device_id: DeviceId,
  ) -> Result<&RecordingConsent, RecordingNoticeError> {
    let consent = &self.consent;
    consent.check_values()?;
    verify_signature(
      dave_public_key,
      CALL_RECORDING_CONSENT,
      &consent.tls_serialize_detached()?,
      &self.signature,
    )?;
    if consent.call_id != call_id {
      return Err(RecordingNoticeError::WrongCall);
    }
    if consent.device_id != device_id {
      return Err(RecordingNoticeError::WrongDevice);
    }
    Ok(consent)
  }
}

fn verify_signature(
  dave_public_key: &[u8],
  label: &str,
  body: &[u8],
  signature: &[u8; SIGNATURE_BYTES],
) -> Result<(), RecordingNoticeError> {
  let key = VerifyingKey::from_sec1_bytes(dave_public_key).map_err(|_| RecordingNoticeError::InvalidKey)?;
  let signature = Signature::from_slice(signature).map_err(|_| RecordingNoticeError::InvalidSignature)?;
  key
    .verify(&labeled(label, body), &signature)
    .map_err(|_| RecordingNoticeError::InvalidSignature)
}

impl fmt::Debug for SignedRecordingNotice {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SignedRecordingNotice")
      .field("notice", &self.notice)
      .finish_non_exhaustive()
  }
}

impl fmt::Debug for SignedRecordingConsent {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SignedRecordingConsent")
      .field("consent", &self.consent)
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
  const TAKE: u64 = 1_700_000_000_000;

  mod notices {
    use super::*;

    #[test]
    fn a_notice_round_trips_and_verifies_with_the_dave_key() {
      let signer = signer();
      for (sequence, state, asks_consent) in [
        (1, RecordingState::Requesting, true),
        (2, RecordingState::Recording, true),
        (3, RecordingState::Recording, false),
        (4, RecordingState::Stopped, false),
      ] {
        let announcement = RecordingAnnouncement {
          take: TAKE,
          asks_consent,
          state,
        };
        let signed = signer.sign(CALL, LAPTOP, sequence, announcement).unwrap();
        let decoded = SignedRecordingNotice::from_bytes(&signed.to_bytes().unwrap()).unwrap();
        let notice = decoded.verify(&signer.public_key(), CALL, LAPTOP).unwrap();
        assert_eq!(notice.announcement(), announcement);
        assert_eq!(notice.sequence(), sequence);
        assert_eq!((notice.call_id(), notice.device_id()), (CALL, LAPTOP));
      }
    }

    #[test]
    fn a_notice_stays_small() {
      let bytes = signer()
        .sign(CALL, LAPTOP, u64::MAX, recording(true))
        .unwrap()
        .to_bytes()
        .unwrap();
      assert_eq!(bytes.len(), 99);
    }

    #[test]
    fn another_key_another_call_or_another_device_fails() {
      let signer = signer();
      let signed = signer.sign(CALL, LAPTOP, 1, recording(false)).unwrap();
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
    fn a_tampered_state_sequence_take_or_consent_flag_fails() {
      let signer = signer();
      let signed = signer.sign(CALL, LAPTOP, 7, recording(true)).unwrap();
      let tampered: [fn(&mut RecordingNotice); 4] = [
        |notice| notice.state = STATE_STOPPED,
        |notice| notice.sequence = 8,
        |notice| notice.take = TAKE + 1,
        // A recording that asked for consent can't be passed off as one that didn't.
        |notice| notice.asks_consent = 0,
      ];
      for tamper in tampered {
        let mut changed = signed.clone();
        tamper(&mut changed.notice);
        assert!(matches!(
          changed.verify(&signer.public_key(), CALL, LAPTOP),
          Err(RecordingNoticeError::InvalidSignature)
        ));
      }
    }

    #[test]
    fn malformed_bytes_and_keys_are_errors_not_panics() {
      let signer = signer();
      let bytes = signer
        .sign(CALL, LAPTOP, 1, recording(false))
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
    fn an_unknown_state_flag_or_version_is_refused() {
      let signer = signer();
      let mut signed = signer.sign(CALL, LAPTOP, 1, recording(false)).unwrap();
      signed.notice.state = 9;
      assert!(matches!(
        SignedRecordingNotice::from_bytes(&signed.to_bytes().unwrap()),
        Err(RecordingNoticeError::UnknownState(9))
      ));
      let mut flag = signer.sign(CALL, LAPTOP, 1, recording(false)).unwrap();
      flag.notice.asks_consent = 2;
      assert!(matches!(
        SignedRecordingNotice::from_bytes(&flag.to_bytes().unwrap()),
        Err(RecordingNoticeError::InvalidFlag)
      ));
      for version in [1, 3] {
        let mut other = signer.sign(CALL, LAPTOP, 1, recording(false)).unwrap();
        other.notice.version = version;
        assert!(matches!(
          other.verify(&signer.public_key(), CALL, LAPTOP),
          Err(RecordingNoticeError::UnsupportedVersion(found)) if found == version
        ));
      }
    }
  }

  /// Consents (#2115): bound to the call, the answering device, the recorder and the take.
  mod consents {
    use super::*;

    #[test]
    fn a_consent_round_trips_and_verifies_with_the_answering_devices_key() {
      let phone = signer();
      for (sequence, answer) in [(1, ConsentAnswer::Allow), (2, ConsentAnswer::Decline)] {
        let signed = phone
          .sign_consent(CALL, PHONE, LAPTOP, TAKE, sequence, answer)
          .unwrap();
        let decoded = SignedRecordingConsent::from_bytes(&signed.to_bytes().unwrap()).unwrap();
        let consent = decoded.verify(&phone.public_key(), CALL, PHONE).unwrap();
        assert_eq!(consent.answer(), answer);
        assert_eq!(consent.sequence(), sequence);
        assert_eq!((consent.recorder_device_id(), consent.take()), (LAPTOP, TAKE));
      }
    }

    #[test]
    fn the_recorder_cant_sign_a_consent_for_somebody_else() {
      let laptop = signer();
      let phone = signer();
      // The recorder signs "the phone allows" with its own key: the phone's key refuses it.
      let forged = laptop
        .sign_consent(CALL, PHONE, LAPTOP, TAKE, 1, ConsentAnswer::Allow)
        .unwrap();
      assert!(matches!(
        forged.verify(&phone.public_key(), CALL, PHONE),
        Err(RecordingNoticeError::InvalidSignature)
      ));
    }

    #[test]
    fn a_tampered_answer_recorder_take_or_sequence_fails() {
      let phone = signer();
      let signed = phone
        .sign_consent(CALL, PHONE, LAPTOP, TAKE, 5, ConsentAnswer::Decline)
        .unwrap();
      let tampered: [fn(&mut RecordingConsent); 4] = [
        |consent| consent.answer = ANSWER_ALLOW,
        |consent| consent.recorder_device_id = DeviceId::new(12),
        |consent| consent.take = TAKE + 1,
        |consent| consent.sequence = 6,
      ];
      for tamper in tampered {
        let mut changed = signed.clone();
        tamper(&mut changed.consent);
        assert!(matches!(
          changed.verify(&phone.public_key(), CALL, PHONE),
          Err(RecordingNoticeError::InvalidSignature)
        ));
      }
    }

    #[test]
    fn another_call_or_device_fails() {
      let phone = signer();
      let signed = phone
        .sign_consent(CALL, PHONE, LAPTOP, TAKE, 1, ConsentAnswer::Allow)
        .unwrap();
      assert!(matches!(
        signed.verify(&phone.public_key(), CallId::new(501), PHONE),
        Err(RecordingNoticeError::WrongCall)
      ));
      assert!(matches!(
        signed.verify(&phone.public_key(), CALL, LAPTOP),
        Err(RecordingNoticeError::WrongDevice)
      ));
    }

    #[test]
    fn a_notice_never_verifies_as_a_consent() {
      let laptop = signer();
      let notice = laptop.sign(CALL, LAPTOP, 1, recording(true)).unwrap();
      let bytes = notice.to_bytes().unwrap();
      // A different length: it doesn't even parse as a consent.
      assert!(SignedRecordingConsent::from_bytes(&bytes).is_err());
    }

    #[test]
    fn malformed_or_unknown_answers_are_errors_not_panics() {
      let phone = signer();
      let bytes = phone
        .sign_consent(CALL, PHONE, LAPTOP, TAKE, 1, ConsentAnswer::Allow)
        .unwrap()
        .to_bytes()
        .unwrap();
      assert_eq!(bytes.len(), 106);
      assert!(SignedRecordingConsent::from_bytes(&[]).is_err());
      assert!(SignedRecordingConsent::from_bytes(&bytes[..bytes.len() - 1]).is_err());
      let mut unknown = phone
        .sign_consent(CALL, PHONE, LAPTOP, TAKE, 1, ConsentAnswer::Allow)
        .unwrap();
      unknown.consent.answer = 7;
      assert!(matches!(
        SignedRecordingConsent::from_bytes(&unknown.to_bytes().unwrap()),
        Err(RecordingNoticeError::UnknownAnswer(7))
      ));
    }
  }

  fn recording(asks_consent: bool) -> RecordingAnnouncement {
    RecordingAnnouncement {
      take: TAKE,
      asks_consent,
      state: RecordingState::Recording,
    }
  }

  fn signer() -> RecordingNoticeSigner {
    let key = SigningKey::random(&mut OsRng);
    RecordingNoticeSigner::from_dave_private_key(&key.to_bytes()).unwrap()
  }
}
