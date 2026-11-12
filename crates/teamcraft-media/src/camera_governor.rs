//! Keeps an own camera within what its encoders and the machine manage (#1762).
//!
//! Two controllers, pure policy on a clock the caller passes in:
//!
//! - [`LayerRateGuard`]: a layer whose encoder sends far more than the layer's bitrate. Chrome's
//!   WebCodecs H.264 encoder (OpenH264) runs its rate control without frame skipping, and its
//!   quantizer has a ceiling: on hard content (sensor noise, a busy scene) a 320×180 layer's
//!   target of 150 kbit/s leaves about 600 bytes per frame at 30 fps, which no picture fits in. It
//!   sent 0.8 to 1.6 Mbit/s, and on a 3 Mbit/s link that layer alone filled half of it. The guard
//!   measures each layer's output: past [`CameraGovernorConfig::overshoot_factor`] times its
//!   bitrate over a whole window, the encoder is configured at a lower bitrate first (an encoder
//!   that is merely lax follows it), then the layer gets fewer frames (one that can't shrink a
//!   picture any further sends fewer of them).
//! - [`CameraLoad`]: a camera whose encoders don't keep up (frames queue in them, or are
//!   skipped because the queue is full) is captured at a lower frame rate first, then sends
//!   fewer layers, the largest first. Its judge, [`EncoderLoad`], only moves a level index, so
//!   another ladder (a screen share's, #1733) can use it with its own steps; it also takes the
//!   encoders' time per frame against the frame interval where the driver can measure it.
//!
//! Both step back up in the reverse order once it stayed calm for
//! [`CameraGovernorConfig::calm_period`]; a step up that is undone within
//! [`CameraGovernorConfig::probe_proof`] doubles the wait for the next one, up to
//! [`CameraGovernorConfig::max_calm_period`]. A step changes how the encoders are configured,
//! which may cost a keyframe, so they are few and far between.
//!
//! Screen shares keep their own policy (#1733): nothing here is applied to them, though
//! [`EncoderLoad`] is built to be reused there.

use std::collections::VecDeque;
use std::time::Duration;

// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

/// The frame-rate divisors a layer whose encoder overshoots steps through: every second, third
/// and sixth frame (15, 10 and 5 fps of a 30 fps camera).
const LAYER_FRAME_DIVISORS: [u8; 3] = [2, 3, 6];

/// Limits and timings of the camera governor. Every value has a default for a call; the driver
/// may set others.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraGovernorConfig {
  /// How long a layer's output is measured before it is judged, and again after each step.
  pub rate_window: Duration,
  /// A layer that sends more than this many times its bitrate over [`Self::rate_window`]
  /// overshoots. Keyframes and a scene cut stay below it; an encoder that ignores its target
  /// doesn't.
  pub overshoot_factor: f64,
  /// A limited layer steps up once its output after the step (estimated from the output now)
  /// stays below this many times its bitrate. Below [`Self::overshoot_factor`], so a step up
  /// doesn't overshoot at once.
  pub recover_factor: f64,
  /// What a bitrate step configures the encoder at, as a share of the layer's bitrate.
  pub bitrate_cut: f64,
  /// Bitrate steps before the layer gets fewer frames.
  pub max_bitrate_cuts: u8,
  /// A layer is never given fewer frames per second than this.
  pub min_layer_framerate: u8,
  /// How long the encoders' load is measured before it is judged, and again after each step.
  pub load_window: Duration,
  /// Fewer frames than this given to the encoders in a load window say nothing (the camera
  /// just started, or nothing is encoded).
  pub min_load_frames: u32,
  /// The encoders are overloaded when more than this share of the frames found an encoder's
  /// queue full.
  pub overload_skip_ratio: f64,
  /// ...or when the frames found this many frames in an encoder's queue on average: an encoder
  /// that keeps up has taken the previous one by the time the next arrives.
  pub overload_queue_depth: f64,
  /// The encoders are calm when no frame was skipped and the queue held at most this many on
  /// average.
  pub calm_queue_depth: f64,
  /// ...or overloaded when encoding took more than this share of the frame interval, where the
  /// driver measures it ([`FrameTally::encode_time`]).
  pub overload_encode_share: f64,
  /// Calm needs encoding to take at most this share of the frame interval, where measured.
  pub calm_encode_share: f64,
  /// The camera is never captured at fewer frames per second than this for the encoders' sake.
  pub min_capture_framerate: u8,
  /// How long a limit has to stay calm before it steps back up.
  pub calm_period: Duration,
  /// The longest wait for a step up after step ups that didn't hold.
  pub max_calm_period: Duration,
  /// A step up that held this long counts as carried; one undone sooner doubles the wait.
  pub probe_proof: Duration,
}

/// What a layer's encoder is held to besides the layer's bitrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerLimit {
  /// Bitrate steps taken: the encoder runs at [`CameraGovernorConfig::bitrate_cut`] to this power.
  pub bitrate_cuts: u8,
  /// The layer gets every this many-th frame (1: every frame).
  pub frame_divisor: u8,
}

/// One layer's output against its bitrate.
#[derive(Debug, Clone)]
pub struct LayerRateGuard {
  level: u8,
  output: OutputWindow,
  maybe_calm_since: Option<Instant>,
  probes: Probes,
}

/// How much of the camera's plan its encoders get: a frame rate and the layers left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadStep {
  pub framerate: u8,
  /// The top layers not encoded.
  pub top_layers_off: u8,
}

/// Why the encoders were last found overloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverloadReason {
  /// Frames found an encoder's queue full and were skipped.
  SkippedFrames,
  /// Frames waited in the encoders' queues.
  QueuedFrames,
  /// Encoding took most of the frame interval.
  SlowEncoding,
}

/// A camera's encoders against the frames they get, on the camera's ladder of [`LoadStep`]s.
#[derive(Debug, Clone)]
pub struct CameraLoad {
  ladder: Vec<LoadStep>,
  load: EncoderLoad,
}

/// Encoders against the frames they get, on a ladder of `level_count` levels: 0 is the plan,
/// each higher one less work. The caller decides what a level means.
#[derive(Debug, Clone)]
pub struct EncoderLoad {
  level_count: usize,
  level: usize,
  window: LoadWindow,
  maybe_calm_since: Option<Instant>,
  probes: Probes,
  maybe_reason: Option<OverloadReason>,
}

/// One frame given to the encoders, as they took it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameTally {
  /// Layers whose encoder took the frame.
  pub given: u32,
  /// Layers whose encoder's queue was full: the frame was skipped there.
  pub skipped: u32,
  /// Frames already waiting in the encoders that took it, summed over them.
  pub queued: u32,
  /// How long encoding took, summed over the frames measured; zero where the driver can't tell
  /// (WebCodecs encodes out of sight).
  pub encode_time: Duration,
  /// The frame interval of the frames whose [`Self::encode_time`] is summed.
  pub frame_interval: Duration,
}

/// What a judgement found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
  Over,
  Calm,
  Neither,
}

/// Bytes a layer sent, over a sliding window.
#[derive(Debug, Clone)]
struct OutputWindow {
  samples: VecDeque<(Instant, usize)>,
  /// Measuring since: a judgement needs a whole window.
  since: Instant,
}

/// Frames given to the encoders since the window started.
#[derive(Debug, Clone, Copy)]
struct LoadWindow {
  since: Instant,
  tally: FrameTally,
}

/// The step-up probes of one controller: how long it waits before the next.
#[derive(Debug, Clone, Copy)]
struct Probes {
  calm_needed: Duration,
  maybe_stepped_up_at: Option<Instant>,
}

impl Default for CameraGovernorConfig {
  fn default() -> Self {
    Self {
      rate_window: Duration::from_secs(2),
      overshoot_factor: 2.0,
      recover_factor: 1.25,
      bitrate_cut: 0.5,
      max_bitrate_cuts: 1,
      min_layer_framerate: 5,
      load_window: Duration::from_secs(2),
      min_load_frames: 10,
      overload_skip_ratio: 0.1,
      overload_queue_depth: 1.0,
      calm_queue_depth: 0.25,
      overload_encode_share: 0.9,
      calm_encode_share: 0.5,
      min_capture_framerate: 10,
      calm_period: Duration::from_secs(10),
      max_calm_period: Duration::from_secs(120),
      probe_proof: Duration::from_secs(10),
    }
  }
}

impl LayerLimit {
  pub const NONE: Self = Self {
    bitrate_cuts: 0,
    frame_divisor: 1,
  };

  pub fn is_limited(self) -> bool {
    self != Self::NONE
  }

  /// What the encoder of a layer of `budget_bps` is configured at.
  pub fn bitrate_bps(self, budget_bps: u32, config: &CameraGovernorConfig) -> u32 {
    let share = config
      .bitrate_cut
      .clamp(0.05, 1.0)
      .powi(i32::from(self.bitrate_cuts));
    // Invariant: a share of at most 1.0 of a u32 fits a u32.
    (f64::from(budget_bps) * share).round().max(1.0) as u32
  }

  /// The layer's frame rate when the camera is captured at `framerate`.
  pub fn framerate(self, framerate: u8) -> u8 {
    (framerate / self.frame_divisor.max(1)).max(1)
  }
}

impl LayerRateGuard {
  pub fn new(now: Instant) -> Self {
    Self {
      level: 0,
      output: OutputWindow::new(now),
      maybe_calm_since: None,
      probes: Probes::new(),
    }
  }

  /// The limit the layer runs at now, with the camera captured at `framerate`.
  pub fn limit(&self, framerate: u8, config: &CameraGovernorConfig) -> LayerLimit {
    limit_at(self.level, framerate, config)
  }

  /// The layer's encoder gave a picture of `bytes`.
  pub fn on_output(&mut self, bytes: usize, now: Instant, config: &CameraGovernorConfig) {
    self.output.push(bytes, now, config.rate_window);
  }

  /// Measures afresh from `now`, keeping the limit: the layer paused, resumed or got a new
  /// encoder, and what it sent before says nothing about it now.
  pub fn measure_afresh(&mut self, now: Instant) {
    self.output.reset(now);
    self.maybe_calm_since = None;
  }

  /// Judges the layer's output against its bitrate `budget_bps`; the new limit when it changed.
  pub fn evaluate(
    &mut self,
    budget_bps: u32,
    framerate: u8,
    now: Instant,
    config: &CameraGovernorConfig,
  ) -> Option<LayerLimit> {
    self.probes.note_held(now, config);
    // A lower capture frame rate leaves fewer frame steps (none below the layer's floor).
    self.level = self.level.min(max_level(framerate, config));
    let rate_bps = self.output.rate_bps(now, config.rate_window)?;
    let budget = f64::from(budget_bps.max(1));
    let verdict = if rate_bps > budget * config.overshoot_factor {
      Verdict::Over
    } else if self.level > 0
      && rate_bps * self.step_up_growth(framerate, config) <= budget * config.recover_factor
    {
      Verdict::Calm
    } else {
      Verdict::Neither
    };
    let can_step_down = self.level < max_level(framerate, config);
    let step = decide(
      verdict,
      can_step_down,
      &mut self.maybe_calm_since,
      &mut self.probes,
      now,
      config,
    )?;
    self.level = match step {
      Step::Down => self.level + 1,
      Step::Up => self.level - 1,
    };
    self.output.reset(now);
    Some(self.limit(framerate, config))
  }

  /// How much more the layer would send one step up: the frames it would get again, or the
  /// bitrate the encoder would be configured at again.
  fn step_up_growth(&self, framerate: u8, config: &CameraGovernorConfig) -> f64 {
    let now = limit_at(self.level, framerate, config);
    let above = limit_at(self.level.saturating_sub(1), framerate, config);
    if now.frame_divisor != above.frame_divisor {
      f64::from(now.frame_divisor) / f64::from(above.frame_divisor.max(1))
    } else {
      1.0 / config.bitrate_cut.clamp(0.05, 1.0)
    }
  }
}

impl CameraLoad {
  /// A camera planned at `framerate` with `layer_count` layers, all of them encoded.
  pub fn new(framerate: u8, layer_count: u8, now: Instant, config: &CameraGovernorConfig) -> Self {
    let ladder = load_ladder(framerate, layer_count, config);
    let load = EncoderLoad::with_levels(ladder.len(), now);
    Self { ladder, load }
  }

  /// What the encoders get now.
  pub fn step(&self) -> LoadStep {
    // Invariant: the ladder has its first step, the plan, and the level never leaves it.
    self.ladder[self.load.level()]
  }

  /// The plan: the frame rate the camera started with, every layer.
  pub fn planned(&self) -> LoadStep {
    self.ladder[0]
  }

  /// Why the encoders were last found overloaded, while the camera runs below its plan.
  pub fn maybe_reason(&self) -> Option<OverloadReason> {
    self.load.maybe_reason()
  }

  /// A frame was given to the encoders.
  pub fn on_frame(&mut self, tally: FrameTally) {
    self.load.on_frame(tally);
  }

  /// Measures afresh from `now`: the camera paused, or its encoders were replaced.
  pub fn measure_afresh(&mut self, now: Instant) {
    self.load.measure_afresh(now);
  }

  /// Judges the window once it is whole; the new step when it changed.
  pub fn evaluate(&mut self, now: Instant, config: &CameraGovernorConfig) -> Option<LoadStep> {
    self.load.evaluate(now, config).map(|_| self.step())
  }
}

impl EncoderLoad {
  /// A ladder of `level_count` levels (at least the plan), starting at the plan.
  pub fn with_levels(level_count: usize, now: Instant) -> Self {
    Self {
      level_count: level_count.max(1),
      level: 0,
      window: LoadWindow::new(now),
      maybe_calm_since: None,
      probes: Probes::new(),
      maybe_reason: None,
    }
  }

  /// The level now: 0 is the plan.
  pub fn level(&self) -> usize {
    self.level
  }

  /// Why the encoders were last found overloaded, while below the plan.
  pub fn maybe_reason(&self) -> Option<OverloadReason> {
    self.maybe_reason.filter(|_| self.level > 0)
  }

  /// A frame was given to the encoders.
  pub fn on_frame(&mut self, tally: FrameTally) {
    self.window.add(tally);
  }

  /// Measures afresh from `now`: the encoders paused or were replaced.
  pub fn measure_afresh(&mut self, now: Instant) {
    self.window = LoadWindow::new(now);
    self.maybe_calm_since = None;
  }

  /// Judges the window once it is whole; the new level when it changed.
  pub fn evaluate(&mut self, now: Instant, config: &CameraGovernorConfig) -> Option<usize> {
    self.probes.note_held(now, config);
    if now.saturating_duration_since(self.window.since) < config.load_window {
      return None;
    }
    let tally = std::mem::replace(&mut self.window, LoadWindow::new(now)).tally;
    let offered = tally.given + tally.skipped;
    if offered < config.min_load_frames {
      return None;
    }
    let skip_ratio = f64::from(tally.skipped) / f64::from(offered);
    let queue_depth = f64::from(tally.queued) / f64::from(tally.given.max(1));
    let maybe_encode_share = (!tally.frame_interval.is_zero())
      .then(|| tally.encode_time.as_secs_f64() / tally.frame_interval.as_secs_f64());
    let maybe_reason = if skip_ratio > config.overload_skip_ratio {
      Some(OverloadReason::SkippedFrames)
    } else if queue_depth >= config.overload_queue_depth {
      Some(OverloadReason::QueuedFrames)
    } else if maybe_encode_share.is_some_and(|share| share > config.overload_encode_share) {
      Some(OverloadReason::SlowEncoding)
    } else {
      None
    };
    let is_calm = tally.skipped == 0
      && queue_depth <= config.calm_queue_depth
      && maybe_encode_share.is_none_or(|share| share <= config.calm_encode_share);
    let verdict = match maybe_reason {
      Some(_) => Verdict::Over,
      None if self.level > 0 && is_calm => Verdict::Calm,
      None => Verdict::Neither,
    };
    let can_step_down = self.level + 1 < self.level_count;
    let step = decide(
      verdict,
      can_step_down,
      &mut self.maybe_calm_since,
      &mut self.probes,
      now,
      config,
    )?;
    match step {
      Step::Down => {
        self.level += 1;
        self.maybe_reason = maybe_reason;
      },
      Step::Up => self.level -= 1,
    }
    Some(self.level)
  }
}

/// A controller's move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
  Down,
  Up,
}

/// The move a controller makes on `verdict`: down at once when it can, up after a calm period.
fn decide(
  verdict: Verdict,
  can_step_down: bool,
  maybe_calm_since: &mut Option<Instant>,
  probes: &mut Probes,
  now: Instant,
  config: &CameraGovernorConfig,
) -> Option<Step> {
  match verdict {
    Verdict::Over => {
      *maybe_calm_since = None;
      if !can_step_down {
        return None;
      }
      probes.on_step_down(now, config);
      Some(Step::Down)
    },
    Verdict::Calm => {
      let calm_since = *maybe_calm_since.get_or_insert(now);
      if now.saturating_duration_since(calm_since) < probes.calm_needed(config) {
        return None;
      }
      *maybe_calm_since = None;
      probes.on_step_up(now);
      Some(Step::Up)
    },
    Verdict::Neither => {
      *maybe_calm_since = None;
      None
    },
  }
}

/// The steps of a camera planned at `framerate` with `layer_count` layers: two thirds and half
/// the frame rate (not below [`CameraGovernorConfig::min_capture_framerate`]), then one top layer
/// after another left out at the lowest of them, down to the lowest layer.
fn load_ladder(framerate: u8, layer_count: u8, config: &CameraGovernorConfig) -> Vec<LoadStep> {
  let framerate = framerate.max(1);
  let floor = config.min_capture_framerate.min(framerate);
  let mut ladder = vec![LoadStep {
    framerate,
    top_layers_off: 0,
  }];
  for lower in [framerate * 2 / 3, framerate / 2] {
    let lower = lower.max(floor);
    if lower < ladder[ladder.len() - 1].framerate {
      ladder.push(LoadStep {
        framerate: lower,
        top_layers_off: 0,
      });
    }
  }
  let slowest = ladder[ladder.len() - 1].framerate;
  for off in 1..layer_count {
    ladder.push(LoadStep {
      framerate: slowest,
      top_layers_off: off,
    });
  }
  ladder
}

/// The limit of `level`: bitrate steps first, then frame divisors that keep the layer at
/// [`CameraGovernorConfig::min_layer_framerate`] or more.
fn limit_at(level: u8, framerate: u8, config: &CameraGovernorConfig) -> LayerLimit {
  let cuts = level.min(config.max_bitrate_cuts);
  let divisor_steps = usize::from(level - cuts);
  let frame_divisor = usable_divisors(framerate, config)
    .take(divisor_steps)
    .last()
    .unwrap_or(1);
  LayerLimit {
    bitrate_cuts: cuts,
    frame_divisor,
  }
}

fn max_level(framerate: u8, config: &CameraGovernorConfig) -> u8 {
  let divisors = u8::try_from(usable_divisors(framerate, config).count()).unwrap_or(u8::MAX);
  config.max_bitrate_cuts.saturating_add(divisors)
}

fn usable_divisors(framerate: u8, config: &CameraGovernorConfig) -> impl Iterator<Item = u8> {
  let floor = config.min_layer_framerate.max(1);
  LAYER_FRAME_DIVISORS
    .into_iter()
    .filter(move |divisor| framerate / divisor >= floor)
}

impl OutputWindow {
  fn new(now: Instant) -> Self {
    Self {
      samples: VecDeque::new(),
      since: now,
    }
  }

  fn push(&mut self, bytes: usize, now: Instant, window: Duration) {
    self.samples.push_back((now, bytes));
    self.prune(now, window);
  }

  fn reset(&mut self, now: Instant) {
    self.samples.clear();
    self.since = now;
  }

  /// Bits per second over the last `window`; `None` until a whole window was measured.
  fn rate_bps(&mut self, now: Instant, window: Duration) -> Option<f64> {
    if now.saturating_duration_since(self.since) < window || window.is_zero() {
      return None;
    }
    self.prune(now, window);
    let bytes: usize = self.samples.iter().map(|(_, bytes)| bytes).sum();
    Some(bytes as f64 * 8.0 / window.as_secs_f64())
  }

  /// Keeps the samples of the last `window`: the deque never holds more than a window's pictures.
  fn prune(&mut self, now: Instant, window: Duration) {
    while let Some(&(at, _)) = self.samples.front() {
      if now.saturating_duration_since(at) < window {
        break;
      }
      self.samples.pop_front();
    }
  }
}

impl LoadWindow {
  fn new(now: Instant) -> Self {
    Self {
      since: now,
      tally: FrameTally::default(),
    }
  }

  fn add(&mut self, tally: FrameTally) {
    self.tally.given = self.tally.given.saturating_add(tally.given);
    self.tally.skipped = self.tally.skipped.saturating_add(tally.skipped);
    self.tally.queued = self.tally.queued.saturating_add(tally.queued);
    self.tally.encode_time = self.tally.encode_time.saturating_add(tally.encode_time);
    self.tally.frame_interval = self.tally.frame_interval.saturating_add(tally.frame_interval);
  }
}

impl Probes {
  fn new() -> Self {
    Self {
      calm_needed: Duration::ZERO,
      maybe_stepped_up_at: None,
    }
  }

  /// How long a limit has to stay calm before the next step up.
  fn calm_needed(&self, config: &CameraGovernorConfig) -> Duration {
    self.calm_needed.max(config.calm_period)
  }

  /// A step down soon after a step up: that probe failed, the next one waits twice as long.
  fn on_step_down(&mut self, now: Instant, config: &CameraGovernorConfig) {
    if let Some(at) = self.maybe_stepped_up_at.take()
      && now.saturating_duration_since(at) < config.probe_proof
    {
      self.calm_needed = (self.calm_needed(config) * 2).min(config.max_calm_period);
    }
  }

  fn on_step_up(&mut self, now: Instant) {
    self.maybe_stepped_up_at = Some(now);
  }

  /// A step up that held [`CameraGovernorConfig::probe_proof`] resets the wait.
  fn note_held(&mut self, now: Instant, config: &CameraGovernorConfig) {
    if let Some(at) = self.maybe_stepped_up_at
      && now.saturating_duration_since(at) >= config.probe_proof
    {
      self.maybe_stepped_up_at = None;
      self.calm_needed = config.calm_period;
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const BUDGET_BPS: u32 = 150_000;
  const FRAMERATE: u8 = 30;

  mod layer_rate_guard {
    use super::*;

    /// #1762: a 180p layer that sends 1.2 Mbit/s for a 150 kbit/s budget is configured at a lower
    /// bitrate first, then gets fewer and fewer frames until it fits.
    #[test]
    fn an_overshooting_layer_lowers_its_bitrate_then_its_frame_rate() {
      let config = CameraGovernorConfig::default();
      let start = Instant::now();
      let mut guard = LayerRateGuard::new(start);
      let mut clock = Clock::new(start);
      // An encoder that ignores its bitrate: 5 kB per picture, whatever it is told.
      let mut changes = Vec::new();
      for _ in 0..12 {
        let limit = guard.limit(FRAMERATE, &config);
        if let Some(change) = clock.run(&mut guard, 5_000, limit, &config) {
          changes.push(change);
        }
      }
      assert_eq!(changes, vec![limit(1, 1), limit(1, 2), limit(1, 3), limit(1, 6)]);
      // At 5 fps it sends 200 kbit/s, within twice its bitrate: no further step, none possible.
      assert_eq!(guard.limit(FRAMERATE, &config), limit(1, 6));
      assert_eq!(limit(1, 6).bitrate_bps(BUDGET_BPS, &config), 75_000);
      assert_eq!(limit(1, 6).framerate(FRAMERATE), 5);
    }

    #[test]
    fn a_layer_within_its_bitrate_and_short_peaks_stay_unlimited() {
      let config = CameraGovernorConfig::default();
      let start = Instant::now();
      let mut guard = LayerRateGuard::new(start);
      let mut clock = Clock::new(start);
      // 600 bytes per picture is 144 kbit/s; a keyframe of 20 kB every 4 s adds 40 kbit/s.
      for second in 0..20 {
        if second % 4 == 0 {
          guard.on_output(20_000, clock.now, &config);
        }
        let change = clock.run(&mut guard, 600, LayerLimit::NONE, &config);
        assert_eq!(change, None);
      }
    }

    #[test]
    fn nothing_is_judged_before_a_whole_window() {
      let config = CameraGovernorConfig::default();
      let start = Instant::now();
      let mut guard = LayerRateGuard::new(start);
      guard.on_output(1_000_000, start, &config);
      let early = start + config.rate_window / 2;
      assert_eq!(guard.evaluate(BUDGET_BPS, FRAMERATE, early, &config), None);
    }

    /// The content got easy again: the layer climbs back in the reverse order, one step per calm
    /// period, frames first and the bitrate last.
    #[test]
    fn a_calm_layer_steps_back_up_in_reverse_order() {
      let config = CameraGovernorConfig::default();
      let start = Instant::now();
      let mut guard = LayerRateGuard::new(start);
      let mut clock = Clock::new(start);
      while guard.limit(FRAMERATE, &config) != limit(1, 6) {
        let current = guard.limit(FRAMERATE, &config);
        clock.run(&mut guard, 5_000, current, &config);
      }
      // A calm camera now: 200 bytes per picture, 48 kbit/s at 30 fps.
      let mut changes = Vec::new();
      for _ in 0..60 {
        let current = guard.limit(FRAMERATE, &config);
        if let Some(change) = clock.run(&mut guard, 200, current, &config) {
          changes.push((change, clock.now - start));
        }
      }
      let steps: Vec<LayerLimit> = changes.iter().map(|(change, _)| *change).collect();
      assert_eq!(
        steps,
        vec![limit(1, 3), limit(1, 2), limit(1, 1), LayerLimit::NONE]
      );
      // Each step waited a calm period after the last.
      for pair in changes.windows(2) {
        assert!(pair[1].1 - pair[0].1 >= config.calm_period);
      }
    }

    /// Hysteresis: the output at 15 fps would overshoot again at 30 fps, so the layer stays.
    #[test]
    fn a_layer_whose_step_up_would_overshoot_stays_limited() {
      let config = CameraGovernorConfig::default();
      let start = Instant::now();
      let mut guard = LayerRateGuard::new(start);
      let mut clock = Clock::new(start);
      while guard.limit(FRAMERATE, &config) != limit(1, 2) {
        let current = guard.limit(FRAMERATE, &config);
        clock.run(&mut guard, 5_000, current, &config);
      }
      // 1 kB per picture: 120 kbit/s at 15 fps, 240 kbit/s at 30 fps (above 1.25 × 150 kbit/s).
      for _ in 0..60 {
        let current = guard.limit(FRAMERATE, &config);
        assert_eq!(clock.run(&mut guard, 1_000, current, &config), None);
      }
      assert_eq!(guard.limit(FRAMERATE, &config), limit(1, 2));
    }

    /// A step up that overshoots at once doubles the calm period before the next probe.
    #[test]
    fn a_failed_probe_doubles_the_wait_for_the_next() {
      let config = CameraGovernorConfig::default();
      let start = Instant::now();
      let mut guard = LayerRateGuard::new(start);
      let mut clock = Clock::new(start);
      clock.run(&mut guard, 5_000, LayerLimit::NONE, &config);
      assert_eq!(guard.limit(FRAMERATE, &config), limit(1, 1));
      // Calm at the cut bitrate: steps up after one calm period...
      let calm_from = clock.now;
      let mut maybe_up_at = None;
      while maybe_up_at.is_none() {
        let current = guard.limit(FRAMERATE, &config);
        if clock.run(&mut guard, 200, current, &config) == Some(LayerLimit::NONE) {
          maybe_up_at = Some(clock.now);
        }
      }
      let up_at = maybe_up_at.unwrap();
      assert!(up_at - calm_from >= config.calm_period);
      // ...overshoots right away (a failed probe)...
      clock.run(&mut guard, 5_000, LayerLimit::NONE, &config);
      assert_eq!(guard.limit(FRAMERATE, &config), limit(1, 1));
      // ...and the next step up waits twice as long.
      let calm_from = clock.now;
      loop {
        let current = guard.limit(FRAMERATE, &config);
        if clock.run(&mut guard, 200, current, &config) == Some(LayerLimit::NONE) {
          break;
        }
      }
      assert!(clock.now - calm_from >= config.calm_period * 2);
    }

    /// The camera dropped to 15 fps for the encoders' load: a layer at 5 fps of 30 is held at the
    /// lowest frame step 15 fps still allows, and climbs from there.
    #[test]
    fn a_lower_camera_frame_rate_keeps_the_limit_within_its_steps() {
      let config = CameraGovernorConfig::default();
      let start = Instant::now();
      let mut guard = LayerRateGuard::new(start);
      let mut clock = Clock::new(start);
      while guard.limit(FRAMERATE, &config) != limit(1, 6) {
        let current = guard.limit(FRAMERATE, &config);
        clock.run(&mut guard, 5_000, current, &config);
      }
      // At 15 fps the divisors 2 and 3 keep 5 fps or more; 6 doesn't.
      assert_eq!(guard.limit(15, &config), limit(1, 3));
      guard.on_output(100, clock.now, &config);
      guard.evaluate(BUDGET_BPS, 15, clock.now, &config);
      assert_eq!(guard.level, max_level(15, &config));
    }

    /// Feeds a window of pictures of `bytes` at the layer's frame rate, then judges it.
    struct Clock {
      now: Instant,
    }

    impl Clock {
      fn new(now: Instant) -> Self {
        Self { now }
      }

      fn run(
        &mut self,
        guard: &mut LayerRateGuard,
        bytes: usize,
        limit: LayerLimit,
        config: &CameraGovernorConfig,
      ) -> Option<LayerLimit> {
        let framerate = limit.framerate(FRAMERATE);
        let interval = Duration::from_secs(1) / u32::from(framerate);
        let end = self.now + config.rate_window;
        while self.now < end {
          self.now += interval;
          guard.on_output(bytes, self.now, config);
        }
        guard.evaluate(BUDGET_BPS, FRAMERATE, self.now, config)
      }
    }

    fn limit(bitrate_cuts: u8, frame_divisor: u8) -> LayerLimit {
      LayerLimit {
        bitrate_cuts,
        frame_divisor,
      }
    }
  }

  mod encoder_load {
    use super::*;

    #[test]
    fn the_ladder_lowers_the_frame_rate_before_the_layers() {
      let config = CameraGovernorConfig::default();
      let ladder = load_ladder(30, 4, &config);
      let steps: Vec<(u8, u8)> = ladder
        .iter()
        .map(|step| (step.framerate, step.top_layers_off))
        .collect();
      assert_eq!(steps, vec![(30, 0), (20, 0), (15, 0), (15, 1), (15, 2), (15, 3)]);
      // A slow camera keeps its floor, and a single layer is never left out.
      let ladder = load_ladder(12, 1, &config);
      assert_eq!(ladder, vec![step(12, 0), step(10, 0)]);
    }

    /// An encoder that can't keep up: the frame rate goes first, then the top layers, one step
    /// per load window.
    #[test]
    fn an_overloaded_camera_steps_down_frame_rate_first_then_resolution() {
      let config = CameraGovernorConfig::default();
      let start = Instant::now();
      let mut load = CameraLoad::new(30, 3, start, &config);
      let mut now = start;
      let mut steps = Vec::new();
      for _ in 0..6 {
        let framerate = load.step().framerate;
        now = feed(&mut load, now, framerate, overloaded(), &config);
        if let Some(step) = load.evaluate(now, &config) {
          steps.push(step);
        }
      }
      assert_eq!(steps, vec![step(20, 0), step(15, 0), step(15, 1), step(15, 2)]);
      assert_eq!(load.maybe_reason(), Some(OverloadReason::SkippedFrames));
    }

    #[test]
    fn frames_waiting_in_the_queue_count_as_overload_too() {
      let config = CameraGovernorConfig::default();
      let start = Instant::now();
      let mut load = CameraLoad::new(30, 3, start, &config);
      let queued = FrameTally {
        given: 3,
        skipped: 0,
        queued: 4,
        ..FrameTally::default()
      };
      let now = feed(&mut load, start, 30, queued, &config);
      assert_eq!(load.evaluate(now, &config), Some(step(20, 0)));
      assert_eq!(load.maybe_reason(), Some(OverloadReason::QueuedFrames));
    }

    /// Between overloaded and calm (a frame in the queue now and then): no step either way.
    #[test]
    fn a_busy_but_coping_camera_stays_where_it_is() {
      let config = CameraGovernorConfig::default();
      let start = Instant::now();
      let mut load = CameraLoad::new(30, 3, start, &config);
      let mut now = feed(&mut load, start, 30, overloaded(), &config);
      load.evaluate(now, &config);
      let busy = FrameTally {
        given: 3,
        skipped: 0,
        queued: 1,
        ..FrameTally::default()
      };
      for _ in 0..30 {
        now = feed(&mut load, now, 20, busy, &config);
        assert_eq!(load.evaluate(now, &config), None);
      }
      assert_eq!(load.step(), step(20, 0));
    }

    /// The machine got idle: layers come back first, then the frame rate, each after a calm
    /// period; a probe that overloads at once doubles the wait.
    #[test]
    fn a_calm_camera_recovers_in_reverse_order_with_backoff() {
      let config = CameraGovernorConfig::default();
      let start = Instant::now();
      let mut load = CameraLoad::new(30, 2, start, &config);
      let mut now = start;
      while load.step() != step(15, 1) {
        let framerate = load.step().framerate;
        now = feed(&mut load, now, framerate, overloaded(), &config);
        load.evaluate(now, &config);
      }
      let mut steps = Vec::new();
      while load.step() != step(30, 0) {
        let framerate = load.step().framerate;
        now = feed(&mut load, now, framerate, calm(), &config);
        if let Some(step) = load.evaluate(now, &config) {
          steps.push((step, now));
        }
      }
      let order: Vec<LoadStep> = steps.iter().map(|(step, _)| *step).collect();
      assert_eq!(order, vec![step(15, 0), step(20, 0), step(30, 0)]);
      for pair in steps.windows(2) {
        assert!(pair[1].1 - pair[0].1 >= config.calm_period);
      }
      assert_eq!(load.maybe_reason(), None);
      // Overloaded right after the step up: back down, and the next probe waits twice as long.
      now = feed(&mut load, now, 30, overloaded(), &config);
      assert_eq!(load.evaluate(now, &config), Some(step(20, 0)));
      let calm_from = now;
      loop {
        now = feed(&mut load, now, 20, calm(), &config);
        if load.evaluate(now, &config).is_some() {
          break;
        }
      }
      assert!(now - calm_from >= config.calm_period * 2);
    }

    #[test]
    fn too_few_frames_say_nothing() {
      let config = CameraGovernorConfig::default();
      let start = Instant::now();
      let mut load = CameraLoad::new(30, 3, start, &config);
      load.on_frame(overloaded());
      let now = start + config.load_window;
      assert_eq!(load.evaluate(now, &config), None);
      assert_eq!(load.step(), load.planned());
    }

    /// #1733 reuses the judge with its own ladder: encoding that takes most of the frame
    /// interval is overload, and the level index is all it moves.
    #[test]
    fn the_judge_moves_a_level_index_on_encode_time_too() {
      let config = CameraGovernorConfig::default();
      let start = Instant::now();
      let mut load = EncoderLoad::with_levels(3, start);
      let interval = Duration::from_millis(33);
      let slow = FrameTally {
        given: 1,
        encode_time: Duration::from_millis(31),
        frame_interval: interval,
        ..FrameTally::default()
      };
      for _ in 0..60 {
        load.on_frame(slow);
      }
      assert_eq!(load.evaluate(start + config.load_window, &config), Some(1));
      assert_eq!(load.maybe_reason(), Some(OverloadReason::SlowEncoding));
      // Half the interval is neither overloaded nor calm: it stays.
      let busy = FrameTally {
        encode_time: Duration::from_millis(20),
        ..slow
      };
      let mut now = start + config.load_window;
      for _ in 0..10 {
        for _ in 0..60 {
          load.on_frame(busy);
        }
        now += config.load_window;
        assert_eq!(load.evaluate(now, &config), None);
      }
      // A single level never steps.
      let mut single = EncoderLoad::with_levels(0, start);
      for _ in 0..60 {
        single.on_frame(slow);
      }
      assert_eq!(single.evaluate(start + config.load_window, &config), None);
      assert_eq!(single.level(), 0);
    }

    /// Gives a load window of frames at `framerate`, each taken as `tally` says.
    fn feed(
      load: &mut CameraLoad,
      start: Instant,
      framerate: u8,
      tally: FrameTally,
      config: &CameraGovernorConfig,
    ) -> Instant {
      let frames = u32::from(framerate) * u32::try_from(config.load_window.as_secs()).unwrap();
      for _ in 0..frames {
        load.on_frame(tally);
      }
      start + config.load_window
    }

    /// Three layers, one of which found its queue full.
    fn overloaded() -> FrameTally {
      FrameTally {
        given: 2,
        skipped: 1,
        queued: 4,
        ..FrameTally::default()
      }
    }

    fn calm() -> FrameTally {
      FrameTally {
        given: 3,
        skipped: 0,
        queued: 0,
        ..FrameTally::default()
      }
    }

    fn step(framerate: u8, top_layers_off: u8) -> LoadStep {
      LoadStep {
        framerate,
        top_layers_off,
      }
    }
  }
}
