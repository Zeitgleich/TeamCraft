//! Consent to a call recording (issue #2115, e2ee.md §8.3).
//!
//! Where the signed policy says `all_consent`, a recording asks everybody in the call before it
//! records them. Every other device answers with a signed [`ConsentAnswer`] (allow or decline)
//! that names the call, the recorder and the take, signed with the answering device's DAVE key of
//! this call:
//!
//! - [`RecordingConsents`] keeps the answers the other devices gave, verified against their DAVE
//!   keys; one that can't be checked yet waits (at most [`MAX_PENDING_CONSENTS`] per device) and
//!   counts for nothing until it verifies. The recorder mixes in only the devices with a verified
//!   "allow" for its very take ([`RecordingConsents::allowed`]); every other app shows who is left
//!   out from the same answers. Neither the server nor the recorder can make up an answer: it
//!   verifies only with the key of the device that gave it.
//! - Only a higher-numbered answer replaces an earlier one, so a withdrawn consent can't be
//!   replayed back in by the server.
//! - [`OwnConsents`] numbers and signs this device's answers, sends them again until the SFU
//!   confirms it keeps them, and signs them anew after a rejoin or a new DAVE session (the SFU
//!   forgot them, and late joiners verify with the new key).
//!
//! Failing safe: an answer that is missing, unverified, for another take or lost on the way
//! leaves the device out of the recording, never in it.

use std::collections::VecDeque;
use std::time::Duration;

use teamcraft_crypto::call_recording::{
  ConsentAnswer, RecordingNoticeError, RecordingNoticeSigner, SignedRecordingConsent,
};
use teamcraft_proto::{CallId, DeviceId};
use tracing::warn;
use web_time::Instant;

use crate::call_auth::{crypto_call_id, crypto_device_id, proto_device_id};

/// Answers that wait for their sender's key, per device; beyond this the oldest goes.
pub const MAX_PENDING_CONSENTS: usize = 8;
/// The recorders whose answers are kept per device, at most (the SFU keeps as many).
pub const MAX_ANSWERS_PER_DEVICE: usize = teamcraft_proto::MAX_RECORDING_CONSENTS_PER_DEVICE;
/// How long an own answer may wait for the SFU's confirmation before it is sent again.
const RESEND_AFTER: Duration = Duration::from_secs(2);

/// The other devices' verified answers in one call. A list, not a map: a call has few devices.
#[derive(Debug, Default)]
pub struct RecordingConsents {
  devices: Vec<(DeviceId, DeviceConsents)>,
}

#[derive(Debug, Default)]
struct DeviceConsents {
  /// The key its last answer verified with.
  maybe_key: Option<Vec<u8>>,
  /// The highest verified answer per recorder.
  answers: Vec<VerifiedAnswer>,
  /// Answers that couldn't be checked yet, oldest first.
  pending: VecDeque<SignedRecordingConsent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VerifiedAnswer {
  recorder: DeviceId,
  take: u64,
  sequence: u64,
  answer: ConsentAnswer,
}

/// This device's answers to the others' recordings.
#[derive(Debug, Default)]
pub struct OwnConsents {
  answers: Vec<OwnAnswer>,
  /// The number of the last answer signed.
  last_sequence: u64,
}

#[derive(Debug)]
struct OwnAnswer {
  recorder: DeviceId,
  take: u64,
  answer: ConsentAnswer,
  /// The last signed copy sent that the SFU hasn't confirmed yet, and when it went out.
  maybe_unconfirmed: Option<(Vec<u8>, Instant)>,
}

impl RecordingConsents {
  /// Takes an answer the SFU relayed from `device_id`. `maybe_key` is that device's DAVE key in
  /// this call if known here. Returns whether a verified answer changed.
  pub fn receive(
    &mut self,
    call_id: CallId,
    device_id: DeviceId,
    bytes: &[u8],
    maybe_key: Option<&[u8]>,
  ) -> bool {
    let signed = match SignedRecordingConsent::from_bytes(bytes) {
      Ok(signed) => signed,
      Err(error) => {
        warn!(%error, %device_id, "malformed recording consent dropped");
        return false;
      },
    };
    if signed.unverified().device_id() != crypto_device_id(device_id) {
      warn!(%device_id, "recording consent of another device dropped");
      return false;
    }
    let entry = self.entry(device_id);
    match maybe_key.map(<[u8]>::to_vec).or_else(|| entry.maybe_key.clone()) {
      Some(key) => match entry.verify(&signed, &key, call_id, device_id) {
        Ok(has_changed) => has_changed,
        // A new DAVE session's answer fails the old key: it waits for the new one.
        Err(_) if maybe_key.is_none() => {
          entry.hold(signed);
          false
        },
        Err(error) => {
          log_refused(&error, device_id);
          false
        },
      },
      None => {
        entry.hold(signed);
        false
      },
    }
  }

  /// Checks the answers held for want of a key, now that `key_of` may know more keys. Returns
  /// whether a verified answer changed.
  pub fn recheck(&mut self, call_id: CallId, key_of: impl Fn(DeviceId) -> Option<Vec<u8>>) -> bool {
    let mut has_changed = false;
    for (device_id, entry) in &mut self.devices {
      if entry.pending.is_empty() {
        continue;
      }
      let Some(key) = key_of(*device_id) else {
        continue;
      };
      let pending: Vec<SignedRecordingConsent> = entry.pending.drain(..).collect();
      for signed in &pending {
        match entry.verify(signed, &key, call_id, *device_id) {
          Ok(changed) => has_changed |= changed,
          Err(error) => log_refused(&error, *device_id),
        }
      }
    }
    has_changed
  }

  /// The device left the call: its answers go (it says them again when it rejoins). Leaving
  /// only ever takes a voice out of a recording. Returns whether a verified answer went.
  pub fn forget(&mut self, device_id: DeviceId) -> bool {
    let Some(index) = self.devices.iter().position(|(device, _)| *device == device_id) else {
      return false;
    };
    let (_, entry) = self.devices.swap_remove(index);
    !entry.answers.is_empty()
  }

  /// `device_id`'s verified answer to `recorder`'s take `take`, if it gave one.
  pub fn answer(&self, device_id: DeviceId, recorder: DeviceId, take: u64) -> Option<ConsentAnswer> {
    let (_, entry) = self.devices.iter().find(|(device, _)| *device == device_id)?;
    entry
      .answers
      .iter()
      .find(|answer| answer.recorder == recorder && answer.take == take)
      .map(|answer| answer.answer)
  }

  /// The devices whose verified "allow" for `recorder`'s take `take` is here: who may be in it.
  pub fn allowed(&self, recorder: DeviceId, take: u64) -> Vec<DeviceId> {
    self
      .devices
      .iter()
      .filter(|(device_id, _)| self.answer(*device_id, recorder, take) == Some(ConsentAnswer::Allow))
      .map(|(device_id, _)| *device_id)
      .collect()
  }

  fn entry(&mut self, device_id: DeviceId) -> &mut DeviceConsents {
    let index = match self.devices.iter().position(|(device, _)| *device == device_id) {
      Some(index) => index,
      None => {
        self.devices.push((device_id, DeviceConsents::default()));
        self.devices.len() - 1
      },
    };
    &mut self.devices[index].1
  }
}

impl DeviceConsents {
  /// Verifies an answer and keeps it if it is the newest for its recorder; returns whether the
  /// kept answers changed.
  fn verify(
    &mut self,
    signed: &SignedRecordingConsent,
    key: &[u8],
    call_id: CallId,
    device_id: DeviceId,
  ) -> Result<bool, RecordingNoticeError> {
    let consent = signed.verify(key, crypto_call_id(call_id), crypto_device_id(device_id))?;
    self.maybe_key = Some(key.to_vec());
    let Ok(recorder) = proto_device_id(consent.recorder_device_id()) else {
      return Ok(false);
    };
    let verified = VerifiedAnswer {
      recorder,
      take: consent.take(),
      sequence: consent.sequence(),
      answer: consent.answer(),
    };
    match self.answers.iter_mut().find(|kept| kept.recorder == recorder) {
      Some(kept) if kept.sequence >= verified.sequence => Ok(false),
      Some(kept) => {
        let has_changed = (kept.take, kept.answer) != (verified.take, verified.answer);
        *kept = verified;
        Ok(has_changed)
      },
      None => {
        if self.answers.len() >= MAX_ANSWERS_PER_DEVICE {
          self.answers.remove(0);
        }
        self.answers.push(verified);
        Ok(true)
      },
    }
  }

  fn hold(&mut self, signed: SignedRecordingConsent) {
    if self.pending.contains(&signed) {
      return;
    }
    if self.pending.len() == MAX_PENDING_CONSENTS {
      self.pending.pop_front();
    }
    self.pending.push_back(signed);
  }
}

impl OwnConsents {
  /// This device's answer to `recorder`'s take `take`, if it gave one.
  pub fn answer(&self, recorder: DeviceId, take: u64) -> Option<ConsentAnswer> {
    self
      .answers
      .iter()
      .find(|own| own.recorder == recorder && own.take == take)
      .map(|own| own.answer)
  }

  /// Answers `recorder`'s take `take` (a new answer replaces the last one to that recorder), and
  /// returns the signed answer to send. `now_ms` starts the numbering where an app that restarted
  /// mid-call left off, as for notices.
  #[allow(clippy::too_many_arguments)]
  pub fn give(
    &mut self,
    signer: &RecordingNoticeSigner,
    call_id: CallId,
    device_id: DeviceId,
    recorder: DeviceId,
    take: u64,
    answer: ConsentAnswer,
    now_ms: u64,
    now: Instant,
  ) -> Result<Vec<u8>, RecordingNoticeError> {
    self.answers.retain(|own| own.recorder != recorder);
    if self.answers.len() >= MAX_ANSWERS_PER_DEVICE {
      self.answers.remove(0);
    }
    self.answers.push(OwnAnswer {
      recorder,
      take,
      answer,
      maybe_unconfirmed: None,
    });
    let index = self.answers.len() - 1;
    self.sign(index, signer, call_id, device_id, now_ms, now)
  }

  /// Signs every answer anew (a rejoin, a new DAVE session: the SFU forgot them, and the others
  /// verify with the new key). Returns the signed answers to send.
  pub fn resign_all(
    &mut self,
    signer: &RecordingNoticeSigner,
    call_id: CallId,
    device_id: DeviceId,
    now_ms: u64,
    now: Instant,
  ) -> Vec<Vec<u8>> {
    (0..self.answers.len())
      .filter_map(
        |index| match self.sign(index, signer, call_id, device_id, now_ms, now) {
          Ok(bytes) => Some(bytes),
          Err(error) => {
            warn!(%error, "signing a recording consent failed");
            None
          },
        },
      )
      .collect()
  }

  /// The recorder stopped or left: its answers are no longer needed.
  pub fn forget(&mut self, recorder: DeviceId) {
    self.answers.retain(|own| own.recorder != recorder);
  }

  /// The SFU confirmed it keeps `consent`. Returns whether that was one waiting.
  pub fn confirm(&mut self, consent: &[u8]) -> bool {
    let mut was_waiting = false;
    for own in &mut self.answers {
      if own
        .maybe_unconfirmed
        .as_ref()
        .is_some_and(|(bytes, _)| bytes.as_slice() == consent)
      {
        own.maybe_unconfirmed = None;
        was_waiting = true;
      }
    }
    was_waiting
  }

  /// The answers to send again now, as the SFU hasn't confirmed them for a while.
  pub fn due_resends(&mut self, now: Instant) -> Vec<Vec<u8>> {
    self
      .answers
      .iter_mut()
      .filter_map(|own| {
        let (bytes, sent) = own.maybe_unconfirmed.as_mut()?;
        if now.saturating_duration_since(*sent) < RESEND_AFTER {
          return None;
        }
        *sent = now;
        Some(bytes.clone())
      })
      .collect()
  }

  /// A new connection: what the old one confirmed counts for nothing there.
  pub fn forget_confirmations(&mut self) {
    for own in &mut self.answers {
      own.maybe_unconfirmed = None;
    }
  }

  fn sign(
    &mut self,
    index: usize,
    signer: &RecordingNoticeSigner,
    call_id: CallId,
    device_id: DeviceId,
    now_ms: u64,
    now: Instant,
  ) -> Result<Vec<u8>, RecordingNoticeError> {
    let sequence = now_ms.max(self.last_sequence.saturating_add(1));
    let own = &mut self.answers[index];
    let bytes = signer
      .sign_consent(
        crypto_call_id(call_id),
        crypto_device_id(device_id),
        crypto_device_id(own.recorder),
        own.take,
        sequence,
        own.answer,
      )?
      .to_bytes()?;
    own.maybe_unconfirmed = Some((bytes.clone(), now));
    self.last_sequence = sequence;
    Ok(bytes)
  }
}

/// An answer that failed its check: only the device id is logged.
fn log_refused(error: &RecordingNoticeError, device_id: DeviceId) {
  warn!(%error, %device_id, "recording consent failed verification, dropped");
}

#[cfg(test)]
mod tests {
  use davey::SigningKeyPair;
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  const CALL: i64 = 500;
  const LAPTOP: i64 = 10;
  const PHONE: i64 = 11;
  const TABLET: i64 = 12;
  const TAKE: u64 = 1_000;

  mod recorder_side {
    use super::*;

    #[test]
    fn only_a_verified_allow_for_this_take_lets_a_device_in() {
      let (phone, phone_key) = signer();
      let mut own = OwnConsents::default();
      let mut consents = RecordingConsents::default();
      let allow = give(&mut own, &phone, PHONE, TAKE, ConsentAnswer::Allow, 1);
      assert!(consents.receive(call(), id(PHONE), &allow, Some(&phone_key)));
      assert_eq!(consents.allowed(id(LAPTOP), TAKE), vec![id(PHONE)]);
      assert!(consents.allowed(id(LAPTOP), TAKE + 1).is_empty(), "another take");
      assert!(consents.allowed(id(TABLET), TAKE).is_empty(), "another recorder");
    }

    #[test]
    fn a_decline_or_a_withdrawal_leaves_a_device_out() {
      let (phone, phone_key) = signer();
      let mut own = OwnConsents::default();
      let mut consents = RecordingConsents::default();
      let allow = give(&mut own, &phone, PHONE, TAKE, ConsentAnswer::Allow, 1);
      consents.receive(call(), id(PHONE), &allow, Some(&phone_key));
      let decline = give(&mut own, &phone, PHONE, TAKE, ConsentAnswer::Decline, 2);
      assert!(consents.receive(call(), id(PHONE), &decline, Some(&phone_key)));
      assert!(consents.allowed(id(LAPTOP), TAKE).is_empty());
      assert_eq!(
        consents.answer(id(PHONE), id(LAPTOP), TAKE),
        Some(ConsentAnswer::Decline)
      );
    }

    #[test]
    fn an_unchecked_answer_counts_for_nothing_until_it_verifies() {
      let (phone, phone_key) = signer();
      let mut own = OwnConsents::default();
      let mut consents = RecordingConsents::default();
      let allow = give(&mut own, &phone, PHONE, TAKE, ConsentAnswer::Allow, 1);
      assert!(!consents.receive(call(), id(PHONE), &allow, None));
      assert!(consents.allowed(id(LAPTOP), TAKE).is_empty());
      assert!(consents.recheck(call(), |_| Some(phone_key.clone())));
      assert_eq!(consents.allowed(id(LAPTOP), TAKE), vec![id(PHONE)]);
    }

    #[test]
    fn a_device_that_leaves_is_left_out_until_it_answers_again() {
      let (phone, phone_key) = signer();
      let mut own = OwnConsents::default();
      let mut consents = RecordingConsents::default();
      let allow = give(&mut own, &phone, PHONE, TAKE, ConsentAnswer::Allow, 1);
      consents.receive(call(), id(PHONE), &allow, Some(&phone_key));
      assert!(consents.forget(id(PHONE)));
      assert!(consents.allowed(id(LAPTOP), TAKE).is_empty());
    }
  }

  /// What a hostile server or recorder could try: forge, replay, misattribute or flood answers.
  mod hostile {
    use super::*;

    #[test]
    fn a_consent_signed_by_somebody_else_is_dropped() {
      let (_, phone_key) = signer();
      let (laptop, _) = signer();
      let mut own = OwnConsents::default();
      let mut consents = RecordingConsents::default();
      // The recorder signs "the phone allows" with its own key.
      let forged = give(&mut own, &laptop, PHONE, TAKE, ConsentAnswer::Allow, 1);
      assert!(!consents.receive(call(), id(PHONE), &forged, Some(&phone_key)));
      assert!(consents.allowed(id(LAPTOP), TAKE).is_empty());
    }

    #[test]
    fn a_replayed_older_allow_doesnt_undo_a_withdrawal() {
      let (phone, phone_key) = signer();
      let mut own = OwnConsents::default();
      let mut consents = RecordingConsents::default();
      let allow = give(&mut own, &phone, PHONE, TAKE, ConsentAnswer::Allow, 1);
      let decline = give(&mut own, &phone, PHONE, TAKE, ConsentAnswer::Decline, 2);
      consents.receive(call(), id(PHONE), &decline, Some(&phone_key));
      assert!(!consents.receive(call(), id(PHONE), &allow, Some(&phone_key)));
      assert!(consents.allowed(id(LAPTOP), TAKE).is_empty());
    }

    #[test]
    fn an_answer_relayed_as_another_devices_another_calls_or_garbled_is_dropped() {
      let (phone, phone_key) = signer();
      let mut own = OwnConsents::default();
      let mut consents = RecordingConsents::default();
      let allow = give(&mut own, &phone, PHONE, TAKE, ConsentAnswer::Allow, 1);
      assert!(!consents.receive(call(), id(TABLET), &allow, Some(&phone_key)));
      assert!(!consents.receive(id(501), id(PHONE), &allow, Some(&phone_key)));
      assert!(!consents.receive(call(), id(PHONE), &allow[..50], Some(&phone_key)));
      assert!(!consents.receive(call(), id(PHONE), &[], None));
      assert!(consents.allowed(id(LAPTOP), TAKE).is_empty());
    }

    #[test]
    fn waiting_answers_stay_bounded() {
      let (forger, _) = signer();
      let mut own = OwnConsents::default();
      let mut consents = RecordingConsents::default();
      for sequence in 1..100 {
        let bytes = give(&mut own, &forger, PHONE, TAKE, ConsentAnswer::Allow, sequence);
        consents.receive(call(), id(PHONE), &bytes, None);
      }
      assert_eq!(consents.devices[0].1.pending.len(), MAX_PENDING_CONSENTS);
    }
  }

  mod own {
    use super::*;

    #[test]
    fn an_unconfirmed_answer_is_sent_again_until_confirmed() {
      let (phone, _) = signer();
      let start = Instant::now();
      let mut own = OwnConsents::default();
      let sent = own
        .give(
          &phone,
          call(),
          id(PHONE),
          id(LAPTOP),
          TAKE,
          ConsentAnswer::Allow,
          1,
          start,
        )
        .unwrap();
      assert!(own.due_resends(start + Duration::from_secs(1)).is_empty());
      assert_eq!(own.due_resends(start + RESEND_AFTER), vec![sent.clone()]);
      assert!(own.confirm(&sent));
      assert!(own.due_resends(start + Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn a_rejoin_signs_every_answer_anew_with_a_higher_number() {
      let (old, _) = signer();
      let (new, new_key) = signer();
      let mut own = OwnConsents::default();
      own
        .give(
          &old,
          call(),
          id(PHONE),
          id(LAPTOP),
          TAKE,
          ConsentAnswer::Allow,
          5,
          Instant::now(),
        )
        .unwrap();
      let again = own.resign_all(&new, call(), id(PHONE), 1, Instant::now());
      assert_eq!(again.len(), 1);
      let mut consents = RecordingConsents::default();
      assert!(consents.receive(call(), id(PHONE), &again[0], Some(&new_key)));
      assert_eq!(own.answer(id(LAPTOP), TAKE), Some(ConsentAnswer::Allow));
      own.forget(id(LAPTOP));
      assert_eq!(own.answer(id(LAPTOP), TAKE), None);
    }
  }

  fn give(
    own: &mut OwnConsents,
    signer: &RecordingNoticeSigner,
    device: i64,
    take: u64,
    answer: ConsentAnswer,
    sequence: u64,
  ) -> Vec<u8> {
    own
      .give(
        signer,
        call(),
        id(device),
        id(LAPTOP),
        take,
        answer,
        sequence,
        Instant::now(),
      )
      .unwrap()
  }

  fn signer() -> (RecordingNoticeSigner, Vec<u8>) {
    let signer = RecordingNoticeSigner::from_dave_private_key(&SigningKeyPair::generate().private).unwrap();
    let public = signer.public_key();
    (signer, public)
  }

  fn call() -> CallId {
    id(CALL)
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
