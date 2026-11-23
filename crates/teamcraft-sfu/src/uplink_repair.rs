//! Repairing what a publisher's uplink lost (#2189).
//!
//! A packet lost between the publisher and the SFU is lost for every viewer. Viewers can't get it
//! from the SFU's history (it never arrived), so before, each of them waited out its NACKs, asked
//! for a keyframe, and the publisher sent one through the same lossy uplink. A still screen sends
//! almost nothing between slide changes: one lost packet of a change held the old slide for
//! seconds, and a slide shown briefly could be skipped altogether.
//!
//! Now the SFU watches the RTP sequence numbers of each layer it forwards. A gap is asked of the
//! publisher at once ([`teamcraft_proto::CallEvent::RetransmitRequested`]), again after about one
//! and a half of the publisher's round trips, at most [`UplinkRepairConfig::max_requests`] times
//! (the same rules as a viewer's NACKs, [`teamcraft_media::nack`]). The publisher keeps what it
//! sent for the SFU's retransmission window ([`teamcraft_media::retransmit`]) and sends it once
//! more, still DAVE encrypted; the SFU forwards it like any packet, so every viewer gets it within
//! a round trip of the uplink, and keeps it for viewers' NACKs.
//!
//! A packet still missing after [`UplinkRepairConfig::screen_keyframe_after`] (a screen share) or
//! [`UplinkRepairConfig::camera_keyframe_after`] (a camera) is given up: the publisher is asked for
//! a keyframe of the layer, at most once per [`UplinkRepairConfig::min_keyframe_interval`], so a
//! lossy uplink doesn't turn the layer into keyframes only. Layers nobody receives are watched
//! (their numbering goes on) but not repaired.
//!
//! A keyframe the uplink loses as well (its packets given up) is no reason to ask for the next one
//! as soon: a limited uplink whose queue can't hold a keyframe of a screen's top layer dropped its
//! tail, and every keyframe asked for after it overflowed the same queue (#2246, five of them in
//! 2.2 s). Each keyframe lost in a row doubles the wait before the next one is asked for, up to
//! [`UplinkRepairConfig::max_keyframe_interval`], so the link drains what it queued; meanwhile the
//! layer's other keyframe requests (its receivers', who lost the same packets) wait too
//! ([`UplinkRepair::holds_keyframes`]). A keyframe that got through ends it. A keyframe that is due
//! while the wait runs is asked for when it ends, not dropped: a still screen sends nothing more
//! that would ask again.
//!
//! The SFU reads nothing but the RTP sequence number it already routes by.

use std::ops::Range;
use std::time::{Duration, Instant};

use teamcraft_media::nack::{MissingPackets, NackConfig};
use teamcraft_proto::VideoTrackKind;

/// A packet this far behind the next expected one is no late packet but a new start of the
/// publisher's numbering: watching starts over.
const RESTART_DISTANCE: u64 = 1 << 12;
/// Extended sequence numbers start here, so a number behind the first one never goes below zero.
const SEQUENCE_BASE: u64 = 1 << 32;

/// How lost uplink packets are asked for, and when a keyframe is asked for instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UplinkRepairConfig {
  /// How often one missing packet is asked of the publisher at most; zero turns the repair off.
  pub max_requests: u8,
  /// The publisher's round trip assumed until one is measured on its answers.
  pub initial_rtt: Duration,
  /// A packet of a screen share still missing this long after a gap showed it is given up: the
  /// publisher is asked for a keyframe. Short, as a slide change is all a still screen sends.
  pub screen_keyframe_after: Duration,
  /// The same for a camera, whose next frames come anyway and whose viewers ask themselves.
  pub camera_keyframe_after: Duration,
  /// Keyframes asked for because of uplink loss, per track and layer, at most this often.
  pub min_keyframe_interval: Duration,
  /// The longest wait between keyframes asked for while the uplink loses them as well: the
  /// interval doubles with each keyframe lost in a row up to this (#2246).
  pub max_keyframe_interval: Duration,
}

/// The publisher's packets of one layer: which are missing, and what to do about them.
#[derive(Debug)]
pub struct UplinkRepair {
  /// `None`: the repair is off; only the numbering is followed.
  maybe_config: Option<UplinkRepairConfig>,
  missing: MissingPackets,
  /// The extended sequence number expected next; `None` before the first packet.
  maybe_expected: Option<u64>,
  maybe_keyframe_requested_at: Option<Instant>,
  /// A keyframe was due while the interval ran: it is asked for once the interval is over.
  is_keyframe_owed: bool,
  /// The packets of the latest keyframe that arrived in part or whole.
  maybe_keyframe: Option<KeyframeSpan>,
  /// Keyframes in a row whose packets the uplink lost and the repair gave up.
  lost_keyframes: u32,
}

/// Where an arriving packet sits in a keyframe: its fragment's index and the fragment count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyframeFragment {
  pub index: u16,
  pub count: u16,
}

/// The extended sequence numbers of one keyframe's packets, and whether its fate is known.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyframeSpan {
  packets: Range<u64>,
  is_judged: bool,
}

/// What a packet's arrival or a timer asks of the SFU.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RepairStep {
  /// RTP sequence numbers to ask the publisher for, oldest first.
  pub requests: Vec<u16>,
  /// Packets were given up: the publisher should send a keyframe of the layer.
  pub needs_keyframe: bool,
}

impl Default for UplinkRepairConfig {
  fn default() -> Self {
    Self {
      max_requests: 3,
      initial_rtt: Duration::from_millis(100),
      screen_keyframe_after: Duration::from_millis(300),
      camera_keyframe_after: Duration::from_millis(500),
      min_keyframe_interval: Duration::from_secs(1),
      max_keyframe_interval: Duration::from_secs(4),
    }
  }
}

impl UplinkRepairConfig {
  /// Whether lost uplink packets are asked for at all.
  pub fn is_enabled(&self) -> bool {
    self.max_requests > 0
  }

  /// How long a packet of a `kind` track is waited for before a keyframe is asked for.
  pub fn keyframe_after(&self, kind: &VideoTrackKind) -> Duration {
    match kind {
      VideoTrackKind::Screen => self.screen_keyframe_after,
      _ => self.camera_keyframe_after,
    }
  }
}

impl UplinkRepair {
  /// Watches one layer of a `kind` track; `maybe_config` is `None` where nothing is repaired (the
  /// SFU keeps no packets, so publishers keep none either).
  pub fn new(maybe_config: Option<UplinkRepairConfig>, kind: &VideoTrackKind) -> Self {
    let maybe_config = maybe_config.filter(UplinkRepairConfig::is_enabled);
    let maybe_nack = maybe_config.map(|config| NackConfig {
      // Nothing older is asked for: it has been given up for a keyframe.
      window: config.keyframe_after(kind),
      max_requests: config.max_requests,
      initial_rtt: config.initial_rtt,
    });
    Self {
      maybe_config,
      missing: MissingPackets::new(maybe_nack),
      maybe_expected: None,
      maybe_keyframe_requested_at: None,
      is_keyframe_owed: false,
      maybe_keyframe: None,
      lost_keyframes: 0,
    }
  }

  /// A packet with `sequence_number` arrived; `maybe_keyframe` when it is a fragment of a
  /// keyframe. `is_wanted`: someone receives the layer, so what's missing of it is asked for.
  pub fn on_arrival(
    &mut self,
    sequence_number: u16,
    maybe_keyframe: Option<KeyframeFragment>,
    is_wanted: bool,
    now: Instant,
  ) -> RepairStep {
    let sequence = self.extend(sequence_number);
    let mut step = RepairStep::default();
    match self.maybe_expected {
      Some(expected) if sequence < expected && expected - sequence > RESTART_DISTANCE => {
        self.restart(sequence);
      },
      Some(expected) if sequence < expected => {
        // Repaired, reordered, or a duplicate: whichever, it's no longer missing.
        self.missing.arrived(sequence, now);
      },
      Some(expected) => {
        self.maybe_expected = Some(sequence + 1);
        self.note_keyframe(sequence, maybe_keyframe);
        if is_wanted && self.missing.asks_for_packets() && !self.missing.add(expected..sequence, now) {
          // More missing than a NACK names: a burst no retransmission repairs in time.
          let is_keyframe_lost = self.is_keyframe_hit(expected..sequence);
          self.missing.clear();
          self.note_given_up(is_keyframe_lost);
          step.needs_keyframe = self.may_ask_for_keyframe(now);
        }
      },
      None => {
        self.maybe_expected = Some(sequence + 1);
        self.note_keyframe(sequence, maybe_keyframe);
      },
    }
    if !is_wanted {
      self.missing.clear();
      return step;
    }
    self.judge_keyframe_through();
    step.requests = self.due_requests(now);
    step
  }

  /// Retries and give-ups; called periodically (every ~100 ms) with whether someone still receives
  /// the layer.
  pub fn on_timer(&mut self, is_wanted: bool, now: Instant) -> RepairStep {
    let mut step = RepairStep::default();
    if !is_wanted {
      // Nobody to show a keyframe to: the next one comes with the next receiver.
      self.missing.clear();
      self.is_keyframe_owed = false;
      return step;
    }
    let span = self.unjudged_keyframe_packets();
    let missing_of_keyframe = self.missing.count_in(span.clone());
    if self.missing.expire(now).is_some() {
      self.note_given_up(self.missing.count_in(span) < missing_of_keyframe);
      self.is_keyframe_owed = true;
    }
    if self.is_keyframe_owed {
      step.needs_keyframe = self.may_ask_for_keyframe(now);
    }
    step.requests = self.due_requests(now);
    step
  }

  /// Whether other keyframe requests for the layer wait (#2246): the uplink lost the keyframes
  /// asked for lately, and the wait before the next one runs. The repair asks once it is over.
  pub fn holds_keyframes(&self, now: Instant) -> bool {
    self.lost_keyframes > 0
      && self
        .maybe_keyframe_requested_at
        .is_some_and(|at| now.saturating_duration_since(at) < self.keyframe_interval())
  }

  /// Whether packets are missing now.
  pub fn is_missing_packets(&self) -> bool {
    !self.missing.is_empty()
  }

  fn due_requests(&mut self, now: Instant) -> Vec<u16> {
    self
      .missing
      .due_requests(now)
      .into_iter()
      // The low 16 bits are the RTP sequence number.
      .map(|sequence| (sequence & u64::from(u16::MAX)) as u16)
      .collect()
  }

  /// Whether a keyframe may be asked for now (at most once per interval), counted as asked; one
  /// that may not is owed until it may.
  fn may_ask_for_keyframe(&mut self, now: Instant) -> bool {
    if self.maybe_config.is_none() {
      return false;
    }
    let interval = self.keyframe_interval();
    let is_due = self
      .maybe_keyframe_requested_at
      .is_none_or(|at| now.saturating_duration_since(at) >= interval);
    if is_due {
      self.maybe_keyframe_requested_at = Some(now);
    }
    self.is_keyframe_owed = !is_due;
    is_due
  }

  /// The wait between keyframes asked for: doubled for each keyframe lost in a row, within the
  /// configured bounds.
  fn keyframe_interval(&self) -> Duration {
    let Some(config) = self.maybe_config else {
      return Duration::MAX;
    };
    let factor = 1u32.checked_shl(self.lost_keyframes).unwrap_or(u32::MAX);
    config
      .min_keyframe_interval
      .saturating_mul(factor)
      .min(config.max_keyframe_interval.max(config.min_keyframe_interval))
  }

  /// Follows the latest keyframe's packets: a fragment of a new one starts its span.
  fn note_keyframe(&mut self, sequence: u64, maybe_keyframe: Option<KeyframeFragment>) {
    let Some(fragment) = maybe_keyframe else {
      return;
    };
    let start = sequence.saturating_sub(u64::from(fragment.index));
    let packets = start..start + u64::from(fragment.count.max(1));
    if self
      .maybe_keyframe
      .as_ref()
      .is_none_or(|span| span.packets != packets)
    {
      self.maybe_keyframe = Some(KeyframeSpan {
        packets,
        is_judged: false,
      });
    }
  }

  /// The packets of the latest keyframe while its fate is open; empty otherwise.
  fn unjudged_keyframe_packets(&self) -> Range<u64> {
    self
      .maybe_keyframe
      .as_ref()
      .filter(|span| !span.is_judged)
      .map_or(0..0, |span| span.packets.clone())
  }

  /// Whether the latest keyframe, its fate open, lost packets of `gap` or still misses some.
  fn is_keyframe_hit(&self, gap: Range<u64>) -> bool {
    let span = self.unjudged_keyframe_packets();
    let overlaps = gap.start < span.end && span.start < gap.end;
    overlaps || self.missing.count_in(span) > 0
  }

  /// Packets were given up; `is_keyframe_lost`: some were the latest keyframe's.
  fn note_given_up(&mut self, is_keyframe_lost: bool) {
    if !is_keyframe_lost {
      return;
    }
    self.lost_keyframes = self.lost_keyframes.saturating_add(1);
    if let Some(span) = self.maybe_keyframe.as_mut() {
      span.is_judged = true;
    }
  }

  /// The latest keyframe got through once everything up to its end arrived and none of it is
  /// missing: keyframes are asked for at the usual interval again.
  fn judge_keyframe_through(&mut self) {
    let packets = self.unjudged_keyframe_packets();
    let is_past = self
      .maybe_expected
      .is_some_and(|expected| !packets.is_empty() && expected >= packets.end);
    if !is_past || self.missing.count_in(packets) > 0 {
      return;
    }
    self.lost_keyframes = 0;
    if let Some(span) = self.maybe_keyframe.as_mut() {
      span.is_judged = true;
    }
  }

  fn restart(&mut self, sequence: u64) {
    self.missing.clear();
    self.maybe_expected = Some(sequence + 1);
    self.maybe_keyframe = None;
  }

  /// The sequence number closest to the next expected one with these low 16 bits.
  fn extend(&self, sequence_number: u16) -> u64 {
    match self.maybe_expected {
      None => SEQUENCE_BASE + u64::from(sequence_number),
      Some(expected) => {
        // The low 16 bits of `expected` are its RTP sequence number.
        let offset = sequence_number.wrapping_sub(expected as u16) as i16;
        expected.saturating_add_signed(i64::from(offset))
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod requests {
    use super::*;

    #[test]
    fn a_gap_is_asked_for_at_once() {
      let (mut repair, start) = (screen(), Instant::now());
      assert_eq!(repair.on_arrival(10, None, true, start), RepairStep::default());
      let step = repair.on_arrival(13, None, true, start);
      assert_eq!(step.requests, vec![11, 12]);
      assert!(!step.needs_keyframe);
    }

    #[test]
    fn a_repaired_packet_is_no_longer_asked_for() {
      let (mut repair, start) = (screen(), Instant::now());
      repair.on_arrival(10, None, true, start);
      repair.on_arrival(12, None, true, start);
      let answered = start + Duration::from_millis(60);
      let step = repair.on_arrival(11, None, true, answered);
      assert!(step.requests.is_empty());
      assert!(!repair.is_missing_packets());
      // Nothing is asked again or given up later.
      let later = answered + Duration::from_secs(1);
      assert_eq!(repair.on_timer(true, later), RepairStep::default());
    }

    #[test]
    fn an_unanswered_request_is_repeated_after_the_round_trip() {
      let (mut repair, start) = (camera(), Instant::now());
      repair.on_arrival(10, None, true, start);
      assert_eq!(repair.on_arrival(12, None, true, start).requests, vec![11]);
      // One and a half of the assumed 100 ms round trip, plus the margin.
      assert!(
        repair
          .on_timer(true, start + Duration::from_millis(100))
          .requests
          .is_empty()
      );
      assert_eq!(
        repair.on_timer(true, start + Duration::from_millis(170)).requests,
        vec![11]
      );
    }

    #[test]
    fn numbers_wrap() {
      let (mut repair, start) = (screen(), Instant::now());
      repair.on_arrival(65_534, None, true, start);
      assert_eq!(repair.on_arrival(1, None, true, start).requests, vec![65_535, 0]);
      repair.on_arrival(0, None, true, start);
      assert!(repair.is_missing_packets());
      repair.on_arrival(65_535, None, true, start);
      assert!(!repair.is_missing_packets());
    }

    #[test]
    fn a_layer_nobody_receives_is_not_repaired() {
      let (mut repair, start) = (screen(), Instant::now());
      repair.on_arrival(10, None, false, start);
      assert_eq!(repair.on_arrival(20, None, false, start), RepairStep::default());
      assert!(!repair.is_missing_packets());
      // Someone switches to it: the numbering is known, only new gaps count.
      assert!(repair.on_arrival(21, None, true, start).requests.is_empty());
      assert_eq!(repair.on_arrival(23, None, true, start).requests, vec![22]);
      // And stops again: what was missing is forgotten.
      assert_eq!(repair.on_timer(false, start), RepairStep::default());
      assert!(!repair.is_missing_packets());
    }

    #[test]
    fn a_new_start_of_the_numbering_is_no_gap() {
      let (mut repair, start) = (screen(), Instant::now());
      repair.on_arrival(30_000, None, true, start);
      let step = repair.on_arrival(5, None, true, start);
      assert_eq!(step, RepairStep::default());
      assert_eq!(repair.on_arrival(7, None, true, start).requests, vec![6]);
    }

    #[test]
    fn nothing_is_asked_while_the_repair_is_off() {
      let start = Instant::now();
      let off = UplinkRepairConfig {
        max_requests: 0,
        ..UplinkRepairConfig::default()
      };
      for maybe_config in [None, Some(off)] {
        let mut repair = UplinkRepair::new(maybe_config, &VideoTrackKind::Screen);
        repair.on_arrival(1, None, true, start);
        assert_eq!(repair.on_arrival(5, None, true, start), RepairStep::default());
        let later = start + Duration::from_secs(1);
        assert_eq!(repair.on_timer(true, later), RepairStep::default());
      }
    }
  }

  mod keyframes {
    use super::*;

    #[test]
    fn a_packet_still_missing_after_the_bound_asks_for_a_keyframe() {
      let (mut repair, start) = (screen(), Instant::now());
      repair.on_arrival(10, None, true, start);
      repair.on_arrival(12, None, true, start);
      let before = start + Duration::from_millis(200);
      assert!(!repair.on_timer(true, before).needs_keyframe);
      let after = start + UplinkRepairConfig::default().screen_keyframe_after;
      assert!(repair.on_timer(true, after).needs_keyframe);
      assert!(!repair.is_missing_packets(), "given up");
    }

    #[test]
    fn a_camera_waits_longer_than_a_screen() {
      let start = Instant::now();
      let (mut screen, mut camera) = (screen(), camera());
      for repair in [&mut screen, &mut camera] {
        repair.on_arrival(10, None, true, start);
        repair.on_arrival(12, None, true, start);
      }
      let at = start + Duration::from_millis(350);
      assert!(screen.on_timer(true, at).needs_keyframe);
      assert!(!camera.on_timer(true, at).needs_keyframe);
      assert!(
        camera
          .on_timer(true, start + Duration::from_millis(500))
          .needs_keyframe
      );
    }

    #[test]
    fn a_burst_too_large_to_ask_for_asks_for_a_keyframe_at_once() {
      let (mut repair, start) = (screen(), Instant::now());
      repair.on_arrival(0, None, true, start);
      let step = repair.on_arrival(1000, None, true, start);
      assert!(step.needs_keyframe);
      assert!(step.requests.is_empty());
    }

    /// A lossy uplink loses a packet every 100 ms for ten seconds, and none is repaired (the
    /// publisher sends nothing again): one keyframe a second at most, not one per loss.
    #[test]
    fn a_lossy_uplink_gets_no_keyframe_storm() {
      let (mut repair, start) = (screen(), Instant::now());
      let mut sequence_number = 0u16;
      let mut keyframes = 0;
      for tick in 0..100u32 {
        let now = start + Duration::from_millis(100) * tick;
        repair.on_arrival(sequence_number, None, true, now);
        // The next one is lost.
        sequence_number = sequence_number.wrapping_add(2);
        keyframes += u32::from(repair.on_timer(true, now).needs_keyframe);
      }
      assert!((9..=10).contains(&keyframes), "{keyframes} keyframes in 10 s");
    }
  }

  /// #2246: a limited uplink whose queue can't hold a keyframe loses the tail of every one.
  mod lost_keyframes {
    use super::*;

    /// Each keyframe lost in a row doubles the wait before the next is asked for, up to the
    /// bound: the link drains what it queued instead of overflowing on the next one.
    #[test]
    fn each_lost_keyframe_doubles_the_wait_for_the_next() {
      let (mut repair, start) = (screen(), Instant::now());
      let asked = run(&mut repair, start, 100, |_| false);
      let asked_ms: Vec<u128> = asked.iter().map(Duration::as_millis).collect();
      // The first keyframe is lost at the start: asked for at once, then after 2 s (the one
      // asked for was lost too, the wait doubles to 4 s), then every 4 s.
      assert_eq!(
        asked_ms,
        vec![300, 4300, 8300],
        "keyframes asked for at {asked_ms:?} ms"
      );
    }

    /// Without the keyframes' tails lost, a lossy uplink gets its keyframe a second as before.
    #[test]
    fn lost_deltas_keep_the_usual_interval() {
      let (mut repair, start) = (screen(), Instant::now());
      let mut sequence_number = 0u16;
      let mut asked = 0;
      for tick in 0..50u32 {
        let now = start + Duration::from_millis(100) * tick;
        let fragment = (tick == 0).then_some(KeyframeFragment { index: 0, count: 1 });
        repair.on_arrival(sequence_number, fragment, true, now);
        sequence_number = sequence_number.wrapping_add(2);
        asked += u32::from(repair.on_timer(true, now).needs_keyframe);
        assert!(!repair.holds_keyframes(now));
      }
      assert!((4..=5).contains(&asked), "{asked} keyframes in 5 s");
    }

    /// While the wait runs, other requests for the layer wait too; a keyframe that gets through
    /// ends it.
    #[test]
    fn a_keyframe_that_gets_through_ends_the_wait() {
      let (mut repair, start) = (screen(), Instant::now());
      // The first keyframe asked for (at 300 ms) arrives whole; the one before it was lost.
      let asked = run(&mut repair, start, 10, |keyframe| keyframe > 0);
      assert_eq!(asked, vec![Duration::from_millis(300)]);
      assert!(!repair.holds_keyframes(start + Duration::from_millis(1000)));

      let (mut repair, start) = (screen(), Instant::now());
      run(&mut repair, start, 4, |_| false);
      assert!(repair.holds_keyframes(start + Duration::from_millis(400)));
      assert!(!repair.holds_keyframes(start + Duration::from_millis(2300)));
    }

    /// A keyframe due while the wait runs is asked for when it ends: a still screen sends nothing
    /// that would ask again.
    #[test]
    fn a_keyframe_due_meanwhile_is_asked_for_after_the_wait() {
      let (mut repair, start) = (screen(), Instant::now());
      let asked = run(&mut repair, start, 4, |_| false);
      assert_eq!(asked, vec![Duration::from_millis(300)]);
      // The publisher's keyframe (numbers 14 to 23) is lost too, one delta follows, and then the
      // screen stays still.
      let sent_at = start + Duration::from_millis(400);
      send_keyframe(&mut repair, 14, false, sent_at);
      repair.on_arrival(24, None, true, sent_at);
      let mut asked_at = None;
      for tick in 5..60u32 {
        let now = start + Duration::from_millis(100) * tick;
        if repair.on_timer(true, now).needs_keyframe {
          asked_at = Some(now.duration_since(start));
          break;
        }
      }
      assert_eq!(asked_at, Some(Duration::from_millis(4300)));
    }

    /// A publisher that sends a keyframe of ten packets and a delta every 100 ms; the keyframe
    /// asked for goes out with the next tick, whole when `arrives_whole(its number)` (the first
    /// one, sent at the start, is number 0), else without its last two packets, never resent.
    /// Returns when keyframes were asked for, since `start`.
    fn run(
      repair: &mut UplinkRepair,
      start: Instant,
      ticks: u32,
      arrives_whole: impl Fn(u32) -> bool,
    ) -> Vec<Duration> {
      let mut sequence_number = 0u16;
      let mut keyframes = 0;
      let mut is_keyframe_due = true;
      let mut asked = Vec::new();
      for tick in 0..ticks {
        let now = start + Duration::from_millis(100) * tick;
        if is_keyframe_due {
          send_keyframe(repair, sequence_number, arrives_whole(keyframes), now);
          sequence_number = sequence_number.wrapping_add(10);
          keyframes += 1;
          is_keyframe_due = false;
        }
        let step = repair.on_arrival(sequence_number, None, true, now);
        sequence_number = sequence_number.wrapping_add(1);
        let step_timer = repair.on_timer(true, now);
        if step.needs_keyframe || step_timer.needs_keyframe {
          asked.push(now.duration_since(start));
          is_keyframe_due = true;
        }
      }
      asked
    }

    /// A keyframe of ten packets numbered from `first`; without its last two unless `is_whole`.
    fn send_keyframe(repair: &mut UplinkRepair, first: u16, is_whole: bool, now: Instant) {
      let count = 10u16;
      let arriving = if is_whole { count } else { count - 2 };
      for index in 0..arriving {
        let fragment = KeyframeFragment { index, count };
        repair.on_arrival(first.wrapping_add(index), Some(fragment), true, now);
      }
    }
  }

  fn screen() -> UplinkRepair {
    UplinkRepair::new(Some(UplinkRepairConfig::default()), &VideoTrackKind::Screen)
  }

  fn camera() -> UplinkRepair {
    UplinkRepair::new(Some(UplinkRepairConfig::default()), &VideoTrackKind::Camera)
  }
}
