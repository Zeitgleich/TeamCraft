//! How a screen share is encoded (#1733): which layers, in which codec, at which size and rate.
//!
//! Meetings share text, not games. Under CPU or bandwidth pressure a detail share keeps its
//! resolution and lowers its frame rate; a higher rate comes only with headroom. The layers:
//!
//! - **Base:** H.264 at about 480p, always sent. Every device decodes it, so a viewer without AV1
//!   or on a weak link gets it without the publisher noticing (the SFU picks per viewer). It is
//!   the floor for weak viewers, never the next step down from the full picture.
//! - **Middle:** half the share's full picture, where that is clearly larger than the base (from
//!   1440p) and clearly smaller than the top. In the top's codec where every viewer decodes it,
//!   else H.264.
//! - **Top:** the share's picture at full size, in the best codec this device encodes well and
//!   some viewer decodes: AV1 or VP9 where it encodes them in hardware, AV1 in software only up to
//!   about 1080p and at a reduced rate, else H.264. H.264 is a codec of its own, not a fallback:
//!   a device without a good AV1 encoder sends its full picture in H.264, at the bitrate H.264
//!   needs for it ([`codec_bitrate_bps`]: AV1 and VP9 get less for the same picture).
//!
//! A [`ShareLadder`] lists what the encoders get, from the full plan down to the least the share
//! is sent at, in the order agreed for CPU pressure:
//!
//! 1. the top layer's frame rate, down to about 5 fps;
//! 2. the middle layer off;
//! 3. a top layer in software AV1 to H.264 at the same size;
//! 4. the top layer's size, step by step ([`top_sizes`]: 2160, 1440, 1080, 900 and 720 lines in
//!    the capture's aspect), each at the least frame rate;
//! 5. only when even the smallest of them is too much, the base layer alone.
//!
//! A [`ShareGovernor`] walks the ladder: the encoders' load (frames queued or skipped, encode time
//! against the frame interval) down at once and back up in reverse after a calm period, waiting
//! longer after each step up that didn't hold. A limited uplink first lowers the top layer's frame
//! rate with its bitrate ([`share_framerate`]); where it can't carry the top layer even so, the
//! governor steps the top's size down the same sizes, one at a time, and back up once the uplink
//! carried the smaller one whole for a while. Recovery is the reverse: size first, then rate.
//! Pure policy: the driver feeds the inputs and builds encoders from the layers.

use std::collections::VecDeque;
use std::time::Duration;

use teamcraft_proto::{ScreenContentMode, VideoCodec, VideoLayerInfo};
// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::camera_governor::{CameraGovernorConfig, EncoderLoad, FrameTally, OverloadReason};
use crate::video::{ScreenProfile, VideoResolution, fit_within, screen_bitrate_bps};

/// The base layer's most lines: about 480p, as an integer fraction of the top (#1733): a third of
/// 1440p (480), half of 1080p (540), a quarter of 2160p (540). An integer step keeps a scrolled
/// page's one-pixel moves whole pixels in the base, which the encoders follow cheaply; at 1.5
/// times smaller they became fractions, and OpenH264 skipped nearly every picture to hold the
/// bitrate.
const BASE_MAX_LINES: u16 = 540;
/// The base layer's frame rate for a detail share: text needs no more.
const BASE_DETAIL_FRAMERATE: u8 = 15;
/// Bits per pixel of the base layer: H.264 at 480p needs more per pixel than the large layers to
/// keep small text readable at all.
const BASE_BITS_PER_PIXEL: f64 = 0.08;
const BASE_MIN_BITRATE_BPS: u32 = 300_000;
/// A middle layer above the base needs at least this many times its lines to be worth sending: a
/// 1080p share's half (540p) is the base, a 1440p share's (720p) is clearly more.
const MIN_LINES_RATIO: f64 = 1.4;
/// A stepped-down top layer needs at least this many times the base's lines: 720p above a 540p
/// base still is a step worth taking before the base alone.
const MIN_TOP_LINES_RATIO: f64 = 1.3;
/// The least frame rate a share is sent at, for the CPU and the uplink alike ("about 5 fps").
pub const MIN_SHARE_FRAMERATE: u8 = 5;
/// The frame rates a detail share's top layer steps down through, highest first; the least is
/// [`MIN_SHARE_FRAMERATE`].
const DETAIL_TOP_FRAMERATES: [u8; 5] = [30, 20, 15, 10, MIN_SHARE_FRAMERATE];
/// A top layer in software AV1 starts no higher than this ("reduced fps").
const SOFTWARE_AV1_MAX_FRAMERATE: u8 = 15;
/// A top layer in software AV1 has at most this many pixels (1080p and a little): above, the
/// browser's AV1 encoder fell to about 9 fps at 1440p on a loaded machine.
const SOFTWARE_AV1_MAX_PIXELS: u32 = 1920 * 1080 * 11 / 10;
/// The sizes the top layer steps down through, in lines (the shorter side), below the share's
/// full picture: each kept where it is clearly larger than the base.
const TOP_LINES: [u16; 5] = [2160, 1440, 1080, 900, 720];
/// The longer side of a stepped-down top layer is a multiple of this: hardware encoders work in
/// 16-pixel macroblocks, and the shorter side keeps the standard line count (even, I420).
const SIDE_ALIGNMENT: u16 = 16;

/// The share of H.264's bitrate an AV1 layer gets for the same picture: with its screen content
/// tools AV1 at 2 Mbit/s was as sharp as H.264 at 4 (docs/performance.md).
const AV1_BITRATE_SHARE: f64 = 0.6;
/// The same for VP9, between the two.
const VP9_BITRATE_SHARE: f64 = 0.8;

/// How this device encodes a codec, as far as the platform tells (WebCodecs: `isConfigSupported`
/// with `prefer-hardware` and `MediaCapabilities.encodingInfo().powerEfficient`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderKind {
  /// A hardware encoder: full size and rate.
  Hardware,
  /// Software only: AV1 up to about 1080p at a reduced rate, VP9 not at all.
  Software,
  /// No encoder.
  Unavailable,
}

/// How this device encodes the codecs a share's top layer may use besides H.264, which every
/// device encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareEncoders {
  pub av1: EncoderKind,
  pub vp9: EncoderKind,
}

/// The codecs a share's layers may use, from what the other devices decode; both lists only of
/// codecs this device encodes, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareCodecs {
  /// Decoded by every viewer ([`crate::call_engine::CallEngine::video_codecs_for`]): the middle
  /// layer's where the top's is one of them, and the whole share's where the SFU forwards no
  /// layers in another codec than their track's. Empty means H.264 only.
  pub by_all: Vec<VideoCodec>,
  /// Decoded by some viewers as a layer in another codec than its track's
  /// ([`crate::call_engine::CallEngine::layer_codecs_for`]); empty where the SFU forwards no such
  /// layers: the whole share then goes out in one codec of `by_all`.
  pub by_some: Vec<VideoCodec>,
}

/// What a share is planned from: what is captured and how, and what the codecs allow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharePlan {
  pub source: VideoResolution,
  /// The rate the screen is captured at.
  pub framerate: u8,
  pub profile: ScreenProfile,
  pub codecs: ShareCodecs,
  pub encoders: ShareEncoders,
}

/// What the share's encoders get at one step of the [`ShareLadder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareStep {
  pub top_codec: VideoCodec,
  pub top_framerate: u8,
  /// The top layer's size; `None` at the last step: the base layer alone.
  pub maybe_top: Option<VideoResolution>,
  pub has_middle: bool,
}

/// Every step a share may be sent at, the full plan first ([`ShareLadder::steps`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareLadder {
  plan: SharePlan,
  steps: Vec<ShareStep>,
  /// The top layer's sizes, the full picture first ([`top_sizes`]).
  top_sizes: Vec<VideoResolution>,
}

/// A share's encoders against the frames they get, and its uplink against its top layer, on its
/// [`ShareLadder`] (module docs). The encoders' load uses the same controller as a camera's
/// ([`EncoderLoad`]); the ladder is the share's own.
#[derive(Debug, Clone)]
pub struct ShareGovernor {
  ladder: ShareLadder,
  load: EncoderLoad,
  uplink: UplinkSizes,
  config: CameraGovernorConfig,
}

/// How the uplink treats a share's top layer now ([`crate::call_engine::CallEngine::uplink_top`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UplinkTop {
  /// It carries the top layer at its whole bitrate.
  Whole,
  /// It carries the top layer at a share of its bitrate (a lower frame rate).
  Reduced,
  /// It doesn't carry the top layer at all.
  Held,
}

/// Why a share's layers are what they are (stats for nerds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareLimits {
  /// How the sender encodes AV1 and VP9.
  pub encoders: ShareEncoders,
  /// The full plan's step and the step now.
  pub planned: ShareStep,
  pub now: ShareStep,
  /// Steps down the CPU ladder, of [`ShareLadder::len`] less one.
  pub level: usize,
  pub levels: usize,
  /// Why the encoders were last found overloaded, while below the plan.
  pub maybe_reason: Option<OverloadReason>,
  /// Sizes the top layer stepped down for the uplink, of `sizes` less one.
  pub uplink_steps: usize,
  pub sizes: usize,
}

/// Encoders whose work is out of sight (WebCodecs): each frame's time from being given to an
/// encoder until its picture came out, matched by the frame's timestamp.
#[derive(Debug, Clone, Default)]
pub struct EncodeTimer {
  /// When each of the last frames was given, by its timestamp in whole microseconds.
  starts: VecDeque<(i64, Instant)>,
}

/// The top layer's size as the uplink allows it: an index into the ladder's top sizes.
#[derive(Debug, Clone)]
struct UplinkSizes {
  index: usize,
  maybe_held_since: Option<Instant>,
  maybe_whole_since: Option<Instant>,
  calm_needed: Duration,
  maybe_stepped_up_at: Option<Instant>,
}

impl ShareLadder {
  /// The steps for `plan`, the full plan first.
  pub fn new(plan: SharePlan) -> Self {
    let top_sizes = top_sizes(full_top_size(&plan));
    let steps = ladder_steps(&plan, &top_sizes);
    Self {
      plan,
      steps,
      top_sizes,
    }
  }

  pub fn plan(&self) -> &SharePlan {
    &self.plan
  }

  /// The steps, the full plan first; never empty.
  pub fn steps(&self) -> &[ShareStep] {
    &self.steps
  }

  pub fn len(&self) -> usize {
    self.steps.len()
  }

  pub fn is_empty(&self) -> bool {
    self.steps.is_empty()
  }

  /// The top layer's sizes, the full picture first; never empty.
  pub fn top_sizes(&self) -> &[VideoResolution] {
    &self.top_sizes
  }

  /// The step at `level` (0: the full plan), the last one beyond.
  pub fn step(&self, level: usize) -> &ShareStep {
    // Invariant: `ladder_steps` always yields the full plan, so the index is in range.
    &self.steps[level.min(self.steps.len().saturating_sub(1))]
  }

  /// The codec the share is published in at `step`: its base layer's. It changes with the step
  /// only where the SFU takes one codec per track (the whole share falls back to H.264).
  pub fn track_codec(&self, step: &ShareStep) -> VideoCodec {
    base_codec(&self.plan.codecs, step)
  }

  /// The layers at `step`, lowest first, each layer in another codec than
  /// [`Self::track_codec`] naming it.
  pub fn layers(&self, step: &ShareStep) -> Vec<VideoLayerInfo> {
    share_layers(&self.plan, step)
  }
}

impl ShareGovernor {
  /// Starts at the full plan of `plan`.
  pub fn new(plan: SharePlan, now: Instant) -> Self {
    Self::with_config(plan, CameraGovernorConfig::default(), now)
  }

  pub fn with_config(plan: SharePlan, config: CameraGovernorConfig, now: Instant) -> Self {
    let ladder = ShareLadder::new(plan);
    let load = EncoderLoad::with_levels(ladder.len(), now);
    Self {
      ladder,
      load,
      uplink: UplinkSizes::new(),
      config,
    }
  }

  pub fn ladder(&self) -> &ShareLadder {
    &self.ladder
  }

  /// The CPU ladder's level: 0 is the plan.
  pub fn level(&self) -> usize {
    self.load.level()
  }

  /// What the encoders get now: the CPU ladder's step, its top no larger than the uplink allows.
  pub fn step(&self) -> ShareStep {
    let mut step = self.ladder.step(self.level()).clone();
    let uplink_top = self.ladder.top_sizes[self.uplink.index.min(self.ladder.top_sizes.len() - 1)];
    if let Some(top) = step.maybe_top
      && lines(uplink_top) < lines(top)
    {
      step.maybe_top = Some(uplink_top);
    }
    step
  }

  /// The layers now, lowest first.
  pub fn layers(&self) -> Vec<VideoLayerInfo> {
    self.ladder.layers(&self.step())
  }

  /// The codec the share is published in now.
  pub fn track_codec(&self) -> VideoCodec {
    self.ladder.track_codec(&self.step())
  }

  /// A frame was given to the encoders (or an encoded one came back with its encode time).
  pub fn on_frame(&mut self, tally: FrameTally) {
    self.load.on_frame(tally);
  }

  /// Measures afresh from `now`: the encoders were replaced or paused.
  pub fn measure_afresh(&mut self, now: Instant) {
    self.load.measure_afresh(now);
  }

  /// Judges the encoders' load once a window is whole; the new CPU level when it changed.
  pub fn evaluate(&mut self, now: Instant) -> Option<usize> {
    self.load.evaluate(now, &self.config)
  }

  /// The uplink treats the top layer as `top` at `now`: whether the top layer's size changed.
  /// Held (even at its least frame rate) for [`CameraGovernorConfig::load_window`] makes it one
  /// size smaller; carried whole for the calm period makes it one size larger again, with the
  /// same backoff as the encoders' load after a step up that didn't hold.
  ///
  /// While the encoders are overloaded, a held top layer doesn't step the size (#1846): a starved
  /// encoder delivers its frames in bursts, which the uplink's estimate reads as a narrow link,
  /// and the CPU's own steps already lower what is sent. Stepping on both flapped the size every
  /// few seconds on a clean uplink, and every change sent the share anew to all its viewers.
  pub fn on_uplink(&mut self, top: UplinkTop, now: Instant) -> bool {
    let top = if top == UplinkTop::Held && self.load.maybe_reason().is_some() {
      UplinkTop::Reduced
    } else {
      top
    };
    let sizes = self.ladder.top_sizes.len();
    self.uplink.on_uplink(top, sizes, now, &self.config)
  }

  /// Plans the share anew (the source was resized, the viewers' codecs changed): the load and
  /// its level carry over where the new ladder has as many steps, else it starts at the plan.
  pub fn replan(&mut self, plan: SharePlan, now: Instant) {
    if plan == *self.ladder.plan() {
      return;
    }
    let ladder = ShareLadder::new(plan);
    if ladder.len() != self.ladder.len() {
      self.load = EncoderLoad::with_levels(ladder.len(), now);
    }
    if ladder.top_sizes.len() != self.ladder.top_sizes.len() {
      self.uplink = UplinkSizes::new();
    }
    self.ladder = ladder;
  }

  pub fn limits(&self) -> ShareLimits {
    ShareLimits {
      encoders: self.ladder.plan().encoders,
      planned: self.ladder.step(0).clone(),
      now: self.step(),
      level: self.level(),
      levels: self.ladder.len().saturating_sub(1),
      maybe_reason: self.load.maybe_reason(),
      uplink_steps: self.uplink.index,
      sizes: self.ladder.top_sizes.len(),
    }
  }
}

impl UplinkSizes {
  fn new() -> Self {
    Self {
      index: 0,
      maybe_held_since: None,
      maybe_whole_since: None,
      calm_needed: Duration::ZERO,
      maybe_stepped_up_at: None,
    }
  }

  fn on_uplink(&mut self, top: UplinkTop, sizes: usize, now: Instant, config: &CameraGovernorConfig) -> bool {
    if self
      .maybe_stepped_up_at
      .is_some_and(|at| now.saturating_duration_since(at) >= config.probe_proof)
    {
      // The last step up held: the uplink carries it.
      self.maybe_stepped_up_at = None;
      self.calm_needed = Duration::ZERO;
    }
    match top {
      UplinkTop::Held => {
        self.maybe_whole_since = None;
        let since = *self.maybe_held_since.get_or_insert(now);
        if now.saturating_duration_since(since) < config.load_window || self.index + 1 >= sizes {
          return false;
        }
        if let Some(at) = self.maybe_stepped_up_at.take()
          && now.saturating_duration_since(at) < config.probe_proof
        {
          self.calm_needed = (self.calm_needed.max(config.calm_period) * 2).min(config.max_calm_period);
        }
        self.index += 1;
        // The smaller top goes out anew: it has a whole window to show whether it fits.
        self.maybe_held_since = None;
        true
      },
      UplinkTop::Whole if self.index > 0 => {
        self.maybe_held_since = None;
        let since = *self.maybe_whole_since.get_or_insert(now);
        if now.saturating_duration_since(since) < self.calm_needed.max(config.calm_period) {
          return false;
        }
        self.index -= 1;
        self.maybe_whole_since = None;
        self.maybe_stepped_up_at = Some(now);
        true
      },
      UplinkTop::Whole | UplinkTop::Reduced => {
        self.maybe_held_since = None;
        self.maybe_whole_since = None;
        false
      },
    }
  }
}

impl EncodeTimer {
  /// The frames an encoder may hold before the oldest start is forgotten.
  const MAX_FRAMES: usize = 64;

  /// The frame stamped `timestamp_us` was given to the encoder at `given_at`.
  pub fn on_given(&mut self, timestamp_us: f64, given_at: Instant) {
    // The cast keeps the whole microseconds a frame's timestamp has.
    self.starts.push_back((timestamp_us as i64, given_at));
    if self.starts.len() > Self::MAX_FRAMES {
      self.starts.pop_front();
    }
  }

  /// The picture of the frame stamped `timestamp_us` came out at `encoded_at`: how long it took,
  /// when that frame was given. Frames given before it and never encoded (the encoder dropped
  /// them) are forgotten.
  pub fn on_encoded(&mut self, timestamp_us: f64, encoded_at: Instant) -> Option<Duration> {
    let key = timestamp_us as i64;
    let index = self.starts.iter().position(|(stamp, _)| *stamp == key)?;
    let (_, given_at) = self.starts[index];
    self.starts.drain(..=index);
    Some(encoded_at.saturating_duration_since(given_at))
  }

  /// Forgets every start: the encoders were replaced.
  pub fn clear(&mut self) {
    self.starts.clear();
  }
}

/// The frame rate a screen share's layer is encoded at when the uplink gives it `bitrate_bps` of
/// its [`VideoLayerInfo::max_bitrate_bps`] (#1733, [`crate::uplink_layers::UplinkPolicy`]): the
/// same share of its rate, so each picture keeps its quality and the resolution stays; at least
/// [`MIN_SHARE_FRAMERATE`].
pub fn share_framerate(info: &VideoLayerInfo, bitrate_bps: u32) -> u8 {
  let share = f64::from(bitrate_bps) / f64::from(info.max_bitrate_bps.max(1));
  let framerate = (f64::from(info.max_framerate) * share.min(1.0)).round();
  // Within 0..=max_framerate (a u8) after `min(1.0)`.
  (framerate as u8).clamp(
    MIN_SHARE_FRAMERATE.min(info.max_framerate),
    info.max_framerate.max(1),
  )
}

/// The top layer's sizes for a share whose full top is `full_top`: the full picture, then each of
/// [`TOP_LINES`] below it that is clearly larger than the base, in the full picture's aspect: the
/// shorter side at the line count, the longer side scaled and aligned to [`SIDE_ALIGNMENT`].
pub fn top_sizes(full_top: VideoResolution) -> Vec<VideoResolution> {
  let base_lines = f64::from(lines(base_size(full_top)));
  let mut sizes = vec![full_top];
  for step_lines in TOP_LINES {
    if step_lines >= lines(full_top) || f64::from(step_lines) < base_lines * MIN_TOP_LINES_RATIO {
      continue;
    }
    sizes.push(with_lines(full_top, step_lines));
  }
  sizes
}

/// The steps of [`ShareLadder::new`].
fn ladder_steps(plan: &SharePlan, top_sizes: &[VideoResolution]) -> Vec<ShareStep> {
  let full_top = top_sizes[0];
  let top_codec = planned_top_codec(plan, full_top);
  let is_software_av1 = top_codec == VideoCodec::Av1 && plan.encoders.av1 != EncoderKind::Hardware;
  let framerate = plan.framerate.clamp(1, plan.profile.limits().1);
  let max_framerate = if is_software_av1 {
    framerate.min(SOFTWARE_AV1_MAX_FRAMERATE)
  } else {
    framerate
  };
  let full = ShareStep {
    top_codec,
    top_framerate: max_framerate,
    maybe_top: Some(full_top),
    has_middle: middle_size(full_top).is_some(),
  };
  let mut steps = match plan.profile.mode {
    ScreenContentMode::Motion => motion_steps(full, top_sizes),
    ScreenContentMode::Detail | ScreenContentMode::Unknown => detail_steps(full, top_sizes, is_software_av1),
  };
  // The least: the base layer alone, where there is a base below the top.
  if let Some(last) = steps.last().cloned()
    && last
      .maybe_top
      .is_some_and(|top| lines(top) > lines(base_size(full_top)))
  {
    steps.push(ShareStep {
      maybe_top: None,
      has_middle: false,
      ..last
    });
  }
  steps
}

/// A detail share: frame rate, middle layer, software AV1, then size, each size at the least
/// rate.
fn detail_steps(full: ShareStep, top_sizes: &[VideoResolution], is_software_av1: bool) -> Vec<ShareStep> {
  let mut steps = vec![full.clone()];
  let mut step = full;
  for framerate in DETAIL_TOP_FRAMERATES {
    if framerate < step.top_framerate {
      step.top_framerate = framerate;
      steps.push(step.clone());
    }
  }
  if step.has_middle {
    step.has_middle = false;
    steps.push(step.clone());
  }
  if is_software_av1 {
    step.top_codec = VideoCodec::H264;
    steps.push(step.clone());
  }
  for &size in &top_sizes[1..] {
    step.maybe_top = Some(size);
    steps.push(step.clone());
  }
  steps
}

/// A motion share (#530, chosen for video and animation): it keeps its rate longest, so the
/// middle layer goes first, then the size step by step, then the rate down to half.
fn motion_steps(full: ShareStep, top_sizes: &[VideoResolution]) -> Vec<ShareStep> {
  let mut steps = vec![full.clone()];
  let mut step = full;
  if step.has_middle {
    step.has_middle = false;
    steps.push(step.clone());
  }
  for &size in &top_sizes[1..] {
    step.maybe_top = Some(size);
    steps.push(step.clone());
  }
  let half = (step.top_framerate / 2).max(1);
  if half < step.top_framerate {
    step.top_framerate = half;
    steps.push(step);
  }
  steps
}

/// The top layer's codec in the full plan, the first that the share's profile takes, the viewers
/// decode ([`ShareCodecs`]) and this device encodes well enough at this size: AV1 in hardware, VP9
/// in hardware, AV1 in software up to about 1080p, else H.264 at full size.
fn planned_top_codec(plan: &SharePlan, full_top: VideoResolution) -> VideoCodec {
  let decoded = if plan.codecs.by_some.is_empty() {
    &plan.codecs.by_all
  } else {
    &plan.codecs.by_some
  };
  let is_usable = |codec: &VideoCodec| decoded.contains(codec) && plan.profile.codecs().contains(codec);
  let pixels = u32::from(full_top.width) * u32::from(full_top.height);
  let encoders = plan.encoders;
  let candidates = [
    (VideoCodec::Av1, encoders.av1 == EncoderKind::Hardware),
    (VideoCodec::Vp9, encoders.vp9 == EncoderKind::Hardware),
    (
      VideoCodec::Av1,
      encoders.av1 == EncoderKind::Software && pixels <= SOFTWARE_AV1_MAX_PIXELS,
    ),
  ];
  candidates
    .into_iter()
    .find(|(codec, is_good)| *is_good && is_usable(codec))
    // H.264, which every device decodes and encodes, otherwise.
    .map_or(VideoCodec::H264, |(codec, _)| codec)
}

/// `h264_bps` for a layer in `codec`: AV1 and VP9 need less than H.264 for the same picture.
pub fn codec_bitrate_bps(h264_bps: u32, codec: &VideoCodec) -> u32 {
  let share = match codec {
    VideoCodec::Av1 => AV1_BITRATE_SHARE,
    VideoCodec::Vp9 => VP9_BITRATE_SHARE,
    VideoCodec::H264 | VideoCodec::Unknown(_) => 1.0,
  };
  // At most `h264_bps`, so within u32.
  (f64::from(h264_bps) * share).round() as u32
}

/// The base layer's codec: H.264 where the SFU forwards layers in other codecs (every device
/// decodes it), else the one codec of the whole share, the top's.
fn base_codec(codecs: &ShareCodecs, step: &ShareStep) -> VideoCodec {
  if codecs.by_some.is_empty() {
    step.top_codec.clone()
  } else {
    VideoCodec::H264
  }
}

/// The layers of `plan` at `step`, lowest first.
fn share_layers(plan: &SharePlan, step: &ShareStep) -> Vec<VideoLayerInfo> {
  let base_codec = base_codec(&plan.codecs, step);
  let size = &plan.profile.size;
  let base = base_size(full_top_size(plan));
  let is_detail = plan.profile.mode != ScreenContentMode::Motion;
  let base_framerate = if is_detail {
    step.top_framerate.clamp(1, BASE_DETAIL_FRAMERATE)
  } else {
    step.top_framerate
  };
  let mut layers = Vec::new();
  let is_base_below_top = step.maybe_top.is_none_or(|top| lines(top) > lines(base));
  if is_base_below_top {
    layers.push((
      base,
      base_framerate,
      base_bitrate_bps(base, base_framerate),
      base_codec.clone(),
    ));
  }
  let Some(top) = step.maybe_top else {
    return numbered(layers, &base_codec);
  };
  if step.has_middle
    && let Some(middle) = middle_size(full_top_size(plan))
    && f64::from(lines(top)) >= f64::from(lines(middle)) * MIN_LINES_RATIO
  {
    // Without layer codecs the base's codec is the top's, and so is the middle's.
    let middle_codec = if plan.codecs.by_all.contains(&step.top_codec) {
      step.top_codec.clone()
    } else {
      base_codec.clone()
    };
    let bitrate = codec_bitrate_bps(
      screen_bitrate_bps(top, step.top_framerate, size) / 4,
      &middle_codec,
    );
    layers.push((middle, step.top_framerate, bitrate, middle_codec));
  }
  let top_bitrate = codec_bitrate_bps(screen_bitrate_bps(top, step.top_framerate, size), &step.top_codec);
  layers.push((top, step.top_framerate, top_bitrate, step.top_codec.clone()));
  numbered(layers, &base_codec)
}

/// Layers numbered from 0, each in another codec than `base_codec` naming it.
fn numbered(
  layers: Vec<(VideoResolution, u8, u32, VideoCodec)>,
  base_codec: &VideoCodec,
) -> Vec<VideoLayerInfo> {
  layers
    .into_iter()
    .zip(0u8..)
    .map(
      |((resolution, max_framerate, max_bitrate_bps, codec), layer)| VideoLayerInfo {
        layer,
        width: resolution.width,
        height: resolution.height,
        max_framerate,
        max_bitrate_bps,
        maybe_codec: (codec != *base_codec).then_some(codec),
      },
    )
    .collect()
}

/// The top layer's full size: the source within the profile's limit.
fn full_top_size(plan: &SharePlan) -> VideoResolution {
  let (limit, _) = plan.profile.limits();
  fit_within(plan.source, oriented_like(limit, plan.source))
}

/// The base layer's size for a share whose full top is `full_top`: the top divided by the least
/// integer that brings it to [`BASE_MAX_LINES`] or fewer, with even sides; the top itself where it
/// has no more lines.
fn base_size(full_top: VideoResolution) -> VideoResolution {
  let top_lines = lines(full_top);
  if top_lines <= BASE_MAX_LINES {
    return full_top;
  }
  let divisor = top_lines.div_ceil(BASE_MAX_LINES);
  VideoResolution {
    width: ((full_top.width / divisor) & !1).max(2),
    height: ((full_top.height / divisor) & !1).max(2),
  }
}

/// The middle layer's size, half the full top's, where that is clearly larger than the base.
fn middle_size(full_top: VideoResolution) -> Option<VideoResolution> {
  let middle = VideoResolution {
    width: (full_top.width / 2) & !1,
    height: (full_top.height / 2) & !1,
  };
  (f64::from(lines(middle)) >= f64::from(lines(base_size(full_top))) * MIN_LINES_RATIO).then_some(middle)
}

/// A picture's lines: its shorter side (a portrait screen's width).
fn lines(size: VideoResolution) -> u16 {
  size.width.min(size.height)
}

/// `size` with `target` lines and its aspect: the longer side scaled and aligned down to
/// [`SIDE_ALIGNMENT`].
fn with_lines(size: VideoResolution, target: u16) -> VideoResolution {
  let (short, long) = (lines(size), size.width.max(size.height));
  let scaled = u32::from(long) * u32::from(target) / u32::from(short.max(1));
  let aligned = u16::try_from(scaled).unwrap_or(long) / SIDE_ALIGNMENT * SIDE_ALIGNMENT;
  let (short, long) = (target & !1, aligned.max(SIDE_ALIGNMENT));
  if size.width >= size.height {
    VideoResolution {
      width: long,
      height: short,
    }
  } else {
    VideoResolution {
      width: short,
      height: long,
    }
  }
}

/// The base layer's bitrate at `size` and `framerate`.
fn base_bitrate_bps(size: VideoResolution, framerate: u8) -> u32 {
  let pixels_per_second = f64::from(size.width) * f64::from(size.height) * f64::from(framerate);
  // A 480p picture at most 60 fps stays far below u32::MAX bits.
  ((pixels_per_second * BASE_BITS_PER_PIXEL) as u32).max(BASE_MIN_BITRATE_BPS)
}

/// `limit` turned to portrait when `source` is taller than wide.
fn oriented_like(limit: VideoResolution, source: VideoResolution) -> VideoResolution {
  if (source.height > source.width) == (limit.height > limit.width) {
    limit
  } else {
    VideoResolution {
      width: limit.height,
      height: limit.width,
    }
  }
}

#[cfg(feature = "call-view")]
impl ShareLimits {
  /// The limits as the stats overlay's "Share policy" row shows them, from the browser's sender
  /// and the desktop's alike.
  pub fn stats(self) -> teamcraft_client_api::video_stats::ShareLimitsStats {
    use teamcraft_client_api::video_stats::{EncoderOverload, ShareLimitsStats, ShareStepStats, VideoSize};

    let step = |step: ShareStep| ShareStepStats {
      top_codec: step.top_codec,
      top_framerate: step.top_framerate,
      maybe_top: step.maybe_top.map(|top| VideoSize {
        width: top.width,
        height: top.height,
      }),
      has_middle: step.has_middle,
    };
    ShareLimitsStats {
      av1_encoder: self.encoders.av1.stats(),
      vp9_encoder: self.encoders.vp9.stats(),
      planned: step(self.planned),
      now: step(self.now),
      level: u8::try_from(self.level).unwrap_or(u8::MAX),
      levels: u8::try_from(self.levels).unwrap_or(u8::MAX),
      maybe_overload: self.maybe_reason.map(|reason| match reason {
        OverloadReason::SkippedFrames => EncoderOverload::SkippedFrames,
        OverloadReason::QueuedFrames => EncoderOverload::QueuedFrames,
        OverloadReason::SlowEncoding => EncoderOverload::SlowEncoding,
      }),
      uplink_steps: u8::try_from(self.uplink_steps).unwrap_or(u8::MAX),
      sizes: u8::try_from(self.sizes).unwrap_or(u8::MAX),
    }
  }
}

#[cfg(feature = "call-view")]
impl EncoderKind {
  fn stats(self) -> teamcraft_client_api::video_stats::ShareEncoderKind {
    use teamcraft_client_api::video_stats::ShareEncoderKind;

    match self {
      Self::Hardware => ShareEncoderKind::Hardware,
      Self::Software => ShareEncoderKind::Software,
      Self::Unavailable => ShareEncoderKind::Unavailable,
    }
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::ScreenShareResolution;

  use super::*;

  const QHD: VideoResolution = VideoResolution {
    width: 2560,
    height: 1440,
  };
  const FULL_HD: VideoResolution = VideoResolution {
    width: 1920,
    height: 1080,
  };
  const UHD: VideoResolution = VideoResolution {
    width: 3840,
    height: 2160,
  };

  mod encode_timer {
    use super::*;

    #[test]
    fn a_picture_is_timed_from_its_own_frame() {
      let start = Instant::now();
      let mut timer = EncodeTimer::default();
      // 30 fps: timestamps in fractions of a microsecond, as the capture stamps them.
      let interval = 1_000_000.0 / 30.0;
      timer.on_given(0.0, start);
      timer.on_given(interval, start + Duration::from_millis(33));
      // The picture of the second frame, 10 ms after it was given: not 43 ms after the first.
      let took = timer.on_encoded(interval.trunc(), start + Duration::from_millis(43));
      assert_eq!(took, Some(Duration::from_millis(10)));
    }

    #[test]
    fn frames_the_encoder_dropped_are_forgotten() {
      let start = Instant::now();
      let mut timer = EncodeTimer::default();
      for frame in 0..3u32 {
        timer.on_given(
          f64::from(frame) * 100_000.0,
          start + Duration::from_millis(u64::from(frame) * 100),
        );
      }
      assert_eq!(
        timer.on_encoded(200_000.0, start + Duration::from_millis(205)),
        Some(Duration::from_millis(5))
      );
      assert_eq!(timer.on_encoded(0.0, start + Duration::from_millis(300)), None);
    }

    #[test]
    fn an_unknown_picture_times_nothing() {
      let mut timer = EncodeTimer::default();
      timer.on_given(0.0, Instant::now());
      timer.clear();
      assert_eq!(timer.on_encoded(0.0, Instant::now()), None);
    }
  }

  mod layers {
    use super::*;

    /// A 1440p share to viewers of whom some decode AV1, from a hardware AV1 encoder: H.264 at
    /// 480p, AV1 at 720p and 1440p, all at the full rate but the base.
    #[test]
    fn hardware_av1_sends_av1_on_top_of_an_h264_base() {
      let ladder = ShareLadder::new(plan(QHD, HW, both(VideoCodec::Av1)));
      assert_eq!(ladder.track_codec(ladder.step(0)), VideoCodec::H264);
      assert_eq!(
        shape(&ladder.layers(ladder.step(0))),
        vec![
          (852, 480, 15, None),
          (1280, 720, 30, Some(VideoCodec::Av1)),
          (2560, 1440, 30, Some(VideoCodec::Av1)),
        ]
      );
    }

    /// Where not every viewer decodes AV1, the middle layer is H.264: they get 720p, not 480p.
    #[test]
    fn the_middle_layer_is_h264_where_some_viewer_lacks_av1() {
      let codecs = ShareCodecs {
        by_all: vec![VideoCodec::H264],
        by_some: vec![VideoCodec::Av1, VideoCodec::H264],
      };
      let ladder = ShareLadder::new(plan(QHD, HW, codecs));
      assert_eq!(
        shape(&ladder.layers(ladder.step(0))),
        vec![
          (852, 480, 15, None),
          (1280, 720, 30, None),
          (2560, 1440, 30, Some(VideoCodec::Av1)),
        ]
      );
    }

    /// Software AV1 takes a 1080p share at a reduced rate, but not a 1440p one: that goes out
    /// in H.264 at full size rather than in AV1 at less.
    #[test]
    fn software_av1_only_up_to_1080p_at_a_reduced_rate() {
      let full_hd = ShareLadder::new(plan(FULL_HD, SW, both(VideoCodec::Av1)));
      assert_eq!(
        shape(&full_hd.layers(full_hd.step(0))),
        vec![(960, 540, 15, None), (1920, 1080, 15, Some(VideoCodec::Av1))]
      );
      let qhd = ShareLadder::new(plan(QHD, SW, both(VideoCodec::Av1)));
      assert_eq!(
        shape(&qhd.layers(qhd.step(0))),
        vec![
          (852, 480, 15, None),
          (1280, 720, 30, None),
          (2560, 1440, 30, None)
        ]
      );
    }

    /// Without AV1 among the viewers, or from an SFU that takes one codec per track, nothing
    /// changes codec: the share is one codec's simulcast.
    #[test]
    fn one_codec_without_av1_viewers_or_layer_codecs() {
      let no_av1 = ShareLadder::new(plan(QHD, HW, both(VideoCodec::H264)));
      assert!(
        no_av1
          .layers(no_av1.step(0))
          .iter()
          .all(|info| info.maybe_codec.is_none())
      );
      let older_sfu = ShareCodecs {
        by_all: vec![VideoCodec::Av1, VideoCodec::H264],
        by_some: Vec::new(),
      };
      let ladder = ShareLadder::new(plan(QHD, HW, older_sfu.clone()));
      assert_eq!(ladder.track_codec(ladder.step(0)), VideoCodec::Av1);
      assert!(
        ladder
          .layers(ladder.step(0))
          .iter()
          .all(|info| info.maybe_codec.is_none())
      );
      // Software AV1 can't take 1440p: the whole share goes out in H.264, which all decode.
      let ladder = ShareLadder::new(plan(QHD, SW, older_sfu));
      assert_eq!(ladder.track_codec(ladder.step(0)), VideoCodec::H264);
      assert!(
        ladder
          .layers(ladder.step(0))
          .iter()
          .all(|info| info.maybe_codec.is_none())
      );
    }

    /// H.264 is a codec of its own: a device without a good AV1 encoder sends its full picture
    /// in H.264, at more bitrate than AV1 gets for it.
    #[test]
    fn h264_sends_the_full_picture_at_h264s_bitrate() {
      let av1 = ShareLadder::new(plan(QHD, HW, both(VideoCodec::Av1)));
      let h264 = ShareLadder::new(plan(QHD, NONE_, both(VideoCodec::Av1)));
      let (av1_top, h264_top) = (
        av1.layers(av1.step(0)).pop().unwrap(),
        h264.layers(h264.step(0)).pop().unwrap(),
      );
      assert_eq!(
        (h264_top.width, h264_top.height, h264_top.max_framerate),
        (2560, 1440, 30)
      );
      assert_eq!(h264_top.maybe_codec, None);
      assert_eq!(
        h264_top.max_bitrate_bps,
        screen_bitrate_bps(QHD, 30, &ScreenShareResolution::Standard)
      );
      assert_eq!(
        av1_top.max_bitrate_bps,
        codec_bitrate_bps(h264_top.max_bitrate_bps, &VideoCodec::Av1)
      );
      assert!(av1_top.max_bitrate_bps < h264_top.max_bitrate_bps);
    }

    /// VP9 goes on top where this device encodes it in hardware and AV1 not; a software VP9
    /// encoder is never used.
    #[test]
    fn vp9_only_from_a_hardware_encoder() {
      let codecs = ShareCodecs {
        by_all: vec![VideoCodec::H264],
        by_some: vec![VideoCodec::Av1, VideoCodec::Vp9, VideoCodec::H264],
      };
      let vp9_hardware = ShareEncoders {
        av1: EncoderKind::Software,
        vp9: EncoderKind::Hardware,
      };
      let ladder = ShareLadder::new(plan(QHD, vp9_hardware, codecs.clone()));
      assert_eq!(ladder.step(0).top_codec, VideoCodec::Vp9);
      assert_eq!(ladder.step(0).top_framerate, 30, "hardware: the full rate");
      let vp9_software = ShareEncoders {
        av1: EncoderKind::Unavailable,
        vp9: EncoderKind::Software,
      };
      let ladder = ShareLadder::new(plan(QHD, vp9_software, codecs.clone()));
      assert_eq!(ladder.step(0).top_codec, VideoCodec::H264);
      // Hardware AV1 goes before hardware VP9; nobody decoding VP9 rules it out.
      let both_hardware = ShareEncoders {
        av1: EncoderKind::Hardware,
        vp9: EncoderKind::Hardware,
      };
      assert_eq!(
        ShareLadder::new(plan(QHD, both_hardware, codecs))
          .step(0)
          .top_codec,
        VideoCodec::Av1
      );
      let no_vp9_viewers = ShareEncoders {
        av1: EncoderKind::Unavailable,
        vp9: EncoderKind::Hardware,
      };
      assert_eq!(
        ShareLadder::new(plan(QHD, no_vp9_viewers, both(VideoCodec::Av1)))
          .step(0)
          .top_codec,
        VideoCodec::H264
      );
    }

    /// A small window shared is one layer; a portrait screen keeps its pixels upright.
    #[test]
    fn small_and_portrait_sources() {
      let small = VideoResolution {
        width: 800,
        height: 450,
      };
      let ladder = ShareLadder::new(plan(small, HW, both(VideoCodec::Av1)));
      assert_eq!(
        shape(&ladder.layers(ladder.step(0))),
        vec![(800, 450, 30, Some(VideoCodec::Av1))]
      );
      assert_eq!(ladder.layers(ladder.steps().last().unwrap()).len(), 1);
      let portrait = VideoResolution {
        width: 1440,
        height: 2560,
      };
      let ladder = ShareLadder::new(plan(portrait, HW, both(VideoCodec::Av1)));
      let layers = ladder.layers(ladder.step(0));
      assert_eq!((layers[0].width, layers[0].height), (480, 852));
      assert_eq!((layers[2].width, layers[2].height), (1440, 2560));
    }
  }

  /// The top layer's sizes keep the capture's aspect, at encoder-friendly sizes.
  mod sizes {
    use super::*;

    /// The base is an integer fraction of the top at about 480p: one-pixel moves of a scrolled
    /// page stay whole pixels in it.
    #[test]
    fn the_base_is_an_integer_fraction_of_the_top() {
      let base = |width, height| {
        let size = base_size(VideoResolution { width, height });
        (size.width, size.height)
      };
      assert_eq!(base(2560, 1440), (852, 480));
      assert_eq!(base(1920, 1080), (960, 540));
      assert_eq!(base(3840, 2160), (960, 540));
      assert_eq!(base(1280, 720), (640, 360));
      assert_eq!(base(1440, 2560), (480, 852));
      assert_eq!(base(800, 450), (800, 450), "small: no base below it");
    }

    #[test]
    fn a_4k_share_steps_through_every_size_above_the_base() {
      assert_eq!(
        sides(&top_sizes(UHD)),
        vec![(3840, 2160), (2560, 1440), (1920, 1080), (1600, 900), (1280, 720)]
      );
    }

    #[test]
    fn sizes_keep_a_4_3_or_ultrawide_capture_aspect() {
      let four_three = VideoResolution {
        width: 1600,
        height: 1200,
      };
      assert_eq!(
        sides(&top_sizes(four_three)),
        vec![(1600, 1200), (1440, 1080), (1200, 900), (960, 720)]
      );
      let ultrawide_top = VideoResolution {
        width: 2560,
        height: 1072,
      };
      let sizes = top_sizes(ultrawide_top);
      assert_eq!(sides(&sizes), vec![(2560, 1072), (2144, 900), (1712, 720)]);
      for size in &sizes[1..] {
        assert_eq!(size.width % SIDE_ALIGNMENT, 0, "{size:?}");
        assert_eq!(size.height % 2, 0, "{size:?}");
      }
      let portrait = VideoResolution {
        width: 1440,
        height: 2560,
      };
      assert_eq!(
        sides(&top_sizes(portrait)),
        vec![(1440, 2560), (1080, 1920), (900, 1600), (720, 1280)]
      );
    }

    /// A 1080p share steps to 900p and 720p; a 720p share has no smaller size above the base.
    #[test]
    fn small_shares_have_few_or_no_size_steps() {
      assert_eq!(
        sides(&top_sizes(FULL_HD)),
        vec![(1920, 1080), (1600, 900), (1280, 720)]
      );
      let hd = VideoResolution {
        width: 1280,
        height: 720,
      };
      assert_eq!(sides(&top_sizes(hd)), vec![(1280, 720)]);
    }

    fn sides(sizes: &[VideoResolution]) -> Vec<(u16, u16)> {
      sizes.iter().map(|size| (size.width, size.height)).collect()
    }
  }

  mod ladder {
    use super::*;

    /// On a limited uplink the share keeps its pictures' quality and lowers its rate.
    #[test]
    fn the_frame_rate_follows_the_uplinks_share_of_the_bitrate() {
      let ladder = ShareLadder::new(plan(QHD, HW, both(VideoCodec::Av1)));
      let top = ladder.layers(ladder.step(0)).pop().unwrap();
      assert_eq!(share_framerate(&top, top.max_bitrate_bps), 30);
      assert_eq!(share_framerate(&top, top.max_bitrate_bps * 6 / 10), 18);
      assert_eq!(share_framerate(&top, top.max_bitrate_bps / 5), 6);
      assert_eq!(share_framerate(&top, 0), MIN_SHARE_FRAMERATE);
      assert_eq!(share_framerate(&top, u32::MAX), 30);
    }

    /// The agreed order under CPU pressure for a 1080p share in software AV1: frame rate down
    /// to 5 fps, AV1 to H.264, then size step by step at 5 fps, and only then the base alone.
    #[test]
    fn detail_steps_down_rate_codec_size_then_the_base_alone() {
      let ladder = ShareLadder::new(plan(FULL_HD, SW, both(VideoCodec::Av1)));
      let top = |width, height| Some(VideoResolution { width, height });
      assert_eq!(
        summary(&ladder),
        vec![
          (15, false, VideoCodec::Av1, top(1920, 1080)),
          (10, false, VideoCodec::Av1, top(1920, 1080)),
          (5, false, VideoCodec::Av1, top(1920, 1080)),
          (5, false, VideoCodec::H264, top(1920, 1080)),
          (5, false, VideoCodec::H264, top(1600, 900)),
          (5, false, VideoCodec::H264, top(1280, 720)),
          (5, false, VideoCodec::H264, None),
        ]
      );
      for step in ladder.steps() {
        let layers = ladder.layers(step);
        // Half of 1080p: an integer step below the top.
        assert_eq!(
          (layers[0].width, layers[0].height),
          (960, 540),
          "base at {step:?}"
        );
        assert_eq!(layers[0].maybe_codec, None);
      }
      assert_eq!(ladder.layers(ladder.steps().last().unwrap()).len(), 1);
    }

    /// The failure seen on Windows (#1733): a machine without AV1 hardware dropped its 1440p
    /// layer and sent 720p at 27 fps. Along the ladder the top keeps 1440p until the rate is at
    /// 5 fps and the middle layer is off, then steps down 1080p, 900p and 720p before the base
    /// layer is all that is left: no jump to 480p while a larger size is there.
    #[test]
    fn a_1440p_share_keeps_its_size_until_the_rate_is_at_the_floor() {
      let ladder = ShareLadder::new(plan(QHD, SW, both(VideoCodec::Av1)));
      let rates: Vec<u8> = ladder.steps().iter().map(|step| step.top_framerate).collect();
      assert_eq!(rates[..5], [30, 20, 15, 10, 5]);
      assert!(ladder.steps()[..5].iter().all(|step| step.has_middle));
      let tops: Vec<Option<u16>> = ladder
        .steps()
        .iter()
        .map(|step| step.maybe_top.map(lines))
        .collect();
      assert_eq!(
        tops,
        vec![
          Some(1440),
          Some(1440),
          Some(1440),
          Some(1440),
          Some(1440),
          Some(1440),
          Some(1080),
          Some(900),
          Some(720),
          None
        ]
      );
      let smaller = ladder.steps().iter().skip(6);
      assert!(
        smaller
          .clone()
          .all(|step| step.top_framerate == MIN_SHARE_FRAMERATE && !step.has_middle)
      );
    }

    /// Hardware AV1 never falls back to H.264 for CPU's sake: the codec isn't the load.
    #[test]
    fn hardware_av1_keeps_its_codec() {
      let ladder = ShareLadder::new(plan(QHD, HW, both(VideoCodec::Av1)));
      assert!(
        ladder
          .steps()
          .iter()
          .all(|step| step.top_codec == VideoCodec::Av1)
      );
      assert_eq!(ladder.step(0).top_framerate, 30);
      assert_eq!(ladder.step(usize::MAX), ladder.steps().last().unwrap());
    }

    /// A motion share keeps its rate longest: middle and size go first.
    #[test]
    fn motion_steps_down_size_before_rate() {
      let mut plan = plan(QHD, HW, both(VideoCodec::H264));
      plan.profile.mode = ScreenContentMode::Motion;
      plan.framerate = 60;
      let ladder = ShareLadder::new(plan);
      let rates: Vec<u8> = ladder.steps().iter().map(|step| step.top_framerate).collect();
      assert_eq!(rates.first(), Some(&60));
      assert_eq!(rates.last(), Some(&30));
      let before_rate = &ladder.steps()[ladder.len() - 3];
      assert_eq!(before_rate.maybe_top.map(lines), Some(720));
      assert_eq!(before_rate.top_framerate, 60);
    }

    fn summary(ladder: &ShareLadder) -> Vec<(u8, bool, VideoCodec, Option<VideoResolution>)> {
      ladder
        .steps()
        .iter()
        .map(|step| {
          (
            step.top_framerate,
            step.has_middle,
            step.top_codec.clone(),
            step.maybe_top,
          )
        })
        .collect()
    }
  }

  /// The share's governor (#1733, the comment of 2026-11-11): overloaded windows walk the ladder
  /// down in its order, a calm period walks it back up in reverse, and a step up that doesn't
  /// hold makes the next wait longer. The uplink steps the top's size likewise.
  mod governor {
    use super::*;

    #[test]
    fn overload_steps_down_in_the_ladders_order_keeping_the_base() {
      let start = Instant::now();
      let mut governor = ShareGovernor::new(plan(QHD, SW, both(VideoCodec::Av1)), start);
      let mut now = start;
      let mut seen = vec![governor.step()];
      while governor.level() + 1 < governor.ladder().len() {
        now = window(&mut governor, now, overloaded());
        assert!(
          governor.evaluate(now).is_some(),
          "a step down per overloaded window"
        );
        seen.push(governor.step());
        let layers = governor.layers();
        assert_eq!((layers[0].width, layers[0].height), (852, 480));
      }
      assert_eq!(seen, governor.ladder().steps());
      assert_eq!(
        governor.limits().maybe_reason,
        Some(OverloadReason::SkippedFrames)
      );
      now = window(&mut governor, now, overloaded());
      assert_eq!(governor.evaluate(now), None, "nothing below the last step");
    }

    /// Encoding that takes most of the frame interval counts as overload too, also with an
    /// empty queue: the browser's encoder falls behind the capture.
    #[test]
    fn slow_encoding_steps_down() {
      let start = Instant::now();
      let mut governor = ShareGovernor::new(plan(QHD, HW, both(VideoCodec::Av1)), start);
      let slow = FrameTally {
        given: 1,
        encode_time: Duration::from_millis(32),
        frame_interval: Duration::from_millis(33),
        ..FrameTally::default()
      };
      // Within the frame interval: a second window decides (#1792).
      let now = window(&mut governor, start, slow);
      assert_eq!(governor.evaluate(now), None);
      let now = window(&mut governor, now, slow);
      assert_eq!(governor.evaluate(now), Some(1));
      assert_eq!(governor.step().top_framerate, 20);
      assert_eq!(governor.limits().maybe_reason, Some(OverloadReason::SlowEncoding));
    }

    /// Recovery runs in reverse: from the smallest size up the sizes first, then the codec and
    /// middle layer, and the frame rate last; each step after a calm period, a step up that
    /// didn't hold doubling the next wait.
    #[test]
    fn calm_recovers_in_reverse_size_first_with_hysteresis_and_backoff() {
      let start = Instant::now();
      let mut governor = ShareGovernor::new(plan(QHD, HW, both(VideoCodec::Av1)), start);
      let mut now = start;
      while governor.level() + 1 < governor.ladder().len() {
        now = window(&mut governor, now, overloaded());
        governor.evaluate(now);
      }
      let mut ups = vec![governor.step()];
      let calm_period = CameraGovernorConfig::default().calm_period;
      let calm_since = now;
      let mut first_up = None;
      while governor.level() > 0 {
        now = window(&mut governor, now, calm());
        if governor.evaluate(now).is_some() {
          first_up.get_or_insert(now.saturating_duration_since(calm_since));
          ups.push(governor.step());
        }
      }
      assert!(first_up.unwrap() >= calm_period);
      // #1792: within about 5 s of the machine getting idle, not minutes.
      assert!(first_up.unwrap() <= Duration::from_secs(5), "{first_up:?}");
      let mut down: Vec<ShareStep> = governor.ladder().steps().to_vec();
      down.reverse();
      assert_eq!(ups, down, "back up in reverse");
      // The sizes come back before the frame rate does.
      let first_rate_up = ups
        .iter()
        .position(|step| step.top_framerate > MIN_SHARE_FRAMERATE)
        .unwrap();
      assert!(
        ups[..first_rate_up]
          .iter()
          .any(|step| step.maybe_top.map(lines) == Some(720))
      );
      assert_eq!(ups[first_rate_up - 1].maybe_top.map(lines), Some(1440));
      // A step down right after a step up: the next one waits twice as long.
      now = window(&mut governor, now, overloaded());
      assert_eq!(governor.evaluate(now), Some(1));
      let calm_since = now;
      let mut waited = None;
      while waited.is_none() {
        now = window(&mut governor, now, calm());
        if governor.evaluate(now).is_some() {
          waited = Some(now.saturating_duration_since(calm_since));
        }
      }
      assert!(waited.unwrap() >= calm_period * 2, "{waited:?}");
      // Failed steps make the next wait longer, but seconds, not minutes.
      let max_calm_period = CameraGovernorConfig::default().max_calm_period;
      let load_window = CameraGovernorConfig::default().load_window;
      assert!(waited.unwrap() <= max_calm_period + load_window, "{waited:?}");
    }

    /// A limited uplink: the top layer at its least share is still too much. Its size steps
    /// down one at a time, never to the base while a larger size is left, and comes back size
    /// by size once the uplink carried it whole for the calm period.
    #[test]
    fn the_uplink_steps_the_top_size_down_one_at_a_time_and_back() {
      let start = Instant::now();
      let mut governor = ShareGovernor::new(plan(UHD_SHARE, HW, both(VideoCodec::Av1)), start);
      let config = CameraGovernorConfig::default();
      let mut now = start;
      let mut tops = vec![top_lines(&governor)];
      for _ in 0..10 {
        governor.on_uplink(UplinkTop::Held, now);
        now += config.load_window;
        if governor.on_uplink(UplinkTop::Held, now) {
          tops.push(top_lines(&governor));
        }
      }
      assert_eq!(
        tops,
        vec![2160, 1440, 1080, 900, 720],
        "the smallest size, not the base"
      );
      let layers = governor.layers();
      assert_eq!(
        layers.first().map(|info| info.height),
        Some(540),
        "a quarter of 2160p"
      );
      assert_eq!(layers.last().map(|info| info.height), Some(720));
      // A reduced top (a lower rate) is no reason to step up.
      assert!(!governor.on_uplink(UplinkTop::Reduced, now + config.calm_period * 2));
      now += config.calm_period * 2;
      let mut ups = Vec::new();
      for _ in 0..60 {
        now += Duration::from_secs(1);
        if governor.on_uplink(UplinkTop::Whole, now) {
          ups.push(top_lines(&governor));
        }
      }
      assert_eq!(ups, vec![900, 1080, 1440, 2160]);
    }

    /// #1846: while the encoders are overloaded, a held top layer is the starved encoder's bursts,
    /// not the uplink: the size stays, and the CPU's steps lower what is sent.
    #[test]
    fn an_overloaded_encoder_doesnt_step_the_uplink_size() {
      let start = Instant::now();
      let mut governor = ShareGovernor::new(plan(QHD, HW, both(VideoCodec::Av1)), start);
      let config = CameraGovernorConfig::default();
      let mut now = window(&mut governor, start, overloaded());
      now = window(&mut governor, now, overloaded());
      governor.evaluate(now);
      assert!(governor.limits().maybe_reason.is_some(), "overloaded");
      let before = top_lines(&governor);
      for _ in 0..10 {
        now += config.load_window;
        assert!(!governor.on_uplink(UplinkTop::Held, now));
      }
      assert_eq!(top_lines(&governor), before);
      assert_eq!(governor.limits().uplink_steps, 0);
    }

    /// The uplink's size and the CPU's step combine: the smaller top wins.
    #[test]
    fn the_smaller_of_the_cpu_and_uplink_sizes_is_sent() {
      let start = Instant::now();
      let mut governor = ShareGovernor::new(plan(QHD, HW, both(VideoCodec::Av1)), start);
      let config = CameraGovernorConfig::default();
      governor.on_uplink(UplinkTop::Held, start);
      assert!(governor.on_uplink(UplinkTop::Held, start + config.load_window));
      assert_eq!(top_lines(&governor), 1080);
      assert_eq!(governor.step().top_framerate, 30, "the CPU keeps the rate");
      let limits = governor.limits();
      assert_eq!((limits.uplink_steps, limits.sizes), (1, 4));
    }

    /// New codecs (a viewer without AV1 joined) keep the load's level where the ladder keeps
    /// its length.
    #[test]
    fn a_replan_keeps_the_level_where_it_can() {
      let start = Instant::now();
      let mut governor = ShareGovernor::new(plan(QHD, HW, both(VideoCodec::Av1)), start);
      let now = window(&mut governor, start, overloaded());
      governor.evaluate(now);
      let codecs = ShareCodecs {
        by_all: vec![VideoCodec::H264],
        by_some: vec![VideoCodec::Av1, VideoCodec::H264],
      };
      governor.replan(plan(QHD, HW, codecs), now);
      assert_eq!(governor.level(), 1);
      assert_eq!(
        governor.layers()[1].maybe_codec,
        None,
        "the middle layer in H.264"
      );
    }

    const UHD_SHARE: VideoResolution = UHD;

    fn top_lines(governor: &ShareGovernor) -> u16 {
      governor.step().maybe_top.map_or(0, lines)
    }

    /// Feeds a load window of 30 fps frames of `tally`; the time at its end.
    fn window(governor: &mut ShareGovernor, from: Instant, tally: FrameTally) -> Instant {
      let config = CameraGovernorConfig::default();
      for _ in 0..(config.load_window.as_millis() / 33) {
        governor.on_frame(tally);
      }
      from + config.load_window
    }

    fn overloaded() -> FrameTally {
      FrameTally {
        given: 1,
        skipped: 2,
        ..FrameTally::default()
      }
    }

    fn calm() -> FrameTally {
      FrameTally {
        given: 3,
        ..FrameTally::default()
      }
    }
  }

  /// AV1 in hardware, in software, not at all; VP9 not in hardware.
  const HW: ShareEncoders = ShareEncoders {
    av1: EncoderKind::Hardware,
    vp9: EncoderKind::Unavailable,
  };
  const SW: ShareEncoders = ShareEncoders {
    av1: EncoderKind::Software,
    vp9: EncoderKind::Unavailable,
  };
  const NONE_: ShareEncoders = ShareEncoders {
    av1: EncoderKind::Unavailable,
    vp9: EncoderKind::Unavailable,
  };

  fn plan(source: VideoResolution, encoders: ShareEncoders, codecs: ShareCodecs) -> SharePlan {
    let size = if lines(source) > 1440 {
      ScreenShareResolution::Uhd
    } else {
      ScreenShareResolution::Standard
    };
    SharePlan {
      source,
      framerate: 30,
      profile: ScreenProfile {
        mode: ScreenContentMode::Detail,
        size,
      },
      codecs,
      encoders,
    }
  }

  /// Everyone decodes `codec` and H.264, and the SFU forwards layers in another codec.
  fn both(codec: VideoCodec) -> ShareCodecs {
    let codecs = if codec == VideoCodec::H264 {
      vec![codec]
    } else {
      vec![codec, VideoCodec::H264]
    };
    ShareCodecs {
      by_all: codecs.clone(),
      by_some: codecs,
    }
  }

  fn shape(layers: &[VideoLayerInfo]) -> Vec<(u16, u16, u8, Option<VideoCodec>)> {
    layers
      .iter()
      .map(|info| {
        (
          info.width,
          info.height,
          info.max_framerate,
          info.maybe_codec.clone(),
        )
      })
      .collect()
  }
}
