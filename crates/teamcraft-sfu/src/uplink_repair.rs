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
//! The SFU reads nothing but the RTP sequence number it already routes by.

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
    }
  }

  /// A packet with `sequence_number` arrived. `is_wanted`: someone receives the layer, so what's
  /// missing of it is asked for.
  pub fn on_arrival(&mut self, sequence_number: u16, is_wanted: bool, now: Instant) -> RepairStep {
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
        if is_wanted && self.missing.asks_for_packets() && !self.missing.add(expected..sequence, now) {
          // More missing than a NACK names: a burst no retransmission repairs in time.
          self.missing.clear();
          step.needs_keyframe = self.may_ask_for_keyframe(now);
        }
      },
      None => self.maybe_expected = Some(sequence + 1),
    }
    if !is_wanted {
      self.missing.clear();
      return step;
    }
    step.requests = self.due_requests(now);
    step
  }

  /// Retries and give-ups; called periodically (every ~100 ms) with whether someone still receives
  /// the layer.
  pub fn on_timer(&mut self, is_wanted: bool, now: Instant) -> RepairStep {
    let mut step = RepairStep::default();
    if !is_wanted {
      self.missing.clear();
      return step;
    }
    if self.missing.expire(now).is_some() {
      step.needs_keyframe = self.may_ask_for_keyframe(now);
    }
    step.requests = self.due_requests(now);
    step
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

  /// Whether a keyframe may be asked for now (at most once per interval), counted as asked.
  fn may_ask_for_keyframe(&mut self, now: Instant) -> bool {
    let Some(config) = self.maybe_config else {
      return false;
    };
    let is_due = self
      .maybe_keyframe_requested_at
      .is_none_or(|at| now.saturating_duration_since(at) >= config.min_keyframe_interval);
    if is_due {
      self.maybe_keyframe_requested_at = Some(now);
    }
    is_due
  }

  fn restart(&mut self, sequence: u64) {
    self.missing.clear();
    self.maybe_expected = Some(sequence + 1);
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
      assert_eq!(repair.on_arrival(10, true, start), RepairStep::default());
      let step = repair.on_arrival(13, true, start);
      assert_eq!(step.requests, vec![11, 12]);
      assert!(!step.needs_keyframe);
    }

    #[test]
    fn a_repaired_packet_is_no_longer_asked_for() {
      let (mut repair, start) = (screen(), Instant::now());
      repair.on_arrival(10, true, start);
      repair.on_arrival(12, true, start);
      let answered = start + Duration::from_millis(60);
      let step = repair.on_arrival(11, true, answered);
      assert!(step.requests.is_empty());
      assert!(!repair.is_missing_packets());
      // Nothing is asked again or given up later.
      let later = answered + Duration::from_secs(1);
      assert_eq!(repair.on_timer(true, later), RepairStep::default());
    }

    #[test]
    fn an_unanswered_request_is_repeated_after_the_round_trip() {
      let (mut repair, start) = (camera(), Instant::now());
      repair.on_arrival(10, true, start);
      assert_eq!(repair.on_arrival(12, true, start).requests, vec![11]);
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
      repair.on_arrival(65_534, true, start);
      assert_eq!(repair.on_arrival(1, true, start).requests, vec![65_535, 0]);
      repair.on_arrival(0, true, start);
      assert!(repair.is_missing_packets());
      repair.on_arrival(65_535, true, start);
      assert!(!repair.is_missing_packets());
    }

    #[test]
    fn a_layer_nobody_receives_is_not_repaired() {
      let (mut repair, start) = (screen(), Instant::now());
      repair.on_arrival(10, false, start);
      assert_eq!(repair.on_arrival(20, false, start), RepairStep::default());
      assert!(!repair.is_missing_packets());
      // Someone switches to it: the numbering is known, only new gaps count.
      assert!(repair.on_arrival(21, true, start).requests.is_empty());
      assert_eq!(repair.on_arrival(23, true, start).requests, vec![22]);
      // And stops again: what was missing is forgotten.
      assert_eq!(repair.on_timer(false, start), RepairStep::default());
      assert!(!repair.is_missing_packets());
    }

    #[test]
    fn a_new_start_of_the_numbering_is_no_gap() {
      let (mut repair, start) = (screen(), Instant::now());
      repair.on_arrival(30_000, true, start);
      let step = repair.on_arrival(5, true, start);
      assert_eq!(step, RepairStep::default());
      assert_eq!(repair.on_arrival(7, true, start).requests, vec![6]);
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
        repair.on_arrival(1, true, start);
        assert_eq!(repair.on_arrival(5, true, start), RepairStep::default());
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
      repair.on_arrival(10, true, start);
      repair.on_arrival(12, true, start);
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
        repair.on_arrival(10, true, start);
        repair.on_arrival(12, true, start);
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
      repair.on_arrival(0, true, start);
      let step = repair.on_arrival(1000, true, start);
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
        repair.on_arrival(sequence_number, true, now);
        // The next one is lost.
        sequence_number = sequence_number.wrapping_add(2);
        keyframes += u32::from(repair.on_timer(true, now).needs_keyframe);
      }
      assert!((9..=10).contains(&keyframes), "{keyframes} keyframes in 10 s");
    }
  }

  fn screen() -> UplinkRepair {
    UplinkRepair::new(Some(UplinkRepairConfig::default()), &VideoTrackKind::Screen)
  }

  fn camera() -> UplinkRepair {
    UplinkRepair::new(Some(UplinkRepairConfig::default()), &VideoTrackKind::Camera)
  }
}
