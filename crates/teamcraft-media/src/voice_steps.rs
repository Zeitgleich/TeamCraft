//! The own voice on a thin uplink (#1893): which Opus bitrate and packet length it is sent at.
//!
//! A voice at 32 kbit/s in 20 ms packets costs about 70 kbit/s on the wire: at 50 packets a second
//! the headers (IP, UDP, QUIC, datagram, RTP, DAVE) weigh more than the speech. A phone on a link of
//! 64 kbit/s can't send that. So the voice steps down as the uplink estimate falls: to a lower
//! bitrate in 40 ms packets, then lower still in 60 ms packets, which halves and thirds the
//! headers. It steps down at once, and back up one step at a time once the estimate has stayed
//! above the step's bound with a margin for a while ([`VoiceStepConfig::upgrade_after`]); an
//! upgrade the link didn't carry doubles that wait (up to [`VoiceStepConfig::max_upgrade_after`]),
//! so a link just too thin for the next step isn't tried every few seconds.
//!
//! Voice the receivers lose or get late ([`crate::voice_guard`]) steps down too, one step at a
//! time, without waiting for the estimate.
//!
//! Packets longer than 20 ms are only sent while every other participant plays them
//! ([`teamcraft_proto::AudioFeature::LongFrames`]); otherwise the steps keep their bitrates in
//! 20 ms packets.
//!
//! Pure: time is passed in.

use std::time::Duration;

// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::audio_frame::FRAME_SAMPLES_PER_CHANNEL;

/// What the own voice is sent at now, and when it changed last.
#[derive(Debug)]
pub struct VoiceSteps {
  config: VoiceStepConfig,
  step: VoiceStep,
  has_long_frames: bool,
  /// Since when the estimate has stayed high enough for the next step up.
  maybe_better_since: Option<Instant>,
  /// The wait before the next step up; doubles with each upgrade the link didn't carry.
  upgrade_after: Duration,
  maybe_upgraded_at: Option<Instant>,
  maybe_changed_at: Option<Instant>,
}

/// Settings of the voice steps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceStepConfig {
  /// The voice on a link with room: the call's default.
  pub full: VoiceEncoding,
  pub thin: VoiceEncoding,
  pub thinnest: VoiceEncoding,
  /// Below this uplink estimate the voice is sent [`Self::thin`].
  pub thin_below_bps: u32,
  /// Below this uplink estimate the voice is sent [`Self::thinnest`].
  pub thinnest_below_bps: u32,
  /// A step up needs an estimate this many times its bound.
  pub upgrade_margin: f64,
  /// ... for this long.
  pub upgrade_after: Duration,
  /// The longest the wait grows to after upgrades the link didn't carry.
  pub max_upgrade_after: Duration,
  /// A step down within this long after a step up shows the link didn't carry it.
  pub failed_upgrade_within: Duration,
  /// Lost voice steps down again at most this often: what was sent before the last step down
  /// is still on the way.
  pub hurt_step_interval: Duration,
}

/// How the voice is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceEncoding {
  pub bitrate_bps: u32,
  pub frame: VoiceFrame,
}

/// The length of the voice's Opus packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VoiceFrame {
  Ms20,
  Ms40,
  Ms60,
}

/// The voice's steps, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VoiceStep {
  Thinnest,
  Thin,
  Full,
}

impl Default for VoiceStepConfig {
  fn default() -> Self {
    Self {
      full: VoiceEncoding {
        bitrate_bps: 32_000,
        frame: VoiceFrame::Ms20,
      },
      thin: VoiceEncoding {
        bitrate_bps: 16_000,
        frame: VoiceFrame::Ms40,
      },
      thinnest: VoiceEncoding {
        bitrate_bps: 12_000,
        frame: VoiceFrame::Ms60,
      },
      thin_below_bps: 140_000,
      thinnest_below_bps: 60_000,
      upgrade_margin: 1.25,
      upgrade_after: Duration::from_secs(5),
      max_upgrade_after: Duration::from_secs(80),
      failed_upgrade_within: Duration::from_secs(15),
      hurt_step_interval: Duration::from_secs(1),
    }
  }
}

impl VoiceSteps {
  pub fn new(config: VoiceStepConfig) -> Self {
    Self {
      config,
      step: VoiceStep::Full,
      has_long_frames: false,
      maybe_better_since: None,
      upgrade_after: config.upgrade_after,
      maybe_upgraded_at: None,
      maybe_changed_at: None,
    }
  }

  /// Other settings; the step stays where it is.
  pub fn set_config(&mut self, config: VoiceStepConfig) {
    self.config = config;
    self.upgrade_after = config.upgrade_after;
  }

  /// Another network or connection: its room is unknown, the voice starts full again (as the
  /// estimate starts afresh). Returns the new encoding when it changed.
  pub fn start_afresh(&mut self) -> Option<VoiceEncoding> {
    let before = self.encoding();
    self.step = VoiceStep::Full;
    self.maybe_better_since = None;
    self.upgrade_after = self.config.upgrade_after;
    self.maybe_upgraded_at = None;
    self.maybe_changed_at = None;
    let after = self.encoding();
    (after != before).then_some(after)
  }

  /// How the voice is to be encoded now.
  pub fn encoding(&self) -> VoiceEncoding {
    let encoding = match self.step {
      VoiceStep::Full => self.config.full,
      VoiceStep::Thin => self.config.thin,
      VoiceStep::Thinnest => self.config.thinnest,
    };
    if self.has_long_frames {
      encoding
    } else {
      VoiceEncoding {
        frame: VoiceFrame::Ms20,
        ..encoding
      }
    }
  }

  pub fn step(&self) -> VoiceStep {
    self.step
  }

  /// What the uplink has to carry for the next step up to be tried, while the voice is below its
  /// full step: the uplink probes for it ([`crate::rate_probe`]).
  pub fn wanted_bps(&self) -> Option<u32> {
    let bound = match self.step {
      VoiceStep::Full => return None,
      VoiceStep::Thin => self.config.thin_below_bps,
      VoiceStep::Thinnest => self.config.thinnest_below_bps,
    };
    // Saturating float-to-int conversion is intended.
    Some((f64::from(bound) * self.config.upgrade_margin * 1.1) as u32)
  }

  /// Whether every other participant plays packets longer than 20 ms. Returns the new encoding
  /// when it changed.
  pub fn set_long_frames(&mut self, has_long_frames: bool) -> Option<VoiceEncoding> {
    let before = self.encoding();
    self.has_long_frames = has_long_frames;
    let after = self.encoding();
    (after != before).then_some(after)
  }

  /// Follows the uplink estimate and whether the own voice is being lost on the way. Returns the
  /// new encoding when it changed.
  pub fn on_estimate(
    &mut self,
    estimate_bps: u32,
    is_voice_hurt: bool,
    now: Instant,
  ) -> Option<VoiceEncoding> {
    let before = self.encoding();
    let mut target = self.step_for(f64::from(estimate_bps));
    if is_voice_hurt && self.may_step_down_for_hurt(now) {
      target = target.min(self.step.lower());
    }
    if target < self.step {
      self.step_down(target, now);
    } else if is_voice_hurt {
      // Not up while voice is being lost, whatever the estimate says.
      self.maybe_better_since = None;
    } else {
      self.maybe_step_up(estimate_bps, now);
    }
    let after = self.encoding();
    (after != before).then_some(after)
  }

  /// The step an estimate of `estimate_bps` carries.
  fn step_for(&self, estimate_bps: f64) -> VoiceStep {
    if estimate_bps < f64::from(self.config.thinnest_below_bps) {
      VoiceStep::Thinnest
    } else if estimate_bps < f64::from(self.config.thin_below_bps) {
      VoiceStep::Thin
    } else {
      VoiceStep::Full
    }
  }

  fn may_step_down_for_hurt(&self, now: Instant) -> bool {
    self
      .maybe_changed_at
      .is_none_or(|changed_at| now.saturating_duration_since(changed_at) >= self.config.hurt_step_interval)
  }

  fn step_down(&mut self, target: VoiceStep, now: Instant) {
    if let Some(upgraded_at) = self.maybe_upgraded_at.take()
      && now.saturating_duration_since(upgraded_at) < self.config.failed_upgrade_within
    {
      self.upgrade_after = (self.upgrade_after * 2).min(self.config.max_upgrade_after);
    }
    self.step = target;
    self.maybe_better_since = None;
    self.maybe_changed_at = Some(now);
  }

  fn maybe_step_up(&mut self, estimate_bps: u32, now: Instant) {
    if let Some(upgraded_at) = self.maybe_upgraded_at
      && now.saturating_duration_since(upgraded_at) >= self.config.failed_upgrade_within
    {
      // The last upgrade held: the next one waits the least again.
      self.maybe_upgraded_at = None;
      self.upgrade_after = self.config.upgrade_after;
    }
    let with_margin = self.step_for(f64::from(estimate_bps) / self.config.upgrade_margin);
    if with_margin <= self.step {
      self.maybe_better_since = None;
      return;
    }
    let better_since = *self.maybe_better_since.get_or_insert(now);
    if now.saturating_duration_since(better_since) < self.upgrade_after {
      return;
    }
    self.step = self.step.higher();
    self.maybe_better_since = None;
    self.maybe_upgraded_at = Some(now);
    self.maybe_changed_at = Some(now);
  }
}

impl VoiceFrame {
  /// Samples per channel at 48 kHz.
  pub fn samples(self) -> usize {
    FRAME_SAMPLES_PER_CHANNEL
      * match self {
        Self::Ms20 => 1,
        Self::Ms40 => 2,
        Self::Ms60 => 3,
      }
  }

  pub fn duration(self) -> Duration {
    Duration::from_millis(20 * self.samples() as u64 / FRAME_SAMPLES_PER_CHANNEL as u64)
  }
}

impl VoiceStep {
  fn lower(self) -> Self {
    match self {
      Self::Full => Self::Thin,
      Self::Thin | Self::Thinnest => Self::Thinnest,
    }
  }

  fn higher(self) -> Self {
    match self {
      Self::Thinnest => Self::Thin,
      Self::Thin | Self::Full => Self::Full,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const GOOD_BPS: u32 = 1_000_000;
  /// Between the thin and the thinnest bound.
  const THIN_BPS: u32 = 100_000;
  const THINNEST_BPS: u32 = 40_000;

  #[test]
  fn a_link_with_room_keeps_the_full_voice() {
    let mut steps = steps(true);
    let start = Instant::now();
    for tick in 0..100 {
      assert_eq!(steps.on_estimate(GOOD_BPS, false, at(start, tick)), None);
    }
    assert_eq!(steps.encoding(), VoiceStepConfig::default().full);
    assert_eq!(steps.wanted_bps(), None);
  }

  #[test]
  fn a_falling_estimate_steps_down_at_once() {
    let mut steps = steps(true);
    let start = Instant::now();
    assert_eq!(
      steps.on_estimate(THIN_BPS, false, start),
      Some(VoiceStepConfig::default().thin)
    );
    assert_eq!(
      steps.on_estimate(THINNEST_BPS, false, at(start, 1)),
      Some(VoiceStepConfig::default().thinnest)
    );
    assert_eq!(steps.encoding().frame, VoiceFrame::Ms60);
    // A collapse goes straight to the bottom.
    let mut collapsed = self::steps(true);
    collapsed.on_estimate(THINNEST_BPS, false, start);
    assert_eq!(collapsed.step(), VoiceStep::Thinnest);
  }

  #[test]
  fn a_recovered_link_steps_up_one_step_at_a_time_after_the_wait() {
    let config = VoiceStepConfig::default();
    let mut steps = steps(true);
    let start = Instant::now();
    steps.on_estimate(THINNEST_BPS, false, start);
    // Just above the bound is not enough: the margin is missing.
    let just_above = config.thinnest_below_bps + 1;
    for tick in 1..=100 {
      steps.on_estimate(just_above, false, at(start, tick));
    }
    assert_eq!(steps.step(), VoiceStep::Thinnest);
    let room = 10 * config.thin_below_bps;
    let since = at(start, 101);
    steps.on_estimate(room, false, since);
    assert_eq!(
      steps.on_estimate(room, false, since + config.upgrade_after / 2),
      None
    );
    assert_eq!(
      steps.on_estimate(room, false, since + config.upgrade_after),
      Some(config.thin),
      "one step, not straight to the top"
    );
    let since = since + config.upgrade_after + Duration::from_millis(100);
    steps.on_estimate(room, false, since);
    assert_eq!(
      steps.on_estimate(room, false, since + config.upgrade_after),
      Some(config.full)
    );
  }

  #[test]
  fn a_dip_during_the_wait_starts_it_again() {
    let config = VoiceStepConfig::default();
    let mut steps = steps(true);
    let start = Instant::now();
    steps.on_estimate(THIN_BPS, false, start);
    steps.on_estimate(GOOD_BPS, false, at(start, 1));
    steps.on_estimate(THIN_BPS, false, at(start, 30));
    steps.on_estimate(GOOD_BPS, false, at(start, 31));
    assert_eq!(
      steps.on_estimate(GOOD_BPS, false, at(start, 1) + config.upgrade_after),
      None
    );
    assert!(
      steps
        .on_estimate(GOOD_BPS, false, at(start, 31) + config.upgrade_after)
        .is_some()
    );
  }

  /// A link just too thin for the next step: each failed try doubles the wait, and an upgrade that
  /// holds brings the short wait back.
  #[test]
  fn upgrades_the_link_didnt_carry_wait_longer_each_time() {
    let config = VoiceStepConfig::default();
    let mut steps = steps(true);
    let mut now = Instant::now();
    steps.on_estimate(THIN_BPS, false, now);
    let mut waits = Vec::new();
    for _ in 0..6 {
      let since = now + Duration::from_millis(100);
      steps.on_estimate(GOOD_BPS, false, since);
      now = since;
      while steps.step() != VoiceStep::Full {
        now += Duration::from_millis(100);
        steps.on_estimate(GOOD_BPS, false, now);
      }
      waits.push(now.duration_since(since));
      // The full voice doesn't fit after all.
      now += Duration::from_secs(2);
      steps.on_estimate(THIN_BPS, false, now);
    }
    let secs: Vec<u64> = waits.iter().map(Duration::as_secs).collect();
    assert_eq!(secs, vec![5, 10, 20, 40, 80, 80]);
    // This time it holds; after the next dip the wait is short again.
    let since = now + Duration::from_millis(100);
    steps.on_estimate(GOOD_BPS, false, since);
    now = since + config.max_upgrade_after;
    steps.on_estimate(GOOD_BPS, false, now);
    assert_eq!(steps.step(), VoiceStep::Full);
    now += config.failed_upgrade_within;
    steps.on_estimate(GOOD_BPS, false, now);
    steps.on_estimate(THIN_BPS, false, now + Duration::from_millis(100));
    let since = now + Duration::from_millis(200);
    steps.on_estimate(GOOD_BPS, false, since);
    assert!(
      steps
        .on_estimate(GOOD_BPS, false, since + config.upgrade_after)
        .is_some()
    );
  }

  /// Lost voice steps down before the estimate says anything, a step at a time.
  #[test]
  fn lost_voice_steps_down_without_the_estimate() {
    let config = VoiceStepConfig::default();
    let mut steps = steps(true);
    let start = Instant::now();
    assert_eq!(steps.on_estimate(GOOD_BPS, true, start), Some(config.thin));
    assert_eq!(
      steps.on_estimate(GOOD_BPS, true, start + config.hurt_step_interval / 2),
      None,
      "what was sent before is still on the way"
    );
    assert_eq!(
      steps.on_estimate(GOOD_BPS, true, start + config.hurt_step_interval),
      Some(config.thinnest)
    );
    assert_eq!(steps.on_estimate(GOOD_BPS, true, at(start, 100)), None);
    assert_eq!(steps.step(), VoiceStep::Thinnest, "not up while it is being lost");
    let since = at(start, 101);
    steps.on_estimate(GOOD_BPS, false, since);
    assert!(
      steps
        .on_estimate(GOOD_BPS, false, since + config.upgrade_after)
        .is_some()
    );
  }

  /// Without every other participant playing long packets, the steps keep their bitrates in
  /// 20 ms packets; the long ones come as soon as everyone plays them, and go when someone
  /// joins who doesn't.
  #[test]
  fn long_packets_only_where_everyone_plays_them() {
    let config = VoiceStepConfig::default();
    let mut steps = steps(false);
    let start = Instant::now();
    assert_eq!(
      steps.on_estimate(THINNEST_BPS, false, start),
      Some(VoiceEncoding {
        bitrate_bps: config.thinnest.bitrate_bps,
        frame: VoiceFrame::Ms20,
      })
    );
    assert_eq!(steps.set_long_frames(true), Some(config.thinnest));
    assert_eq!(steps.set_long_frames(true), None);
    assert_eq!(
      steps.set_long_frames(false).map(|encoding| encoding.frame),
      Some(VoiceFrame::Ms20)
    );
    // On the full step nothing changes either way.
    let mut full = self::steps(false);
    assert_eq!(full.set_long_frames(true), None);
  }

  #[test]
  fn the_uplink_is_probed_for_the_next_step() {
    let config = VoiceStepConfig::default();
    let mut steps = steps(true);
    steps.on_estimate(THINNEST_BPS, false, Instant::now());
    let wanted = steps.wanted_bps().unwrap();
    assert!(f64::from(wanted) >= f64::from(config.thinnest_below_bps) * config.upgrade_margin);
    assert!(wanted < config.thin_below_bps);
  }

  #[test]
  fn another_network_starts_full() {
    let mut steps = steps(true);
    steps.on_estimate(THINNEST_BPS, false, Instant::now());
    assert_eq!(steps.start_afresh(), Some(VoiceStepConfig::default().full));
    assert_eq!(steps.start_afresh(), None);
  }

  #[test]
  fn frames_last_what_they_say() {
    assert_eq!(VoiceFrame::Ms20.samples(), 960);
    assert_eq!(VoiceFrame::Ms60.samples(), 2880);
    assert_eq!(VoiceFrame::Ms40.duration(), Duration::from_millis(40));
  }

  fn steps(has_long_frames: bool) -> VoiceSteps {
    let mut steps = VoiceSteps::new(VoiceStepConfig::default());
    steps.set_long_frames(has_long_frames);
    steps
  }

  /// `tick` feedback intervals of 100 ms after `start`.
  fn at(start: Instant, tick: u64) -> Instant {
    start + Duration::from_millis(100 * tick)
  }
}
