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
/// Voice packets that may still arrive out of order: 60 ms of a voice.
const REORDER_PACKETS: u64 = 3;
/// The fastest voice datagram's delay within this long is the path's own: what comes later waited.
const BASELINE_WINDOW: Duration = Duration::from_secs(10);
/// A periodic check ([`VoiceGuard::on_timer`], every ~100 ms) this much after the one before came
/// late: the client's own threads didn't run meanwhile (a loaded machine, #2245), and the feedback
/// that came in for that long hasn't been read yet. Two missed ticks and a half.
const STARVED_TICK_GAP: Duration = Duration::from_millis(250);

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
  /// Voice that no feedback reported this much later than the fastest report lately is hurt as
  /// well ([`VoiceGuard::on_timer`]): it waits in a queue the link no longer drains (#1889). The
  /// fastest report is the link's round trip and the feedback's interval: a link across the
  /// world reports later than one next door.
  pub max_unreported: Duration,
  /// Voice hurt for this long without a break of [`VoiceGuardConfig::release_after`] is a link
  /// that collapsed (or is too thin): the video stays off for [`VoiceGuardConfig::hold`] after
  /// it. Hurt for less, it was a burst of loss, which pausing video doesn't help against
  /// (#2191): the video comes back [`VoiceGuardConfig::release_after`] after it. Zero: every
  /// episode lasts.
  pub lasting: Duration,
  /// How long after voice was last hurt by a burst of loss the video comes back.
  pub release_after: Duration,
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
  /// How long reports of voice took (from its sending to its feedback), for the baseline of
  /// [`VoiceGuardConfig::max_unreported`].
  report_times: VecDeque<(Instant, Duration)>,
  /// The sender's clock base for those delays.
  maybe_epoch: Option<Instant>,
  maybe_hurt_at: Option<Instant>,
  /// Since when voice is hurt without the hold running out in between.
  maybe_hurt_since: Option<Instant>,
  /// The episode since then had voice stuck in a queue ([`VoiceGuard::on_timer`]): congestion,
  /// whatever its length.
  is_episode_congested: bool,
  /// When [`VoiceGuard::on_timer`] last ran, to tell a late one.
  maybe_last_tick_at: Option<Instant>,
  /// When the client's threads last ran again after a pause ([`STARVED_TICK_GAP`]): voice counts
  /// as unreported only from then on.
  maybe_resumed_at: Option<Instant>,
}

impl Default for VoiceGuardConfig {
  fn default() -> Self {
    Self {
      window: Duration::from_millis(500),
      min_reported: 8,
      max_loss: 0.1,
      max_delay: Duration::from_millis(150),
      hold: Duration::from_secs(3),
      max_unreported: Duration::MAX,
      lasting: Duration::ZERO,
      release_after: Duration::from_millis(500),
    }
  }
}

impl VoiceGuardConfig {
  /// The own voice on the client's uplink: lost voice counts, voice that arrives late doesn't. The
  /// client sends its voice first (its send queue), so voice that comes late waited behind the
  /// client's own video in the link's queue: a screen share's slide change (a picture of up to a
  /// second of its bitrate) on a limited uplink, which the uplink allocation rides out (#941).
  /// Judged by lateness as well, every slide change stopped the share (#1888).
  ///
  /// Voice that no feedback reported for [`VoiceGuardConfig::max_unreported`] counts, unless the
  /// client itself just sent such a burst ([`VoiceGuard::on_timer`]): a collapsed uplink holds it
  /// in the link's queue for seconds before it overflows and loses it, and judged by losses only,
  /// the call was silent for 4 s before the camera stopped (#1889).
  ///
  /// Hotel or train Wi-Fi loses 1–3 % of the packets in bursts (#2191): a fifth of the voice
  /// within half a second lost is a burst of 100 ms and more, and voice hurt by it for less than
  /// [`VoiceGuardConfig::lasting`] holds the video only until [`VoiceGuardConfig::release_after`]
  /// later. At a tenth and the full hold, bursts held a presenter's video for 20 to 40 seconds of
  /// a minute (seeded simulation, `bursty_link`); a link that collapsed loses more and on, and is
  /// found 400–500 ms after the collapse (300 ms before).
  pub fn own_uplink() -> Self {
    Self {
      max_loss: 0.2,
      max_delay: Duration::MAX,
      max_unreported: Duration::from_millis(400),
      // A burst stays in the window for its length: half a second after it, the voice is found
      // hurt still. Loss that goes on for a second is no burst.
      lasting: Duration::from_secs(1),
      ..Self::default()
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
      report_times: VecDeque::new(),
      maybe_epoch: None,
      maybe_hurt_at: None,
      maybe_hurt_since: None,
      is_episode_congested: false,
      maybe_last_tick_at: None,
      maybe_resumed_at: None,
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
        self
          .report_times
          .push_back((now, now.saturating_duration_since(sent_at)));
        let is_hurt = match maybe_arrival {
          Some(arrival_us) => self.is_late(sent_at, *arrival_us, now),
          None => true,
        };
        self.reports.push_back((now, is_hurt));
      }
    }
    // Voice numbered before what the feedback starts with was skipped by it: no report will come.
    let pending_since = &mut self.pending_since;
    self.pending.retain(|seq| {
      let is_skipped = (feedback.base_seq.wrapping_sub(seq.0) as i16) > 0;
      if is_skipped {
        pending_since.remove(&seq.0);
      }
      !is_skipped && pending_since.contains_key(&seq.0)
    });
    while self
      .report_times
      .front()
      .is_some_and(|(at, _)| now.saturating_duration_since(*at) > BASELINE_WINDOW)
    {
      self.report_times.pop_front();
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
      self.note_hurt(now, false);
    }
  }

  /// Voice was found hurt at `now` (`is_stuck`: waiting in a queue the link doesn't drain): it
  /// goes on the episode whose hold still runs, or starts one.
  fn note_hurt(&mut self, now: Instant, is_stuck: bool) {
    // Within the hold of the episode before, it goes on: a burst after a collapse doesn't
    // shorten the collapse's hold.
    if !self.is_hurt(now) {
      self.maybe_hurt_since = Some(now);
      self.is_episode_congested = false;
    }
    self.is_episode_congested |= is_stuck;
    self.maybe_hurt_at = Some(now);
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

  /// Periodic check (every ~100 ms): voice that no feedback reported
  /// [`VoiceGuardConfig::max_unreported`] later than the fastest report lately is hurt, unless
  /// `has_sent_burst`: the client itself just sent a burst the link has to queue (a slide
  /// change, a keyframe), behind which voice waits and still arrives. Nothing before the first
  /// reports: there is no baseline yet.
  ///
  /// A check that comes [`STARVED_TICK_GAP`] or more after the one before finds the client's own
  /// threads were starved (#2245): the feedback is waiting to be read, not the voice in the link's
  /// queue. Voice counts as unreported only for the time since: on a loaded laptop a screen share
  /// was held for its voice for the rest of a call with no packet lost at all.
  pub fn on_timer(&mut self, has_sent_burst: bool, now: Instant) {
    let is_late = self
      .maybe_last_tick_at
      .is_some_and(|last| now.saturating_duration_since(last) >= STARVED_TICK_GAP);
    self.maybe_last_tick_at = Some(now);
    if is_late {
      self.maybe_resumed_at = Some(now);
    }
    let Some(fastest) = self.report_times.iter().map(|(_, took)| *took).min() else {
      return;
    };
    if has_sent_burst {
      return;
    }
    let bound = fastest.saturating_add(self.config.max_unreported);
    let maybe_resumed_at = self.maybe_resumed_at;
    let is_stuck = self
      .pending
      .front()
      .and_then(|seq| self.pending_since.get(&seq.0))
      .map(|sent_at| maybe_resumed_at.map_or(*sent_at, |resumed_at| resumed_at.max(*sent_at)))
      .is_some_and(|since| now.saturating_duration_since(since) > bound);
    if is_stuck {
      // Random loss doesn't hold voice back: the link queues it, it is congested.
      self.note_hurt(now, true);
    }
  }

  /// Forgets what became of the voice so far: the link is measured afresh (a new video track,
  /// after a call of sound only, starts with every layer, #935).
  pub fn forget(&mut self) {
    // Voice still waiting for a report too: a new connection numbers its datagrams from the
    // start, and its reports would be read as the fate of the old one's voice.
    self.pending.clear();
    self.pending_since.clear();
    self.reports.clear();
    self.delays.clear();
    self.report_times.clear();
    self.maybe_hurt_at = None;
    self.maybe_hurt_since = None;
    self.is_episode_congested = false;
    self.maybe_resumed_at = None;
  }

  /// When voice was last found hurt (the hold not counted).
  pub fn last_hurt_at(&self) -> Option<Instant> {
    self.maybe_hurt_at
  }

  /// Whether video yields to the voice: it was hurt within [`VoiceGuardConfig::hold`] after loss
  /// that lasted, or within [`VoiceGuardConfig::release_after`] after a burst.
  pub fn is_hurt(&self, now: Instant) -> bool {
    let Some(hurt_at) = self.maybe_hurt_at else {
      return false;
    };
    let hold = if self.is_lasting() {
      self.config.hold
    } else {
      self.config.release_after
    };
    now.saturating_duration_since(hurt_at) < hold
  }

  /// Whether the voice's last hurt episode lasted [`VoiceGuardConfig::lasting`], or held voice
  /// back in a queue: a link that collapsed or is too thin, not a burst of loss.
  pub fn is_lasting(&self) -> bool {
    match (self.maybe_hurt_since, self.maybe_hurt_at) {
      (Some(since), Some(at)) => {
        self.is_episode_congested || at.saturating_duration_since(since) >= self.config.lasting
      },
      _ => false,
    }
  }
}

/// Whether the voice a sender's upload delivers has gaps (#1889): the receiving end of a link (the
/// SFU, for each participant's upload) counts the voice's RTP sequence numbers as they arrive.
///
/// A sender whose upload collapsed loses its voice also after its camera stopped: the link is
/// shared with the acknowledgements of everything it receives (a third of 120 kbit/s for one
/// camera on QUIC). Once its voice of the last [`VoiceGuardConfig::window`] arrived with
/// [`VoiceGuardConfig::max_loss`] or more missing, it gets no video for
/// [`VoiceGuardConfig::hold`] after the last such moment ([`ArrivingVoice::is_hurt`]).
#[derive(Debug)]
pub struct ArrivingVoice {
  config: VoiceGuardConfig,
  /// Arrived voice: when, and its RTP sequence number, unwrapped.
  arrivals: VecDeque<(Instant, u64)>,
  maybe_highest: Option<u64>,
  maybe_hurt_at: Option<Instant>,
}

impl ArrivingVoice {
  pub fn new(config: VoiceGuardConfig) -> Self {
    Self {
      config,
      arrivals: VecDeque::new(),
      maybe_highest: None,
      maybe_hurt_at: None,
    }
  }

  /// A voice packet with RTP sequence number `sequence_number` arrived.
  pub fn on_arrival(&mut self, sequence_number: u16, now: Instant) {
    let extended = match self.maybe_highest {
      Some(highest) => unwrap_sequence_number(sequence_number, highest),
      None => u64::from(sequence_number) + (1 << 16),
    };
    self.maybe_highest = Some(
      self
        .maybe_highest
        .map_or(extended, |highest| highest.max(extended)),
    );
    let window = self.config.window;
    while self
      .arrivals
      .front()
      .is_some_and(|(at, _)| now.saturating_duration_since(*at) > window)
    {
      self.arrivals.pop_front();
    }
    self.arrivals.push_back((now, extended));
    while self.arrivals.len() > MAX_PENDING {
      self.arrivals.pop_front();
    }
    // The newest few may still come, out of order.
    let settled = self
      .maybe_highest
      .unwrap_or(extended)
      .saturating_sub(REORDER_PACKETS);
    let lowest = self
      .arrivals
      .iter()
      .map(|(_, seq)| *seq)
      .min()
      .unwrap_or(extended);
    if settled < lowest {
      return;
    }
    let expected = usize::try_from(settled - lowest + 1).unwrap_or(usize::MAX);
    let arrived = self.arrivals.iter().filter(|(_, seq)| *seq <= settled).count();
    let missing = expected.saturating_sub(arrived);
    if expected >= self.config.min_reported && missing as f64 >= self.config.max_loss * expected as f64 {
      self.maybe_hurt_at = Some(now);
    }
  }

  /// Whether the sender's voice arrived with gaps within [`VoiceGuardConfig::hold`].
  pub fn is_hurt(&self, now: Instant) -> bool {
    self
      .maybe_hurt_at
      .is_some_and(|at| now.saturating_duration_since(at) < self.config.hold)
  }
}

/// `sequence_number` as the 64-bit value closest to `reference`.
fn unwrap_sequence_number(sequence_number: u16, reference: u64) -> u64 {
  let base = reference & !0xFFFF;
  [base.saturating_sub(0x1_0000), base, base + 0x1_0000]
    .map(|candidate| candidate + u64::from(sequence_number))
    .into_iter()
    .min_by_key(|candidate| candidate.abs_diff(reference))
    .unwrap_or(base + u64::from(sequence_number))
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

  /// #2191: on the own uplink, a burst of loss holds the video until shortly after it; loss
  /// that goes on for a second holds it for the full hold after.
  #[test]
  fn a_burst_holds_until_shortly_after_it_lasting_loss_for_the_hold() {
    let start = Instant::now();
    let config = VoiceGuardConfig::own_uplink();
    let mut guard = VoiceGuard::new(config);
    let burst = send_voice(&mut guard, 0, 25, start, |index| !(10..16).contains(&index));
    let reported_at = start + 600 * MS;
    guard.on_feedback(&burst, reported_at);
    assert!(guard.is_hurt(reported_at));
    assert!(!guard.is_lasting());
    assert!(!guard.is_hurt(reported_at + config.release_after));

    // Lost on and on: every feedback for over a second finds it hurt.
    let mut guard = VoiceGuard::new(config);
    let mut at = start;
    for round in 0..14u16 {
      let lossy = send_voice(&mut guard, round * 5, 5, at, |index| index % 2 == 0);
      at += 100 * MS;
      guard.on_feedback(&lossy, at);
    }
    assert!(guard.is_lasting());
    assert!(guard.is_hurt(at + config.hold - MS));
    assert!(!guard.is_hurt(at + config.hold));
  }

  /// Voice stuck in a queue the link doesn't drain is congestion at once (#1889): the full hold.
  #[test]
  fn stuck_voice_holds_for_the_hold() {
    let start = Instant::now();
    let config = VoiceGuardConfig::own_uplink();
    let mut guard = VoiceGuard::new(config);
    let arrived = send_voice(&mut guard, 0, 5, start, |_| true);
    guard.on_feedback(&arrived, start + 150 * MS);
    guard.on_send(TransportSeq(5), start + 200 * MS);
    let stuck_at = start + 900 * MS;
    guard.on_timer(false, stuck_at);
    assert!(guard.is_lasting());
    assert!(guard.is_hurt(stuck_at + config.hold - MS));
  }

  /// A burst within a collapse's hold goes on its episode: it doesn't shorten the hold.
  #[test]
  fn a_burst_within_the_hold_does_not_shorten_it() {
    let start = Instant::now();
    let config = VoiceGuardConfig::own_uplink();
    let mut guard = VoiceGuard::new(config);
    guard.on_send(TransportSeq(0), start);
    guard.on_feedback(
      &TransportFeedback {
        base_seq: 0,
        arrivals_us: vec![Some(20_000)],
      },
      start + 100 * MS,
    );
    guard.on_send(TransportSeq(1), start + 120 * MS);
    let stuck_at = start + Duration::from_secs(1);
    guard.on_timer(false, stuck_at);
    let burst_at = stuck_at + Duration::from_secs(1);
    let burst = send_voice(&mut guard, 2, 25, burst_at - 500 * MS, |index| {
      !(10..16).contains(&index)
    });
    guard.on_feedback(&burst, burst_at);
    assert!(guard.is_lasting());
    assert!(guard.is_hurt(burst_at + config.hold - MS));
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

  /// The client's own uplink: voice held up behind its own burst arrives, and isn't hurt.
  #[test]
  fn held_back_voice_doesnt_count_where_only_losses_do() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::own_uplink());
    let on_time = send_voice(&mut guard, 0, 50, start, |_| true);
    guard.on_feedback(&on_time, start + Duration::from_secs(1));
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
    assert!(!guard.is_hurt(later + 800 * MS));
    // Lost voice still is.
    let lossy = send_voice(&mut guard, 75, 25, later + Duration::from_secs(1), |index| {
      index % 3 != 0
    });
    guard.on_feedback(&lossy, later + Duration::from_millis(1500));
    assert!(guard.is_hurt(later + Duration::from_millis(1500)));
  }

  /// A collapsed uplink holds the voice in its queue: hurt once it went unreported for the bound,
  /// before any loss is reported.
  #[test]
  fn voice_unreported_too_long_is_hurt() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::own_uplink());
    let on_time = send_voice(&mut guard, 0, 25, start, |_| true);
    guard.on_feedback(&on_time, start + 500 * MS);
    let stuck_from = start + 500 * MS;
    guard.on_send(TransportSeq(25), stuck_from);
    // The fastest report took 20 ms (the last packet, sent 480 ms in): 420 ms is the bound.
    guard.on_timer(false, stuck_from + 420 * MS);
    assert!(!guard.is_hurt(stuck_from + 420 * MS));
    guard.on_timer(false, stuck_from + 500 * MS);
    assert!(guard.is_hurt(stuck_from + 500 * MS));
  }

  /// A link that reports late anyway (a long round trip) has a later bound.
  #[test]
  fn a_slow_link_has_a_later_bound() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::own_uplink());
    let on_time = send_voice(&mut guard, 0, 25, start, |_| true);
    // The last packet (480 ms in) reported 400 ms later.
    guard.on_feedback(&on_time, start + 880 * MS);
    let stuck_from = start + Duration::from_secs(1);
    guard.on_send(TransportSeq(25), stuck_from);
    guard.on_timer(false, stuck_from + 700 * MS);
    assert!(!guard.is_hurt(stuck_from + 700 * MS));
    guard.on_timer(false, stuck_from + 900 * MS);
    assert!(guard.is_hurt(stuck_from + 900 * MS));
  }

  /// #2245: a loaded machine runs the client's threads late. A check that comes long after the
  /// one before finds the feedback still unread, not the voice stuck: the voice counts as
  /// unreported only from then on, and the feedback read right after reports it.
  #[test]
  fn a_late_check_is_the_own_threads_not_the_link() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::own_uplink());
    let on_time = send_voice(&mut guard, 0, 25, start, |_| true);
    guard.on_feedback(&on_time, start + 500 * MS);
    guard.on_timer(false, start + 500 * MS);
    guard.on_send(TransportSeq(25), start + 520 * MS);
    // The threads stall for a second; the check comes first, the feedback right after it.
    let resumed_at = start + 1520 * MS;
    guard.on_timer(false, resumed_at);
    assert!(!guard.is_hurt(resumed_at), "the feedback waits unread");
    let reported = TransportFeedback {
      base_seq: 25,
      arrivals_us: vec![Some(520_000)],
    };
    guard.on_feedback(&reported, resumed_at + MS);
    let mut at = resumed_at;
    for _ in 0..10 {
      at += 100 * MS;
      guard.on_timer(false, at);
    }
    assert!(!guard.is_hurt(at));
  }

  /// After a stall, voice that the link still holds is stuck once it went unreported for the
  /// bound since the threads ran again.
  #[test]
  fn voice_still_unreported_after_a_stall_is_stuck() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::own_uplink());
    let on_time = send_voice(&mut guard, 0, 25, start, |_| true);
    guard.on_feedback(&on_time, start + 500 * MS);
    guard.on_timer(false, start + 500 * MS);
    guard.on_send(TransportSeq(25), start + 520 * MS);
    let resumed_at = start + 1520 * MS;
    guard.on_timer(false, resumed_at);
    // The fastest report took 20 ms: 420 ms is the bound, counted from the resume.
    let mut at = resumed_at;
    while at < resumed_at + 400 * MS {
      at += 100 * MS;
      guard.on_timer(false, at);
      assert!(!guard.is_hurt(at));
    }
    guard.on_timer(false, resumed_at + 500 * MS);
    assert!(guard.is_hurt(resumed_at + 500 * MS));
  }

  /// Before any report there is no baseline: nothing counts as unreported.
  #[test]
  fn no_reports_yet_is_no_sign() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::own_uplink());
    guard.on_send(TransportSeq(0), start);
    guard.on_timer(false, start + Duration::from_secs(2));
    assert!(!guard.is_hurt(start + Duration::from_secs(2)));
  }

  /// Behind the client's own burst voice waits and arrives: not hurt meanwhile.
  #[test]
  fn voice_behind_an_own_burst_is_not_hurt_while_unreported() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::own_uplink());
    let on_time = send_voice(&mut guard, 0, 25, start, |_| true);
    guard.on_feedback(&on_time, start + 500 * MS);
    guard.on_send(TransportSeq(25), start + 500 * MS);
    guard.on_timer(true, start + Duration::from_secs(2));
    assert!(!guard.is_hurt(start + Duration::from_secs(2)));
  }

  /// Voice a feedback skipped gets no report: it is forgotten, not stuck for ever.
  #[test]
  fn voice_a_feedback_skipped_is_not_stuck() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::own_uplink());
    guard.on_send(TransportSeq(0), start);
    let later = send_voice(&mut guard, 1, 25, start + 20 * MS, |_| true);
    let skipping = TransportFeedback { base_seq: 1, ..later };
    guard.on_feedback(&skipping, start + 600 * MS);
    guard.on_timer(false, start + Duration::from_secs(2));
    assert!(!guard.is_hurt(start + Duration::from_secs(2)));
  }

  /// Without the bound (the SFU's receivers, the default) unreported voice is no sign.
  #[test]
  fn unreported_voice_counts_only_with_a_bound() {
    let start = Instant::now();
    let mut guard = VoiceGuard::new(VoiceGuardConfig::default());
    let on_time = send_voice(&mut guard, 0, 25, start, |_| true);
    guard.on_feedback(&on_time, start + 500 * MS);
    guard.on_send(TransportSeq(25), start + 500 * MS);
    guard.on_timer(false, start + Duration::from_secs(5));
    assert!(!guard.is_hurt(start + Duration::from_secs(5)));
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

  mod arriving {
    use super::*;

    #[test]
    fn voice_that_arrives_whole_is_not_hurt() {
      let start = Instant::now();
      let mut voice = ArrivingVoice::new(VoiceGuardConfig::default());
      for seq in 0..50u16 {
        voice.on_arrival(seq, start + 20 * MS * u32::from(seq));
      }
      assert!(!voice.is_hurt(start + Duration::from_secs(1)));
    }

    #[test]
    fn voice_with_gaps_is_hurt_for_the_hold() {
      let start = Instant::now();
      let mut voice = ArrivingVoice::new(VoiceGuardConfig::default());
      let mut last = start;
      for seq in (0..50u16).filter(|seq| seq % 3 != 0) {
        last = start + 20 * MS * u32::from(seq);
        voice.on_arrival(seq, last);
      }
      assert!(voice.is_hurt(last));
      let hold = VoiceGuardConfig::default().hold;
      assert!(!voice.is_hurt(last + hold));
    }

    #[test]
    fn reordering_and_wrap_around_are_no_gaps() {
      let start = Instant::now();
      let mut voice = ArrivingVoice::new(VoiceGuardConfig::default());
      let first = u16::MAX - 20;
      for index in 0..50u16 {
        // Pairs swapped on the way.
        let swapped = if index % 2 == 0 { index + 1 } else { index - 1 };
        voice.on_arrival(first.wrapping_add(swapped), start + 20 * MS * u32::from(index));
      }
      assert!(!voice.is_hurt(start + Duration::from_secs(1)));
    }

    #[test]
    fn a_muted_moment_is_no_gap() {
      let start = Instant::now();
      let mut voice = ArrivingVoice::new(VoiceGuardConfig::default());
      for seq in 0..10u16 {
        voice.on_arrival(seq, start + 20 * MS * u32::from(seq));
      }
      // Two seconds without voice, then it goes on where it stopped.
      let later = start + Duration::from_secs(2);
      for seq in 10..30u16 {
        voice.on_arrival(seq, later + 20 * MS * u32::from(seq - 10));
      }
      assert!(!voice.is_hurt(later + Duration::from_secs(1)));
    }
  }

  /// #2191: a presenter's uplink on hotel or train Wi-Fi (1–3 % loss in bursts, 20–60 ms), its
  /// voice next to a still screen; five seeds of a minute.
  mod bursty_link {
    use super::*;
    use crate::bursty_loss::BurstyLoss;

    const SEEDS: [u64; 5] = [1, 2, 3, 4, 5];

    /// At a tenth of the voice lost and the full hold, the bursts held the video 19–39 s of a
    /// minute, up to 11 s at once. Now 0–4.4 s, a second at most at once.
    #[test]
    fn bursts_of_loss_hold_the_video_a_moment_only() {
      for seed in SEEDS {
        let run = simulate(seed, VoiceGuardConfig::own_uplink(), None);
        assert!(
          run.held <= Duration::from_secs(5),
          "seed {seed}: held {:?} of a minute",
          run.held
        );
        assert!(
          run.longest <= Duration::from_millis(1200),
          "seed {seed}: held {:?} at once",
          run.longest
        );
      }
    }

    /// #1871 still: a link that collapses (a third to nine tenths of everything lost from then
    /// on) is found within 600 ms and the video stays off.
    #[test]
    fn a_collapse_is_found_at_once_and_held() {
      for (lost_of, every) in [(9, 10), (1, 2), (1, 3)] {
        for seed in SEEDS {
          let run = simulate(seed + 10, VoiceGuardConfig::own_uplink(), Some((lost_of, every)));
          let found_after = run.maybe_hurt_after_collapse.unwrap_or(Duration::MAX);
          assert!(
            found_after <= Duration::from_millis(600),
            "{lost_of}/{every}, seed {seed}: found after {found_after:?}"
          );
          assert!(run.is_lasting, "{lost_of}/{every}, seed {seed}");
          assert!(
            run.held >= Duration::from_secs(29),
            "{lost_of}/{every}, seed {seed}: held {:?}",
            run.held
          );
        }
      }
    }

    struct Run {
      held: Duration,
      longest: Duration,
      holds: usize,
      maybe_hurt_after_collapse: Option<Duration>,
      is_lasting: bool,
    }

    /// Voice every 20 ms, a still screen's tiny frame every 200 ms, its refresh (3 datagrams)
    /// every second and a slide (50 datagrams at once) after 1 s and 2 s in turn, numbered
    /// together, through [`BurstyLoss`]; feedback every 100 ms on what arrived 150 ms before.
    /// `maybe_collapse` (`lost_of` in `every` datagrams lost from 30 s on): when voice was found
    /// hurt after.
    fn simulate(seed: u64, config: VoiceGuardConfig, maybe_collapse: Option<(u64, u64)>) -> Run {
      const TICK: Duration = Duration::from_millis(5);
      const END: Duration = Duration::from_secs(60);
      const COLLAPSE_AT: Duration = Duration::from_secs(30);
      let start = Instant::now();
      let mut link = BurstyLoss::hotel_wifi(seed);
      let mut guard = VoiceGuard::new(config);
      // (seq, sent at, arrival at receiver's clock in µs since start).
      let mut sent: VecDeque<(u16, Instant, Option<u64>)> = VecDeque::new();
      let mut seq = 0u16;
      let mut next_slide_at = Duration::from_secs(5);
      let mut is_short_slide = true;
      let mut maybe_hold_since: Option<Instant> = None;
      let mut run = Run {
        held: Duration::ZERO,
        longest: Duration::ZERO,
        holds: 0,
        maybe_hurt_after_collapse: None,
        is_lasting: false,
      };
      let mut elapsed = Duration::ZERO;
      while elapsed < END {
        let now = start + elapsed;
        let ms = elapsed.as_millis();
        let mut datagrams = Vec::new();
        if ms.is_multiple_of(20) {
          datagrams.push(true);
        }
        if ms.is_multiple_of(200) {
          datagrams.push(false);
        }
        if ms.is_multiple_of(1000) {
          datagrams.extend([false; 3]);
        }
        if elapsed >= next_slide_at {
          datagrams.extend([false; 50]);
          next_slide_at += Duration::from_secs(if is_short_slide { 1 } else { 2 });
          is_short_slide = !is_short_slide;
        }
        for is_voice in datagrams {
          if is_voice {
            guard.on_send(TransportSeq(seq), now);
          }
          let is_lost = link.is_lost(now)
            || maybe_collapse
              .is_some_and(|(lost_of, every)| elapsed >= COLLAPSE_AT && u64::from(seq) % every < lost_of);
          let arrival_us = (elapsed + link.delay()).as_micros() as u64;
          sent.push_back((seq, now, (!is_lost).then_some(arrival_us)));
          seq = seq.wrapping_add(1);
        }
        if ms.is_multiple_of(100) {
          let settled = elapsed.saturating_sub(link.delay() + Duration::from_millis(150));
          let mut arrivals_us = Vec::new();
          let maybe_base = sent.front().map(|(first, ..)| *first);
          while sent
            .front()
            .is_some_and(|(_, sent_at, _)| sent_at.saturating_duration_since(start) <= settled)
          {
            if let Some((_, _, maybe_arrival)) = sent.pop_front() {
              arrivals_us.push(maybe_arrival);
            }
          }
          if let (Some(base_seq), false) = (maybe_base, arrivals_us.is_empty()) {
            guard.on_feedback(
              &TransportFeedback {
                base_seq,
                arrivals_us,
              },
              now,
            );
          }
          guard.on_timer(false, now);
          let is_hurt = guard.is_hurt(now);
          if is_hurt && maybe_collapse.is_some() && elapsed >= COLLAPSE_AT {
            run.maybe_hurt_after_collapse.get_or_insert(elapsed - COLLAPSE_AT);
          }
          match (is_hurt, maybe_hold_since) {
            (true, None) => {
              maybe_hold_since = Some(now);
              run.holds += 1;
            },
            (false, Some(since)) => {
              let held = now.saturating_duration_since(since);
              run.held += held;
              run.longest = run.longest.max(held);
              maybe_hold_since = None;
            },
            _ => {},
          }
        }
        elapsed += TICK;
      }
      if let Some(since) = maybe_hold_since {
        let held = (start + END).saturating_duration_since(since);
        run.held += held;
        run.longest = run.longest.max(held);
      }
      run.is_lasting = guard.is_lasting();
      run
    }
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
