//! What a signed recording notice (e2ee.md §8.3) claims, read without checking its signature, for
//! the audit log (issue #2117).
//!
//! A notice is signed, not encrypted: the SFU can read its call, device and state, but it doesn't
//! have the DAVE leaf keys to verify it. It doesn't need to for the audit: it takes a notice into
//! account only when it names this call and the very device that sent it (authenticated by its
//! join token), so a device can only ever claim something about itself, and what the SFU reports
//! is what every other app in the call is shown. A device that claims a recording it doesn't make
//! only puts an indicator on everybody's screen and a line in the log about itself; one that
//! records without a notice is the covert capture no notice can prevent.
//!
//! A notice that asks for consent and records nothing yet (`requesting`, #2115) counts as not
//! recording: the log has the recording from the moment it records.
//!
//! The layout is the TLS encoding of `teamcraft_crypto::call_recording::SignedRecordingNotice`,
//! version 2 (fixed size, no length prefixes); a test signs a notice with that type and reads it
//! back here, so the two can't drift apart silently. The same holds for the recorder a consent
//! names ([`consent_recorder`]), by which the SFU keeps one consent per device and recorder.

use teamcraft_proto::{CallId, DeviceId, Id};

/// The only notice encoding there is; another one is ignored (no report), not misread.
const NOTICE_VERSION: u8 = 2;
const STATE_STOPPED: u8 = 0;
const STATE_RECORDING: u8 = 1;
const STATE_REQUESTING: u8 = 2;
/// version (1) + call id (8) + device id (8) + sequence (8) + take (8) + asks consent (1) +
/// state (1) + signature (64).
const NOTICE_BYTES: usize = 99;
const CALL_ID_AT: usize = 1;
const DEVICE_ID_AT: usize = 9;
const STATE_AT: usize = 34;
/// The only consent encoding there is.
const CONSENT_VERSION: u8 = 1;
/// version (1) + call id (8) + device id (8) + recorder (8) + take (8) + sequence (8) + answer (1)
/// + signature (64).
const CONSENT_BYTES: usize = 106;
const CONSENT_RECORDER_AT: usize = 17;

/// What a notice says, unverified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordingClaim {
  pub call_id: u64,
  pub device_id: u64,
  pub is_recording: bool,
}

impl RecordingClaim {
  /// Reads a notice; `None` for anything that isn't a version 1 notice of the expected size and a
  /// known state.
  pub fn read(notice: &[u8]) -> Option<Self> {
    if notice.len() != NOTICE_BYTES || notice[0] != NOTICE_VERSION {
      return None;
    }
    let is_recording = match notice[STATE_AT] {
      STATE_RECORDING => true,
      STATE_STOPPED | STATE_REQUESTING => false,
      _ => return None,
    };
    Some(Self {
      call_id: read_u64(notice, CALL_ID_AT)?,
      device_id: read_u64(notice, DEVICE_ID_AT)?,
      is_recording,
    })
  }

  /// Whether the notice speaks for `device_id` in `call_id`: the only notices the audit counts.
  pub fn is_about(&self, call_id: CallId, device_id: DeviceId) -> bool {
    Some(self.call_id) == raw(call_id.snowflake().get())
      && Some(self.device_id) == raw(device_id.snowflake().get())
  }
}

/// The recorder a signed consent (#2115) answers, unverified: what the SFU keeps one consent per.
/// `None` for anything that isn't a version 1 consent of the expected size.
pub fn consent_recorder(consent: &[u8]) -> Option<u64> {
  if consent.len() != CONSENT_BYTES || consent[0] != CONSENT_VERSION {
    return None;
  }
  read_u64(consent, CONSENT_RECORDER_AT)
}

fn read_u64(bytes: &[u8], at: usize) -> Option<u64> {
  let field: [u8; 8] = bytes.get(at..at + 8)?.try_into().ok()?;
  Some(u64::from_be_bytes(field))
}

/// Snowflakes are never negative; one that is matches no notice.
fn raw(snowflake: i64) -> Option<u64> {
  u64::try_from(snowflake).ok()
}

#[cfg(test)]
mod tests {
  use teamcraft_crypto::call_binding::CallId as CryptoCallId;
  use teamcraft_crypto::call_recording::{
    ConsentAnswer, RecordingAnnouncement, RecordingNoticeSigner, RecordingState,
  };
  use teamcraft_crypto::ids::DeviceId as CryptoDeviceId;
  use teamcraft_proto::Snowflake;

  use super::*;

  fn signed(call: u64, device: u64, state: RecordingState) -> Vec<u8> {
    // Any scalar below the curve order is a valid P-256 key.
    let signer = RecordingNoticeSigner::from_dave_private_key(&[7; 32]).unwrap();
    signer
      .sign(
        CryptoCallId::new(call),
        CryptoDeviceId::new(device),
        1_700_000_000_000,
        RecordingAnnouncement {
          take: 1_700_000_000_000,
          asks_consent: true,
          state,
        },
      )
      .unwrap()
      .to_bytes()
      .unwrap()
  }

  fn call(raw: i64) -> CallId {
    CallId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  #[test]
  fn reads_what_the_crypto_crate_signs() {
    let recording = RecordingClaim::read(&signed(7, 42, RecordingState::Recording)).unwrap();
    assert_eq!(
      recording,
      RecordingClaim {
        call_id: 7,
        device_id: 42,
        is_recording: true,
      }
    );
    let stopped = RecordingClaim::read(&signed(7, 42, RecordingState::Stopped)).unwrap();
    assert!(!stopped.is_recording);
    let asking = RecordingClaim::read(&signed(7, 42, RecordingState::Requesting)).unwrap();
    assert!(!asking.is_recording, "asking for consent records nothing yet");
  }

  #[test]
  fn reads_the_recorder_of_what_the_crypto_crate_signs_as_a_consent() {
    let signer = RecordingNoticeSigner::from_dave_private_key(&[7; 32]).unwrap();
    let consent = signer
      .sign_consent(
        CryptoCallId::new(7),
        CryptoDeviceId::new(43),
        CryptoDeviceId::new(42),
        1,
        2,
        ConsentAnswer::Allow,
      )
      .unwrap()
      .to_bytes()
      .unwrap();
    assert_eq!(consent_recorder(&consent), Some(42));
    assert_eq!(consent_recorder(&consent[..105]), None, "cut short");
    assert_eq!(
      consent_recorder(&signed(7, 42, RecordingState::Recording)),
      None,
      "a notice"
    );
    let mut newer = consent;
    newer[0] = 2;
    assert_eq!(consent_recorder(&newer), None, "a newer version");
  }

  #[test]
  fn counts_only_notices_of_the_sender_in_this_call() {
    let claim = RecordingClaim::read(&signed(7, 42, RecordingState::Recording)).unwrap();
    assert!(claim.is_about(call(7), device(42)));
    assert!(!claim.is_about(call(8), device(42)), "another call");
    assert!(!claim.is_about(call(7), device(43)), "another device");
  }

  #[test]
  fn ignores_what_isnt_a_known_notice() {
    let notice = signed(7, 42, RecordingState::Recording);
    assert_eq!(RecordingClaim::read(&[]), None);
    assert_eq!(RecordingClaim::read(&notice[..98]), None, "cut short");
    assert_eq!(
      RecordingClaim::read(&[notice.as_slice(), &[0]].concat()),
      None,
      "too long"
    );
    let mut newer = notice.clone();
    newer[0] = 3;
    assert_eq!(RecordingClaim::read(&newer), None, "a newer version");
    let mut unknown_state = notice;
    unknown_state[STATE_AT] = 9;
    assert_eq!(RecordingClaim::read(&unknown_state), None, "an unknown state");
  }
}
