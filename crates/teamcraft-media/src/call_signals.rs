//! In-call reactions and raised hands (issue #2324, e2ee.md §8.5): sealing this device's signals
//! and opening the others', for the call engine.
//!
//! Signals are ephemeral: one that can't be sent or opened now is dropped, never queued for later.
//! The one exception is a signal sealed for the epoch this device is about to take (the sender
//! switched a moment earlier): it waits a few moments in a small buffer, until the epoch's key is
//! here.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use rand_core::OsRng;
use teamcraft_crypto::call_binding::CallId as BindingCallId;
use teamcraft_crypto::call_signal::{CallSignalKey, SignalContext, sealed_epoch};
use teamcraft_crypto::ids::DeviceId as BindingDeviceId;
use teamcraft_proto::{
  Base64Bytes, CallId, CallReaction, DeviceId, Id, InCallSignal, MAX_SEALED_SIGNAL_BYTES, SealedSignalBody,
};
use tracing::debug;
// `std::time::Instant` on native; the browser clock on wasm.
use web_time::Instant;

/// Signals sealed for an epoch this device doesn't hold yet, kept until it does.
const MAX_PENDING_SIGNALS: usize = 32;
/// How many reactions this device sends within [`REACTION_WINDOW`]; more are dropped here, before
/// they use up the SFU's limit, which a raised hand needs too.
const MAX_REACTIONS_PER_WINDOW: usize = 5;
const REACTION_WINDOW: Duration = Duration::from_secs(2);

/// This device's signals and what it took from the others'.
#[derive(Debug, Default)]
pub struct CallSignals {
  /// The counter of this device's next signal. It only counts up, across epochs.
  next_seq: u64,
  is_hand_raised: bool,
  /// When this device's recent reactions went out, oldest first.
  recent_reactions: VecDeque<Instant>,
  /// Per sender, the epoch and counter of the last signal taken: older ones are replays.
  last_taken: HashMap<DeviceId, (u64, u64)>,
  /// Signals sealed for a newer epoch than this device holds.
  pending: VecDeque<(DeviceId, Base64Bytes)>,
}

/// Another device's signal, opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceivedSignal {
  Reaction {
    device_id: DeviceId,
    reaction: CallReaction,
  },
  Hand {
    device_id: DeviceId,
    is_raised: bool,
  },
}

impl CallSignals {
  pub fn is_hand_raised(&self) -> bool {
    self.is_hand_raised
  }

  /// Seals a reaction of this device, unless it sent too many just now.
  pub fn seal_reaction(
    &mut self,
    key: &CallSignalKey,
    call_id: CallId,
    own: DeviceId,
    reaction: CallReaction,
    now: Instant,
  ) -> Option<Base64Bytes> {
    while self
      .recent_reactions
      .front()
      .is_some_and(|sent| now.saturating_duration_since(*sent) >= REACTION_WINDOW)
    {
      self.recent_reactions.pop_front();
    }
    if self.recent_reactions.len() >= MAX_REACTIONS_PER_WINDOW {
      debug!("reaction dropped: too many just now");
      return None;
    }
    let sealed = self.seal(key, call_id, own, InCallSignal::Reaction { reaction })?;
    self.recent_reactions.push_back(now);
    Some(sealed)
  }

  /// Notes whether this device's hand is raised; `true` if it changed.
  pub fn set_hand_raised(&mut self, is_raised: bool) -> bool {
    let has_changed = self.is_hand_raised != is_raised;
    self.is_hand_raised = is_raised;
    has_changed
  }

  /// Seals the state of this device's hand.
  pub fn seal_hand(&mut self, key: &CallSignalKey, call_id: CallId, own: DeviceId) -> Option<Base64Bytes> {
    let is_raised = self.is_hand_raised;
    self.seal(key, call_id, own, InCallSignal::Hand { is_raised })
  }

  /// Opens another device's signal with the key of the epoch this device holds (`None`: none yet).
  /// One sealed for a later epoch waits in the buffer for [`CallSignals::take_pending`].
  pub fn receive(
    &mut self,
    maybe_key: Option<&CallSignalKey>,
    call_id: CallId,
    from: DeviceId,
    sealed: Base64Bytes,
  ) -> Option<ReceivedSignal> {
    let Ok(epoch) = sealed_epoch(sealed.as_slice()) else {
      debug!(device_id = %from, "sealed signal unreadable");
      return None;
    };
    let Some(key) = maybe_key.filter(|key| key.epoch() >= epoch) else {
      self.hold(from, sealed);
      return None;
    };
    let opened = match key.open(&context(call_id, from), sealed.as_slice()) {
      Ok(opened) => opened,
      Err(error) => {
        debug!(device_id = %from, %error, "sealed signal dropped");
        return None;
      },
    };
    let body: SealedSignalBody = match serde_json::from_slice(opened.expose_secret()) {
      Ok(body) => body,
      Err(error) => {
        debug!(device_id = %from, %error, "sealed signal malformed");
        return None;
      },
    };
    if self
      .last_taken
      .get(&from)
      .is_some_and(|&(taken_epoch, taken_seq)| taken_epoch == epoch && taken_seq >= body.seq)
    {
      debug!(device_id = %from, "sealed signal replayed");
      return None;
    }
    self.last_taken.insert(from, (epoch, body.seq));
    match body.signal {
      InCallSignal::Reaction { reaction } if reaction.emoji().is_some() => Some(ReceivedSignal::Reaction {
        device_id: from,
        reaction,
      }),
      InCallSignal::Hand { is_raised } => Some(ReceivedSignal::Hand {
        device_id: from,
        is_raised,
      }),
      // A reaction or signal this version doesn't know.
      InCallSignal::Reaction { .. } | InCallSignal::Unknown => None,
    }
  }

  /// The signals that waited for a later epoch, to open again now that this device holds a new
  /// one. Those still ahead go back into the buffer when received again.
  pub fn take_pending(&mut self) -> Vec<(DeviceId, Base64Bytes)> {
    self.pending.drain(..).collect()
  }

  /// A device left the call: nothing of it is kept.
  pub fn forget(&mut self, device_id: DeviceId) {
    self.last_taken.remove(&device_id);
    self.pending.retain(|(from, _)| *from != device_id);
  }

  fn hold(&mut self, from: DeviceId, sealed: Base64Bytes) {
    if self.pending.len() >= MAX_PENDING_SIGNALS {
      self.pending.pop_front();
    }
    self.pending.push_back((from, sealed));
  }

  fn seal(
    &mut self,
    key: &CallSignalKey,
    call_id: CallId,
    own: DeviceId,
    signal: InCallSignal,
  ) -> Option<Base64Bytes> {
    self.next_seq += 1;
    let body = SealedSignalBody {
      seq: self.next_seq,
      signal,
    };
    // Invariant: the body is a plain struct of numbers and fixed names; it always serializes.
    let plaintext = serde_json::to_vec(&body).ok()?;
    match key.seal(&context(call_id, own), &plaintext, &mut OsRng) {
      Ok(sealed) if sealed.len() <= MAX_SEALED_SIGNAL_BYTES => Some(Base64Bytes::new(sealed)),
      Ok(_) => None,
      Err(error) => {
        debug!(%error, "sealing a signal failed");
        None
      },
    }
  }
}

fn context(call_id: CallId, from: DeviceId) -> SignalContext {
  SignalContext {
    call_id: BindingCallId::new(raw(call_id)),
    from: BindingDeviceId::new(raw(from)),
  }
}

fn raw(id: impl Id) -> u64 {
  // Invariant: snowflakes are non-negative, so the conversion never fails.
  u64::try_from(id.snowflake().get()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::Snowflake;

  use super::*;

  const EXPORTED: [u8; 32] = [5; 32];

  #[test]
  fn a_reaction_and_a_hand_reach_the_other_device() {
    let (mut alice, mut bob) = (CallSignals::default(), CallSignals::default());
    let key = key(2);
    let sealed = alice
      .seal_reaction(&key, call(), device(1), CallReaction::Heart, Instant::now())
      .unwrap();
    assert_eq!(
      bob.receive(Some(&key), call(), device(1), sealed),
      Some(ReceivedSignal::Reaction {
        device_id: device(1),
        reaction: CallReaction::Heart,
      })
    );
    assert!(alice.set_hand_raised(true));
    assert!(!alice.set_hand_raised(true));
    let sealed = alice.seal_hand(&key, call(), device(1)).unwrap();
    assert_eq!(
      bob.receive(Some(&key), call(), device(1), sealed),
      Some(ReceivedSignal::Hand {
        device_id: device(1),
        is_raised: true,
      })
    );
  }

  #[test]
  fn a_signal_for_a_later_epoch_waits_for_its_key() {
    let (mut alice, mut bob) = (CallSignals::default(), CallSignals::default());
    alice.set_hand_raised(true);
    let sealed = alice.seal_hand(&key(3), call(), device(1)).unwrap();
    assert_eq!(
      bob.receive(Some(&key(2)), call(), device(1), sealed.clone()),
      None
    );
    assert_eq!(bob.receive(None, call(), device(1), sealed), None);
    let pending = bob.take_pending();
    assert_eq!(pending.len(), 2);
    let (from, sealed) = pending.into_iter().next().unwrap();
    assert!(bob.receive(Some(&key(3)), call(), from, sealed).is_some());
  }

  #[test]
  fn the_pending_buffer_is_bounded_and_forgets_who_left() {
    let mut bob = CallSignals::default();
    let mut alice = CallSignals::default();
    for _ in 0..MAX_PENDING_SIGNALS + 5 {
      let sealed = alice.seal_hand(&key(9), call(), device(1)).unwrap();
      bob.receive(None, call(), device(1), sealed);
    }
    assert_eq!(bob.pending.len(), MAX_PENDING_SIGNALS);
    bob.forget(device(1));
    assert!(bob.take_pending().is_empty());
  }

  mod rejection {
    use super::*;

    #[test]
    fn a_replayed_or_older_signal_is_dropped() {
      let (mut alice, mut bob) = (CallSignals::default(), CallSignals::default());
      let key = key(2);
      alice.set_hand_raised(true);
      let raised = alice.seal_hand(&key, call(), device(1)).unwrap();
      alice.set_hand_raised(false);
      let lowered = alice.seal_hand(&key, call(), device(1)).unwrap();
      assert!(
        bob
          .receive(Some(&key), call(), device(1), lowered.clone())
          .is_some()
      );
      assert_eq!(bob.receive(Some(&key), call(), device(1), raised), None);
      assert_eq!(bob.receive(Some(&key), call(), device(1), lowered), None);
    }

    #[test]
    fn a_signal_passed_off_as_another_devices_or_of_an_older_epoch_is_dropped() {
      let (mut alice, mut bob) = (CallSignals::default(), CallSignals::default());
      let sealed = alice
        .seal_reaction(&key(2), call(), device(1), CallReaction::Laugh, Instant::now())
        .unwrap();
      assert_eq!(
        bob.receive(Some(&key(2)), call(), device(7), sealed.clone()),
        None
      );
      assert_eq!(bob.receive(Some(&key(3)), call(), device(1), sealed), None);
      assert!(bob.take_pending().is_empty(), "an older epoch never comes back");
    }

    #[test]
    fn garbage_and_unknown_reactions_are_dropped() {
      let mut bob = CallSignals::default();
      assert_eq!(
        bob.receive(Some(&key(2)), call(), device(1), Base64Bytes::new(vec![1; 3])),
        None
      );
      let mut alice = CallSignals::default();
      let unknown = alice
        .seal(
          &key(2),
          call(),
          device(1),
          InCallSignal::Reaction {
            reaction: CallReaction::Unknown("rocket".into()),
          },
        )
        .unwrap();
      assert_eq!(bob.receive(Some(&key(2)), call(), device(1), unknown), None);
    }

    #[test]
    fn a_held_reaction_button_is_paced_here() {
      let mut alice = CallSignals::default();
      let now = Instant::now();
      let sent = (0..20)
        .filter(|_| {
          alice
            .seal_reaction(&key(2), call(), device(1), CallReaction::ThumbsUp, now)
            .is_some()
        })
        .count();
      assert_eq!(sent, MAX_REACTIONS_PER_WINDOW);
      assert!(
        alice
          .seal_reaction(
            &key(2),
            call(),
            device(1),
            CallReaction::ThumbsUp,
            now + REACTION_WINDOW
          )
          .is_some()
      );
      // The hand isn't paced with the reactions.
      assert!(alice.seal_hand(&key(2), call(), device(1)).is_some());
    }
  }

  fn key(epoch: u64) -> CallSignalKey {
    CallSignalKey::from_exporter(epoch, &EXPORTED).unwrap()
  }

  fn call() -> CallId {
    CallId::from_snowflake(Snowflake::new(40).unwrap())
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
