//! Who records the call, from the signed recording notices (e2ee.md §8.3, issue #216).
//!
//! [`RecordingNotices`] keeps what the other devices said and decides what the app shows:
//!
//! - A notice is verified against the sender's DAVE key of this call (the key its call key binding
//!   vouches for). One that fails is dropped.
//! - A "recording" notice that can't be checked yet (the sender's key isn't known here yet) shows
//!   at once, as unverified: the app errs on the side of telling people. Up to
//!   [`MAX_PENDING_NOTICES`] such notices wait per device, so a forged one can't push the real one
//!   out.
//! - Only a verified "stopped" ends a recording on screen, and only a notice numbered higher than
//!   the last verified one of its device counts, so a replayed old notice changes nothing.
//! - The SFU saying that a recording device left isn't enough to hide its recording: the SFU
//!   alone says so. The recording shows as "left the call" until the device is out of the call's
//!   DAVE group, which the members themselves commit ([`RecordingNotices::prune_left`]).
//!
//! [`OwnRecording`] numbers and signs this device's own notices and waits for the SFU to confirm
//! it keeps each one: a notice it doesn't confirm is sent again, and the app warns that the others
//! may not see the recording.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use teamcraft_crypto::call_recording::{
  RecordingNoticeError, RecordingNoticeSigner, RecordingState, SignedRecordingNotice,
};
use teamcraft_proto::{CallId, DeviceId, UserId};
use tracing::warn;
use web_time::Instant;

use crate::call_auth::{crypto_call_id, crypto_device_id};

/// Notices that wait for their sender's key, per device; beyond this the oldest goes.
pub const MAX_PENDING_NOTICES: usize = 8;
/// How long the own notice may wait for the SFU's confirmation before it is sent again.
const RESEND_AFTER: Duration = Duration::from_secs(2);
/// Without a confirmation for this long, the app warns that the others may not see the recording.
const UNCONFIRMED_AFTER: Duration = Duration::from_secs(6);

/// A device that records, as this app shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Recorder {
  pub device_id: DeviceId,
  /// Whose device it is, as the call's roster said while the device was in it.
  pub maybe_user_id: Option<UserId>,
  /// Its notice verified against its DAVE key; `false` while the key isn't known here yet.
  pub is_verified: bool,
  /// The SFU says it left the call, but it is still in the call's DAVE group: it may still hear
  /// the call.
  pub has_left: bool,
}

/// The other devices' recording notices in one call.
#[derive(Debug, Default)]
pub struct RecordingNotices {
  devices: BTreeMap<DeviceId, DeviceNotices>,
}

#[derive(Debug, Default)]
struct DeviceNotices {
  maybe_user_id: Option<UserId>,
  /// The key its last notice verified with: a stop it sends after the SFU called it gone still
  /// verifies.
  maybe_key: Option<Vec<u8>>,
  /// The highest verified notice: its number and state.
  maybe_verified: Option<(u64, RecordingState)>,
  /// Notices that couldn't be checked yet, newer than the verified one, oldest first.
  pending: VecDeque<SignedRecordingNotice>,
  has_left: bool,
}

/// This device's recording, as its notices say it.
#[derive(Debug, Default)]
pub struct OwnRecording {
  is_recording: bool,
  /// The state of the last notice sent, if any.
  maybe_announced: Option<RecordingState>,
  /// The number of the last notice signed.
  last_sequence: u64,
  /// The last notice sent that the SFU hasn't confirmed yet.
  maybe_unconfirmed: Option<Unconfirmed>,
}

#[derive(Debug)]
struct Unconfirmed {
  notice: Vec<u8>,
  first_sent: Instant,
  last_sent: Instant,
}

impl RecordingNotices {
  /// Takes a notice the SFU relayed from `device_id`. `maybe_key` is that device's DAVE key in
  /// this call if known here, `maybe_user_id` whose device it is. Returns whether the recorders
  /// changed.
  pub fn receive(
    &mut self,
    call_id: CallId,
    device_id: DeviceId,
    maybe_user_id: Option<UserId>,
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
    if maybe_user_id.is_some() {
      entry.maybe_user_id = maybe_user_id;
    }
    match (maybe_key, entry.maybe_key.clone()) {
      (Some(key), _) => {
        if let Err(error) = entry.verify(&signed, key, call_id, device_id) {
          log_refused(&error, device_id);
        }
      },
      // The key its last notice verified with (the engine no longer lists it, the SFU called the
      // device gone): a notice that fails it may come from a new session, so it waits.
      (None, Some(known)) => {
        if entry.verify(&signed, &known, call_id, device_id).is_err() {
          entry.hold(signed);
        }
      },
      (None, None) => entry.hold(signed),
    }
    self.recorders() != before
  }

  /// Checks the notices held for want of a key, now that `key_of` may know more keys (a binding
  /// verified, the DAVE group changed). Returns whether the recorders changed.
  pub fn recheck(&mut self, call_id: CallId, key_of: impl Fn(DeviceId) -> Option<Vec<u8>>) -> bool {
    let before = self.recorders();
    for (device_id, entry) in &mut self.devices {
      if entry.pending.is_empty() {
        continue;
      }
      let Some(key) = key_of(*device_id).or_else(|| entry.maybe_key.clone()) else {
        continue;
      };
      let mut pending: Vec<SignedRecordingNotice> = entry.pending.drain(..).collect();
      pending.sort_by_key(|signed| signed.unverified().sequence());
      for signed in &pending {
        if let Err(error) = entry.verify(signed, &key, call_id, *device_id) {
          log_refused(&error, *device_id);
        }
      }
    }
    self.recorders() != before
  }

  /// The SFU says the device left the call. A recording that only claims to run goes with it; a
  /// verified one stays, marked, until the device is out of the DAVE group. Returns whether the
  /// recorders changed.
  pub fn mark_left(&mut self, device_id: DeviceId) -> bool {
    let before = self.recorders();
    match self.devices.get_mut(&device_id) {
      Some(entry) if entry.is_verified_recording() => entry.has_left = true,
      Some(_) => {
        self.devices.remove(&device_id);
      },
      None => {},
    }
    self.recorders() != before
  }

  /// The device is in the call (again).
  pub fn mark_present(&mut self, device_id: DeviceId, user_id: UserId) -> bool {
    let before = self.recorders();
    if let Some(entry) = self.devices.get_mut(&device_id) {
      entry.has_left = false;
      entry.maybe_user_id = Some(user_id);
    }
    self.recorders() != before
  }

  /// Forgets the recordings of devices that left and are out of the DAVE group now
  /// (`is_member` says who is in it). Returns whether the recorders changed.
  pub fn prune_left(&mut self, is_member: impl Fn(DeviceId) -> bool) -> bool {
    let before = self.recorders();
    self
      .devices
      .retain(|device_id, entry| !entry.has_left || is_member(*device_id));
    self.recorders() != before
  }

  /// The devices that record now, by device id.
  pub fn recorders(&self) -> Vec<Recorder> {
    self
      .devices
      .iter()
      .filter(|(_, entry)| entry.is_recording())
      .map(|(device_id, entry)| Recorder {
        device_id: *device_id,
        maybe_user_id: entry.maybe_user_id,
        is_verified: entry.is_verified_recording(),
        has_left: entry.has_left,
      })
      .collect()
  }
}

impl DeviceNotices {
  fn verify(
    &mut self,
    signed: &SignedRecordingNotice,
    key: &[u8],
    call_id: CallId,
    device_id: DeviceId,
  ) -> Result<(), RecordingNoticeError> {
    let notice = signed.verify(key, crypto_call_id(call_id), crypto_device_id(device_id))?;
    self.maybe_key = Some(key.to_vec());
    let sequence = notice.sequence();
    if self
      .maybe_verified
      .is_none_or(|(verified, _)| sequence > verified)
    {
      self.maybe_verified = Some((sequence, notice.state()));
    }
    self
      .pending
      .retain(|pending| pending.unverified().sequence() > sequence);
    Ok(())
  }

  /// Keeps a notice that can't be checked yet if it is newer than the verified one; the oldest
  /// waiting one goes when too many wait.
  fn hold(&mut self, signed: SignedRecordingNotice) {
    let sequence = signed.unverified().sequence();
    if self
      .maybe_verified
      .is_some_and(|(verified, _)| sequence <= verified)
    {
      return;
    }
    if self.pending.contains(&signed) {
      return;
    }
    if self.pending.len() == MAX_PENDING_NOTICES {
      self.pending.pop_front();
    }
    self.pending.push_back(signed);
  }

  fn is_verified_recording(&self) -> bool {
    matches!(self.maybe_verified, Some((_, RecordingState::Recording)))
  }

  /// A verified recording, or an unchecked claim of one: an unchecked stop changes nothing.
  fn is_recording(&self) -> bool {
    self.is_verified_recording()
      || self
        .pending
        .iter()
        .any(|pending| pending.unverified().state() == RecordingState::Recording)
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

  /// Signs the next notice of the current state and waits for the SFU to confirm it. `now_ms`
  /// (any wall clock in milliseconds) starts the numbering where an app that restarted mid-call
  /// left off, give or take its clock.
  pub fn next_notice(
    &mut self,
    signer: &RecordingNoticeSigner,
    call_id: CallId,
    device_id: DeviceId,
    now_ms: u64,
    now: Instant,
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
    // A stop that nobody confirms leaves the others with a recording on screen: harmless, and
    // the next join says it again. Only a recording waits for its confirmation.
    let first_sent = self
      .maybe_unconfirmed
      .as_ref()
      .map_or(now, |unconfirmed| unconfirmed.first_sent);
    self.maybe_unconfirmed = (state == RecordingState::Recording).then(|| Unconfirmed {
      notice: bytes.clone(),
      first_sent,
      last_sent: now,
    });
    Ok(bytes)
  }

  /// The SFU confirmed it keeps `notice`. Returns whether that was the one waiting.
  pub fn confirm(&mut self, notice: &[u8]) -> bool {
    let is_waiting = self
      .maybe_unconfirmed
      .as_ref()
      .is_some_and(|unconfirmed| unconfirmed.notice == notice);
    if is_waiting {
      self.maybe_unconfirmed = None;
    }
    is_waiting
  }

  /// The notice to send again now, if the SFU hasn't confirmed it for a while.
  pub fn due_resend(&mut self, now: Instant) -> Option<Vec<u8>> {
    let unconfirmed = self.maybe_unconfirmed.as_mut()?;
    if now.saturating_duration_since(unconfirmed.last_sent) < RESEND_AFTER {
      return None;
    }
    unconfirmed.last_sent = now;
    Some(unconfirmed.notice.clone())
  }

  /// Whether the recording runs without the SFU confirming its notice for a while: the others
  /// may not see it, and the recorder is told so.
  pub fn is_unconfirmed(&self, now: Instant) -> bool {
    self.is_recording
      && self
        .maybe_unconfirmed
        .as_ref()
        .is_some_and(|unconfirmed| now.saturating_duration_since(unconfirmed.first_sent) >= UNCONFIRMED_AFTER)
  }

  /// A new connection: what the old one confirmed counts for nothing there.
  pub fn forget_confirmation(&mut self) {
    self.maybe_unconfirmed = None;
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
  const ALICE: i64 = 1;

  mod receiving {
    use super::*;

    #[test]
    fn a_verified_recording_shows_and_a_verified_stop_ends_it() {
      let (signer, key) = signer();
      let mut notices = RecordingNotices::default();
      assert!(receive(&mut notices, &notice(&signer, true, 1), Some(&key)));
      assert_eq!(notices.recorders(), vec![recorder(true, false)]);
      assert!(receive(&mut notices, &notice(&signer, false, 2), Some(&key)));
      assert!(notices.recorders().is_empty());
    }

    #[test]
    fn an_unchecked_recording_shows_at_once_and_is_confirmed_later() {
      let (signer, key) = signer();
      let mut notices = RecordingNotices::default();
      assert!(receive(&mut notices, &notice(&signer, true, 5), None));
      assert_eq!(notices.recorders(), vec![recorder(false, false)]);
      assert!(notices.recheck(call(), |_| Some(key.clone())));
      assert_eq!(notices.recorders(), vec![recorder(true, false)]);
    }

    #[test]
    fn an_unchecked_stop_ends_nothing() {
      let (signer, key) = signer();
      let mut notices = RecordingNotices::default();
      receive(&mut notices, &notice(&signer, true, 1), Some(&key));
      // A new session of the recorder: its key isn't known here yet.
      let (new_signer, new_key) = self::signer();
      assert!(!receive(&mut notices, &notice(&new_signer, false, 2), None));
      assert_eq!(notices.recorders(), vec![recorder(true, false)]);
      // Once the key is there, the stop verifies and ends it.
      assert!(notices.recheck(call(), |_| Some(new_key.clone())));
      assert!(notices.recorders().is_empty());
    }

    #[test]
    fn a_replayed_older_notice_changes_nothing() {
      let (signer, key) = signer();
      let mut notices = RecordingNotices::default();
      let old_stop = notice(&signer, false, 1);
      receive(&mut notices, &notice(&signer, true, 2), Some(&key));
      assert!(!receive(&mut notices, &old_stop, Some(&key)));
      assert_eq!(notices.recorders(), vec![recorder(true, false)]);
    }

    #[test]
    fn a_stop_after_the_key_was_dropped_still_verifies() {
      let (signer, key) = signer();
      let mut notices = RecordingNotices::default();
      receive(&mut notices, &notice(&signer, true, 1), Some(&key));
      // The engine no longer lists the key (the SFU called the device gone).
      assert!(receive(&mut notices, &notice(&signer, false, 2), None));
      assert!(notices.recorders().is_empty());
    }
  }

  /// Leaving is the SFU's word: it alone can't hide a recording (e2ee.md §8.3).
  mod leaving {
    use super::*;

    #[test]
    fn a_verified_recording_stays_marked_until_the_device_is_out_of_the_group() {
      let (signer, key) = signer();
      let mut notices = RecordingNotices::default();
      receive(&mut notices, &notice(&signer, true, 1), Some(&key));
      assert!(notices.mark_left(device()));
      assert_eq!(notices.recorders(), vec![recorder(true, true)]);
      // Still a member of the DAVE group: it may still hear the call.
      assert!(!notices.prune_left(|_| true));
      assert_eq!(notices.recorders(), vec![recorder(true, true)]);
      assert!(notices.prune_left(|_| false));
      assert!(notices.recorders().is_empty());
    }

    #[test]
    fn a_device_that_comes_back_is_no_longer_marked() {
      let (signer, key) = signer();
      let mut notices = RecordingNotices::default();
      receive(&mut notices, &notice(&signer, true, 1), Some(&key));
      notices.mark_left(device());
      assert!(notices.mark_present(device(), id(ALICE)));
      assert_eq!(notices.recorders(), vec![recorder(true, false)]);
      // Members that never left aren't pruned.
      assert!(!notices.prune_left(|_| false));
    }

    #[test]
    fn an_unchecked_claim_goes_with_the_device() {
      let (signer, _) = signer();
      let mut notices = RecordingNotices::default();
      receive(&mut notices, &notice(&signer, true, 1), None);
      assert!(notices.mark_left(device()));
      assert!(notices.recorders().is_empty());
    }
  }

  /// What a hostile server could try: forge, misattribute, flood or garble notices.
  mod hostile_server {
    use super::*;

    #[test]
    fn a_forged_stop_is_dropped_once_checked() {
      let (signer, key) = signer();
      let (forger, _) = self::signer();
      let mut notices = RecordingNotices::default();
      receive(&mut notices, &notice(&signer, true, 1), Some(&key));
      assert!(!receive(&mut notices, &notice(&forger, false, 2), Some(&key)));
      assert_eq!(notices.recorders(), vec![recorder(true, false)]);
    }

    #[test]
    fn a_forged_recording_shows_only_until_it_can_be_checked() {
      let (_, key) = signer();
      let (forger, _) = signer();
      let mut notices = RecordingNotices::default();
      receive(&mut notices, &notice(&forger, true, 1), None);
      assert_eq!(
        notices.recorders(),
        vec![recorder(false, false)],
        "fail-safe: shown"
      );
      assert!(notices.recheck(call(), |_| Some(key.clone())));
      assert!(notices.recorders().is_empty(), "the check drops it");
    }

    #[test]
    fn a_forged_notice_with_the_highest_number_doesnt_push_out_the_real_one() {
      let (signer, key) = signer();
      let (forger, _) = self::signer();
      let mut notices = RecordingNotices::default();
      receive(&mut notices, &notice(&signer, true, 5), None);
      receive(&mut notices, &notice(&forger, false, u64::MAX), None);
      assert!(notices.recheck(call(), |_| Some(key.clone())));
      assert_eq!(notices.recorders(), vec![recorder(true, false)]);
    }

    #[test]
    fn waiting_notices_stay_bounded() {
      let (forger, _) = signer();
      let mut notices = RecordingNotices::default();
      for sequence in 1..100 {
        receive(&mut notices, &notice(&forger, true, sequence), None);
      }
      assert_eq!(notices.devices[&device()].pending.len(), MAX_PENDING_NOTICES);
    }

    #[test]
    fn a_notice_relayed_as_another_devices_or_garbled_is_dropped() {
      let (signer, key) = signer();
      let mut notices = RecordingNotices::default();
      let start = notice(&signer, true, 1);
      let other: DeviceId = id(11);
      assert!(!notices.receive(call(), other, None, &start, Some(&key)));
      assert!(!receive(&mut notices, &start[..start.len() - 3], Some(&key)));
      assert!(!receive(&mut notices, &[], None));
      assert!(notices.recorders().is_empty());
    }

    #[test]
    fn a_notice_of_another_call_is_dropped() {
      let (signer, key) = signer();
      let mut own = OwnRecording::default();
      own.set(true);
      let elsewhere = own
        .next_notice(&signer, id(501), device(), 1, Instant::now())
        .unwrap();
      let mut notices = RecordingNotices::default();
      assert!(!receive(&mut notices, &elsewhere, Some(&key)));
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
      let start = own
        .next_notice(&signer, call(), device(), 1_000, Instant::now())
        .unwrap();
      receive(&mut notices, &start, Some(&key));
      own.set(false);
      // The clock jumped back an hour: the stop still counts.
      let stop = own
        .next_notice(&signer, call(), device(), 10, Instant::now())
        .unwrap();
      receive(&mut notices, &stop, Some(&key));
      assert!(notices.recorders().is_empty());
    }

    #[test]
    fn a_rejoin_announces_a_recording_and_an_unsaid_stop() {
      let (signer, _) = signer();
      let mut own = OwnRecording::default();
      assert!(!own.needs_announcing(), "never recorded: nothing to say");
      own.set(true);
      assert!(own.needs_announcing());
      own
        .next_notice(&signer, call(), device(), 1, Instant::now())
        .unwrap();
      assert!(own.needs_announcing(), "re-signed with the new session's key");
      own.set(false);
      assert!(own.needs_announcing(), "the stop wasn't sent while away");
      own
        .next_notice(&signer, call(), device(), 2, Instant::now())
        .unwrap();
      assert!(!own.needs_announcing());
    }

    #[test]
    fn an_unconfirmed_notice_is_sent_again_and_then_warned_about() {
      let (signer, _) = signer();
      let start = Instant::now();
      let mut own = OwnRecording::default();
      own.set(true);
      let sent = own.next_notice(&signer, call(), device(), 1, start).unwrap();
      assert_eq!(own.due_resend(start + Duration::from_secs(1)), None);
      assert_eq!(own.due_resend(start + RESEND_AFTER), Some(sent.clone()));
      assert!(!own.is_unconfirmed(start + RESEND_AFTER));
      assert!(own.is_unconfirmed(start + UNCONFIRMED_AFTER));
      // The SFU confirms: no more resends, no warning.
      assert!(!own.confirm(b"another notice"));
      assert!(own.confirm(&sent));
      assert_eq!(own.due_resend(start + Duration::from_secs(60)), None);
      assert!(!own.is_unconfirmed(start + Duration::from_secs(60)));
    }

    #[test]
    fn a_stop_waits_for_nothing() {
      let (signer, _) = signer();
      let start = Instant::now();
      let mut own = OwnRecording::default();
      own.set(true);
      own.next_notice(&signer, call(), device(), 1, start).unwrap();
      own.set(false);
      own.next_notice(&signer, call(), device(), 2, start).unwrap();
      assert_eq!(own.due_resend(start + Duration::from_secs(60)), None);
      assert!(!own.is_unconfirmed(start + Duration::from_secs(60)));
    }
  }

  fn receive(notices: &mut RecordingNotices, bytes: &[u8], maybe_key: Option<&Vec<u8>>) -> bool {
    notices.receive(
      call(),
      device(),
      Some(id(ALICE)),
      bytes,
      maybe_key.map(Vec::as_slice),
    )
  }

  fn notice(signer: &RecordingNoticeSigner, is_recording: bool, sequence: u64) -> Vec<u8> {
    let mut own = OwnRecording {
      last_sequence: sequence.saturating_sub(1),
      ..OwnRecording::default()
    };
    own.set(is_recording);
    own
      .next_notice(signer, call(), device(), sequence, Instant::now())
      .unwrap()
  }

  fn signer() -> (RecordingNoticeSigner, Vec<u8>) {
    let signer = RecordingNoticeSigner::from_dave_private_key(&SigningKeyPair::generate().private).unwrap();
    let public = signer.public_key();
    (signer, public)
  }

  fn recorder(is_verified: bool, has_left: bool) -> Recorder {
    Recorder {
      device_id: device(),
      maybe_user_id: Some(id(ALICE)),
      is_verified,
      has_left,
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
