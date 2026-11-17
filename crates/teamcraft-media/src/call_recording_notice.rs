//! Who records the call, from the signed recording notices (e2ee.md §8.3, issue #216).
//!
//! [`RecordingNotices`] keeps what the other devices said and decides what the app shows:
//!
//! - A notice is verified against the sender's DAVE key of this call (the key its call key binding
//!   vouches for). One that fails is dropped.
//! - A "recording" notice that can't be checked yet (the sender's key isn't known here yet) shows
//!   at once, as unverified: the app errs on the side of telling people.
//! - Only a verified "stopped" ends a recording on screen, and only a notice numbered higher than
//!   the last verified one of its device counts, so a replayed old notice changes nothing.
//! - A device that leaves the call takes its recording with it.
//!
//! [`OwnRecording`] numbers and signs this device's own notices.

use std::collections::BTreeMap;

use teamcraft_crypto::call_recording::{
  RecordingNoticeError, RecordingNoticeSigner, RecordingState, SignedRecordingNotice,
};
use teamcraft_proto::{CallId, DeviceId};
use tracing::warn;

use crate::call_auth::{crypto_call_id, crypto_device_id};

/// A device that records, as this app shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Recorder {
  pub device_id: DeviceId,
  /// Its notice verified against its DAVE key; `false` while the key isn't known here yet.
  pub is_verified: bool,
}

/// The other devices' recording notices in one call.
#[derive(Debug, Default)]
pub struct RecordingNotices {
  devices: BTreeMap<DeviceId, DeviceNotices>,
}

#[derive(Debug, Default)]
struct DeviceNotices {
  /// The highest verified notice: its number and state.
  maybe_verified: Option<(u64, RecordingState)>,
  /// The latest notice that couldn't be checked yet, newer than the verified one.
  maybe_pending: Option<SignedRecordingNotice>,
}

/// This device's recording, as its notices say it.
#[derive(Debug, Default)]
pub struct OwnRecording {
  is_recording: bool,
  /// The state of the last notice sent, if any.
  maybe_announced: Option<RecordingState>,
  /// The number of the last notice signed.
  last_sequence: u64,
}

impl RecordingNotices {
  /// Takes a notice the SFU relayed from `device_id`. `maybe_key` is that device's DAVE key in
  /// this call, if known here. Returns whether the recorders changed.
  pub fn receive(
    &mut self,
    call_id: CallId,
    device_id: DeviceId,
    bytes: &[u8],
    maybe_key: Option<&[u8]>,
  ) -> bool {
    let before = self.recorders();
    let signed = match SignedRecordingNotice::from_bytes(bytes) {
      Ok(signed) => signed,
      Err(error) => {
        warn!(%error, %device_id, "malformed recording notice dropped");
        return false;
      },
    };
    if signed.unverified().device_id() != crypto_device_id(device_id) {
      warn!(%device_id, "recording notice of another device dropped");
      return false;
    }
    let entry = self.devices.entry(device_id).or_default();
    match maybe_key {
      Some(key) => entry.verify(&signed, key, call_id, device_id),
      None => entry.hold(signed),
    }
    self.recorders() != before
  }

  /// Checks the notices held for want of a key, now that `key_of` may know more keys (the DAVE
  /// group changed). Returns whether the recorders changed.
  pub fn recheck(&mut self, call_id: CallId, key_of: impl Fn(DeviceId) -> Option<Vec<u8>>) -> bool {
    let before = self.recorders();
    for (device_id, entry) in &mut self.devices {
      if entry.maybe_pending.is_none() {
        continue;
      }
      if let Some(key) = key_of(*device_id)
        && let Some(signed) = entry.maybe_pending.take()
      {
        entry.verify(&signed, &key, call_id, *device_id);
      }
    }
    self.recorders() != before
  }

  /// The device left the call. Returns whether it was recording.
  pub fn remove(&mut self, device_id: DeviceId) -> bool {
    let was_recording = self
      .devices
      .get(&device_id)
      .is_some_and(DeviceNotices::is_recording);
    self.devices.remove(&device_id);
    was_recording
  }

  /// The devices that record now, by device id.
  pub fn recorders(&self) -> Vec<Recorder> {
    self
      .devices
      .iter()
      .filter(|(_, entry)| entry.is_recording())
      .map(|(device_id, entry)| Recorder {
        device_id: *device_id,
        is_verified: entry.is_verified_recording(),
      })
      .collect()
  }
}

impl DeviceNotices {
  fn verify(&mut self, signed: &SignedRecordingNotice, key: &[u8], call_id: CallId, device_id: DeviceId) {
    match signed.verify(key, crypto_call_id(call_id), crypto_device_id(device_id)) {
      Ok(notice) => {
        let is_newer = self
          .maybe_verified
          .is_none_or(|(sequence, _)| notice.sequence() > sequence);
        if is_newer {
          self.maybe_verified = Some((notice.sequence(), notice.state()));
        }
        if self
          .maybe_pending
          .as_ref()
          .is_some_and(|pending| pending.unverified().sequence() <= notice.sequence())
        {
          self.maybe_pending = None;
        }
      },
      Err(error) => log_refused(&error, device_id),
    }
  }

  /// Keeps a notice that can't be checked yet if it is newer than what is known.
  fn hold(&mut self, signed: SignedRecordingNotice) {
    let sequence = signed.unverified().sequence();
    let is_newer_than_verified = self
      .maybe_verified
      .is_none_or(|(verified, _)| sequence > verified);
    let is_newer_than_pending = self
      .maybe_pending
      .as_ref()
      .is_none_or(|pending| sequence > pending.unverified().sequence());
    if is_newer_than_verified && is_newer_than_pending {
      self.maybe_pending = Some(signed);
    }
  }

  fn is_verified_recording(&self) -> bool {
    matches!(self.maybe_verified, Some((_, RecordingState::Recording)))
  }

  /// A verified recording, or an unchecked claim of one: an unchecked stop changes nothing.
  fn is_recording(&self) -> bool {
    self.is_verified_recording()
      || self
        .maybe_pending
        .as_ref()
        .is_some_and(|pending| pending.unverified().state() == RecordingState::Recording)
  }
}

impl OwnRecording {
  pub fn is_recording(&self) -> bool {
    self.is_recording
  }

  /// Starts or stops; returns whether that changed anything.
  pub fn set(&mut self, is_recording: bool) -> bool {
    let has_changed = self.is_recording != is_recording;
    self.is_recording = is_recording;
    has_changed
  }

  /// Whether a (re)join must say the state: while recording (the others need a notice signed
  /// with this session's key), or when the last notice sent isn't the state any more.
  pub fn needs_announcing(&self) -> bool {
    self.is_recording || self.maybe_announced.is_some_and(|state| state != self.state())
  }

  /// Signs the next notice of the current state. `now_ms` (any wall clock in milliseconds) starts
  /// the numbering where an app that restarted mid-call left off, give or take its clock.
  pub fn next_notice(
    &mut self,
    signer: &RecordingNoticeSigner,
    call_id: CallId,
    device_id: DeviceId,
    now_ms: u64,
  ) -> Result<Vec<u8>, RecordingNoticeError> {
    let sequence = now_ms.max(self.last_sequence.saturating_add(1));
    let state = self.state();
    let bytes = signer
      .sign(
        crypto_call_id(call_id),
        crypto_device_id(device_id),
        sequence,
        state,
      )?
      .to_bytes()?;
    self.last_sequence = sequence;
    self.maybe_announced = Some(state);
    Ok(bytes)
  }

  fn state(&self) -> RecordingState {
    if self.is_recording {
      RecordingState::Recording
    } else {
      RecordingState::Stopped
    }
  }
}

/// A notice that failed its check: only the device id is logged.
fn log_refused(error: &RecordingNoticeError, device_id: DeviceId) {
  warn!(%error, %device_id, "recording notice failed verification, dropped");
}

#[cfg(test)]
mod tests {
  use davey::SigningKeyPair;
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  const CALL: i64 = 500;
  const LAPTOP: i64 = 10;

  mod receiving {
    use super::*;

    #[test]
    fn a_verified_recording_shows_and_a_verified_stop_ends_it() {
      let (signer, key) = signer();
      let mut notices = RecordingNotices::default();
      let mut own = OwnRecording::default();
      own.set(true);
      let start = own.next_notice(&signer, call(), device(), 1).unwrap();
      assert!(notices.receive(call(), device(), &start, Some(&key)));
      assert_eq!(notices.recorders(), vec![recorder(true)]);
      own.set(false);
      let stop = own.next_notice(&signer, call(), device(), 2).unwrap();
      assert!(notices.receive(call(), device(), &stop, Some(&key)));
      assert!(notices.recorders().is_empty());
    }

    #[test]
    fn an_unchecked_recording_shows_at_once_and_is_confirmed_later() {
      let (signer, key) = signer();
      let mut notices = RecordingNotices::default();
      let start = notice(&signer, true, 5);
      assert!(notices.receive(call(), device(), &start, None));
      assert_eq!(notices.recorders(), vec![recorder(false)]);
      assert!(notices.recheck(call(), |_| Some(key.clone())));
      assert_eq!(notices.recorders(), vec![recorder(true)]);
    }

    #[test]
    fn an_unchecked_stop_ends_nothing() {
      let (signer, key) = signer();
      let mut notices = RecordingNotices::default();
      notices.receive(call(), device(), &notice(&signer, true, 1), Some(&key));
      assert!(!notices.receive(call(), device(), &notice(&signer, false, 2), None));
      assert_eq!(notices.recorders(), vec![recorder(true)]);
      // Once the key is there, the stop verifies and ends it.
      assert!(notices.recheck(call(), |_| Some(key.clone())));
      assert!(notices.recorders().is_empty());
    }

    #[test]
    fn a_replayed_older_notice_changes_nothing() {
      let (signer, key) = signer();
      let mut notices = RecordingNotices::default();
      let old_stop = notice(&signer, false, 1);
      notices.receive(call(), device(), &notice(&signer, true, 2), Some(&key));
      assert!(!notices.receive(call(), device(), &old_stop, Some(&key)));
      assert_eq!(notices.recorders(), vec![recorder(true)]);
    }

    #[test]
    fn a_leaving_device_takes_its_recording_with_it() {
      let (signer, key) = signer();
      let mut notices = RecordingNotices::default();
      notices.receive(call(), device(), &notice(&signer, true, 1), Some(&key));
      assert!(notices.remove(device()));
      assert!(notices.recorders().is_empty());
      assert!(!notices.remove(device()));
    }
  }

  /// What a hostile server could try: forge, misattribute, or garble notices.
  mod hostile_server {
    use super::*;

    #[test]
    fn a_forged_stop_is_dropped_once_checked() {
      let (signer, key) = signer();
      let (forger, _) = self::signer();
      let mut notices = RecordingNotices::default();
      notices.receive(call(), device(), &notice(&signer, true, 1), Some(&key));
      assert!(!notices.receive(call(), device(), &notice(&forger, false, 2), Some(&key)));
      assert_eq!(notices.recorders(), vec![recorder(true)]);
    }

    #[test]
    fn a_forged_recording_shows_only_until_it_can_be_checked() {
      let (_, key) = signer();
      let (forger, _) = signer();
      let mut notices = RecordingNotices::default();
      notices.receive(call(), device(), &notice(&forger, true, 1), None);
      assert_eq!(notices.recorders(), vec![recorder(false)], "fail-safe: shown");
      assert!(notices.recheck(call(), |_| Some(key.clone())));
      assert!(notices.recorders().is_empty(), "the check drops it");
    }

    #[test]
    fn a_notice_relayed_as_another_devices_or_garbled_is_dropped() {
      let (signer, key) = signer();
      let mut notices = RecordingNotices::default();
      let start = notice(&signer, true, 1);
      let other: DeviceId = id(11);
      assert!(!notices.receive(call(), other, &start, Some(&key)));
      assert!(!notices.receive(call(), device(), &start[..start.len() - 3], Some(&key)));
      assert!(!notices.receive(call(), device(), &[], None));
      assert!(notices.recorders().is_empty());
    }

    #[test]
    fn a_notice_of_another_call_is_dropped() {
      let (signer, key) = signer();
      let mut own = OwnRecording::default();
      own.set(true);
      let elsewhere = own.next_notice(&signer, id(501), device(), 1).unwrap();
      let mut notices = RecordingNotices::default();
      assert!(!notices.receive(call(), device(), &elsewhere, Some(&key)));
      assert!(notices.recorders().is_empty());
    }
  }

  mod own {
    use super::*;

    #[test]
    fn numbers_grow_even_when_the_clock_goes_back() {
      let (signer, key) = signer();
      let mut own = OwnRecording::default();
      let mut notices = RecordingNotices::default();
      own.set(true);
      notices.receive(
        call(),
        device(),
        &own.next_notice(&signer, call(), device(), 1_000).unwrap(),
        Some(&key),
      );
      own.set(false);
      // The clock jumped back an hour: the stop still counts.
      let stop = own.next_notice(&signer, call(), device(), 10).unwrap();
      notices.receive(call(), device(), &stop, Some(&key));
      assert!(notices.recorders().is_empty());
    }

    #[test]
    fn a_rejoin_announces_a_recording_and_an_unsaid_stop() {
      let (signer, _) = signer();
      let mut own = OwnRecording::default();
      assert!(!own.needs_announcing(), "never recorded: nothing to say");
      own.set(true);
      assert!(own.needs_announcing());
      own.next_notice(&signer, call(), device(), 1).unwrap();
      assert!(own.needs_announcing(), "re-signed with the new session's key");
      own.set(false);
      assert!(own.needs_announcing(), "the stop wasn't sent while away");
      own.next_notice(&signer, call(), device(), 2).unwrap();
      assert!(!own.needs_announcing());
    }
  }

  fn notice(signer: &RecordingNoticeSigner, is_recording: bool, now_ms: u64) -> Vec<u8> {
    let mut own = OwnRecording {
      last_sequence: now_ms.saturating_sub(1),
      ..OwnRecording::default()
    };
    own.set(is_recording);
    own.next_notice(signer, call(), device(), now_ms).unwrap()
  }

  fn signer() -> (RecordingNoticeSigner, Vec<u8>) {
    let signer = RecordingNoticeSigner::from_dave_private_key(&SigningKeyPair::generate().private).unwrap();
    let public = signer.public_key();
    (signer, public)
  }

  fn recorder(is_verified: bool) -> Recorder {
    Recorder {
      device_id: device(),
      is_verified,
    }
  }

  fn call() -> CallId {
    id(CALL)
  }

  fn device() -> DeviceId {
    id(LAPTOP)
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
