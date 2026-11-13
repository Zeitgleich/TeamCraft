//! Whether a link loses voice (#1871): the sender remembers which datagrams carried audio and reads
//! their fate from the receiver's transport feedback.
//!
//! When a link collapses (5 Mbit/s to 120 kbit/s from one second to the next), its bottleneck holds
//! the video sent for the old rate, and voice packets are lost with it or wait behind it until the
//! bandwidth estimate has come down and the video stopped: two to four seconds in which a call was
//! silent. The estimate is careful by design (a keyframe's burst, a jittery link and a still
//! screen's bursts mustn't cut it), and it speaks for all traffic. Hurt voice is the one signal
//! that needs no judgement: once [`VoiceGuardConfig::max_loss`] or more of the voice reported
//! within the last [`VoiceGuardConfig::window`] was lost or came
//! [`VoiceGuardConfig::max_delay`] later than the fastest lately (it plays as a gap just the same:
//! a sender's queue holds voice back behind a collapsed congestion window, not only the link), the
//! video yields at once ([`VoiceGuard::is_hurt`]) and stays off for [`VoiceGuardConfig::hold`]
//! after the last such moment.
//!
//! Pure state: time is passed in.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use teamcraft_proto::TransportFeedback;
// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::datagram::TransportSeq;

/// Voice datagrams remembered until the feedback reports them: about four seconds of one voice.
/// Older ones are forgotten (the feedback that would have reported them was lost).
const MAX_PENDING: usize = 256;
/// The fastest voice datagram's delay within this long is the path's own: what comes later waited.
const BASELINE_WINDOW: Duration = Duration::from_secs(10);

/// When voice counts as hurt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceGuardConfig {
  /// The voice datagrams reported within this long count.
  pub window: Duration,
  /// At least this many of them have to be reported: one lost packet of a quiet moment isn't a
  /// collapsed link.
  pub min_reported: usize,
  /// The share of them lost or late from which voice is hurt.
  pub max_loss: f64,
  /// A voice datagram that came this much later than the fastest lately is late: a jitter buffer
  /// doesn't wait that long.
  pub max_delay: Duration,
  /// Video stays off this long after voice was last hurt: a link that collapsed doesn't come back
  /// within a second, and switching video on and off sends a keyframe each time.
  pub hold: Duration,
}

/// The voice datagrams sent on one link and what became of them.
#[derive(Debug)]
pub struct VoiceGuard {
  config: VoiceGuardConfig,
  /// Voice datagrams without a report yet, by transport sequence number, oldest first.
  pending: VecDeque<TransportSeq>,
  pending_since: HashMap<u16, Instant>,
  /// Reported voice datagrams: when, and whether lost or late.
  reports: VecDeque<(Instant, bool)>,
  /// One-way delays of reported voice (receiver's clock minus the sender's, in microseconds: only
  /// differences count), for the baseline.
  delays: VecDeque<(Instant, i64)>,
  /// The sender's clock base for those delays.
  maybe_epoch: Option<Instant>,
  maybe_hurt_at: Option<Instant>,
}

impl Default for VoiceGuardConfig {
  fn default() -> Self {
    Self {
      window: Duration::from_millis(500),
      min_reported: 8,
      max_loss: 0.1,
      max_delay: Duration::from_millis(150),
      hold: Duration::from_secs(3),
    }
  }
}

impl VoiceGuard {
  pub fn new(config: VoiceGuardConfig) -> Self {
    Self {
      config,
      pending: VecDeque::new(),
      pending_since: HashMap::new(),
      reports: VecDeque::new(),
      delays: VecDeque::new(),
      maybe_epoch: None,
      maybe_hurt_at: None,
    }
  }

  /// A datagram carrying voice went out with transport sequence number `seq`.
  pub fn on_send(&mut self, seq: TransportSeq, now: Instant) {
    self.maybe_epoch.get_or_insert(now);
    self.pending.push_back(seq);
    self.pending_since.insert(seq.0, now);
    while self.pending.len() > MAX_PENDING {
      if let Some(old) = self.pending.pop_front() {
        self.pending_since.remove(&old.0);
      }
    }
  }

  /// The receiver's transport feedback (every datagram, voice or not, arrived or lost).
  pub fn on_feedback(&mut self, feedback: &TransportFeedback, now: Instant) {
    for (offset, maybe_arrival) in feedback.arrivals_us.iter().enumerate() {
      // A feedback reports far fewer than a u16 range of numbers; one that claims more is cut.
      let Ok(offset) = u16::try_from(offset) else {
        break;
      };
      let seq = feedback.base_seq.wrapping_add(offset);
      if let Some(sent_at) = self.pending_since.remove(&seq) {
        let is_hurt = match maybe_arrival {
          Some(arrival_us) => self.is_late(sent_at, *arrival_us, now),
          None => true,
        };
        self.reports.push_back((now, is_hurt));
      }
    }
    if !self.pending.is_empty() {
      let pending_since = &self.pending_since;
      self.pending.retain(|seq| pending_since.contains_key(&seq.0));
    }
    let window = self.config.window;
    while self
      .reports
      .front()
      .is_some_and(|(at, _)| now.saturating_duration_since(*at) > window)
    {
      self.reports.pop_front();
    }
    let reported = self.reports.len();
    let hurt = self.reports.iter().filter(|(_, is_hurt)| *is_hurt).count();
    if reported >= self.config.min_reported && hurt as f64 >= self.config.max_loss * reported as f64 {
      self.maybe_hurt_at = Some(now);
    }
  }

  /// Whether a voice datagram sent at `sent_at` that arrived at `arrival_us` (the receiver's
  /// clock) came [`VoiceGuardConfig::max_delay`] later than the fastest within
  /// [`BASELINE_WINDOW`].
  fn is_late(&mut self, sent_at: Instant, arrival_us: u64, now: Instant) -> bool {
    let epoch = *self.maybe_epoch.get_or_insert(sent_at);
    let sent_us = i64::try_from(sent_at.saturating_duration_since(epoch).as_micros()).unwrap_or(i64::MAX);
    let delay_us = i64::try_from(arrival_us)
      .unwrap_or(i64::MAX)
      .saturating_sub(sent_us);
    while self
      .delays
      .front()
      .is_some_and(|(at, _)| now.saturating_duration_since(*at) > BASELINE_WINDOW)
    {
      self.delays.pop_front();
    }
    self.delays.push_back((now, delay_us));
    let fastest_us = self
      .delays
      .iter()
      .map(|(_, delay)| *delay)
      .min()
      .unwrap_or(delay_us);
    let max_delay_us = i64::try_from(self.config.max_delay.as_micros()).unwrap_or(i64::MAX);
    delay_us.saturating_sub(fastest_us) > max_delay_us
  }

  /// Forgets what became of the voice so far: the link is measured afresh (a new video track,
  /// after a call of sound only, starts with every layer, #935).
  pub fn forget(&mut self) {
    self.reports.clear();
    self.delays.clear();
    self.maybe_hurt_at = None;
  }

  /// Whether voice was hurt within [`VoiceGuardConfig::hold`]: video yields.
  pub fn is_hurt(&self, now: Instant) -> bool {
    self
      .maybe_hurt_at
      .is_some_and(|at| now.saturating_duration_since(at) < self.config.hold)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const MS: Duration = Duration::from_millis(1);

  #[test]
  fn voice_that_arrives_is_not_hurt() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::default());
    let feedback = send_voice(&mut guard, 0, 25, start, |_| true);
    guard.on_feedback(&feedback, start + 500 * MS);
    assert!(!guard.is_hurt(start + 500 * MS));
  }

  #[test]
  fn lost_voice_is_hurt_at_once_and_for_the_hold() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::default());
    // A collapsed link: every third voice packet lost.
    let feedback = send_voice(&mut guard, 0, 25, start, |index| index % 3 != 0);
    let reported_at = start + 600 * MS;
    guard.on_feedback(&feedback, reported_at);
    assert!(guard.is_hurt(reported_at));
    let hold = VoiceGuardConfig::default().hold;
    assert!(guard.is_hurt(reported_at + hold - MS));
    assert!(!guard.is_hurt(reported_at + hold));
  }

  #[test]
  fn one_lost_packet_of_a_few_is_no_collapse() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::default());
    let feedback = send_voice(&mut guard, 0, 5, start, |index| index != 2);
    guard.on_feedback(&feedback, start + 200 * MS);
    assert!(!guard.is_hurt(start + 200 * MS));
  }

  #[test]
  fn loss_of_other_datagrams_doesnt_count() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::default());
    // Voice on even numbers, video on odd ones; all video lost, all voice arrived.
    for seq in (0..60u16).step_by(2) {
      guard.on_send(TransportSeq(seq), start);
    }
    let feedback = TransportFeedback {
      base_seq: 0,
      arrivals_us: (0..60u64)
        .map(|seq| (seq % 2 == 0).then_some(seq * 1000))
        .collect(),
    };
    guard.on_feedback(&feedback, start + 300 * MS);
    assert!(!guard.is_hurt(start + 300 * MS));
  }

  #[test]
  fn old_reports_leave_the_window() {
    let start = Instant::now();
    let config = VoiceGuardConfig {
      hold: Duration::ZERO,
      ..VoiceGuardConfig::default()
    };
    let mut guard = VoiceGuard::new(config);
    let lossy = send_voice(&mut guard, 0, 20, start, |index| index % 2 == 0);
    guard.on_feedback(&lossy, start + 100 * MS);
    // Two seconds later, voice arrives again: what was lost before is out of the window.
    let later = start + Duration::from_secs(2);
    let clean = send_voice(&mut guard, 20, 20, later, |_| true);
    guard.on_feedback(&clean, later + 100 * MS);
    assert!(!guard.is_hurt(later + 100 * MS));
  }

  #[test]
  fn voice_held_back_is_hurt_like_lost_voice() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::default());
    // A second of voice on time: the path's own delay.
    let on_time = send_voice(&mut guard, 0, 50, start, |_| true);
    guard.on_feedback(&on_time, start + Duration::from_secs(1));
    assert!(!guard.is_hurt(start + Duration::from_secs(1)));
    // Then voice waits 300 ms behind video in a collapsed link: it arrives, too late.
    let later = start + Duration::from_secs(1);
    for index in 0..25u16 {
      guard.on_send(TransportSeq(50 + index), later + 20 * MS * u32::from(index));
    }
    let held_back = TransportFeedback {
      base_seq: 50,
      arrivals_us: (0..25u64)
        .map(|index| Some(1_000_000 + index * 20_000 + 300_000))
        .collect(),
    };
    guard.on_feedback(&held_back, later + 800 * MS);
    assert!(guard.is_hurt(later + 800 * MS));
  }

  #[test]
  fn jitter_below_the_bound_is_not_hurt() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::default());
    for index in 0..50u16 {
      guard.on_send(TransportSeq(index), start + 20 * MS * u32::from(index));
    }
    let jittery = TransportFeedback {
      base_seq: 0,
      arrivals_us: (0..50u64)
        .map(|index| Some(index * 20_000 + (index % 5) * 20_000))
        .collect(),
    };
    guard.on_feedback(&jittery, start + Duration::from_secs(1));
    assert!(!guard.is_hurt(start + Duration::from_secs(1)));
  }

  #[test]
  fn sequence_numbers_wrap_around() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::default());
    let feedback = send_voice(&mut guard, u16::MAX - 10, 25, start, |index| index % 2 == 0);
    guard.on_feedback(&feedback, start + 300 * MS);
    assert!(guard.is_hurt(start + 300 * MS));
  }

  #[test]
  fn pending_voice_stays_bounded() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::default());
    for seq in 0..10_000u16 {
      guard.on_send(TransportSeq(seq), start);
    }
    assert_eq!(guard.pending.len(), MAX_PENDING);
    assert_eq!(guard.pending_since.len(), MAX_PENDING);
  }

  /// Sends `count` voice datagrams from `first`, 20 ms apart, and the feedback that reports them:
  /// `arrives(index)` says which arrived.
  fn send_voice(
    guard: &mut VoiceGuard,
    first: u16,
    count: u16,
    start: Instant,
    arrives: impl Fn(u16) -> bool,
  ) -> TransportFeedback {
    for index in 0..count {
      guard.on_send(
        TransportSeq(first.wrapping_add(index)),
        start + 20 * MS * u32::from(index),
      );
    }
    TransportFeedback {
      base_seq: first,
      arrivals_us: (0..count)
        .map(|index| arrives(index).then_some(u64::from(index) * 20_000))
        .collect(),
    }
  }
}
