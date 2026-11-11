//! How a screen share is encoded (#1733): which layers, in which codec, at which size and rate.
//!
//! Meetings share text, not games. Under CPU or bandwidth pressure a detail share keeps its
//! resolution and lowers its frame rate; a higher rate comes only with headroom. The layers:
//!
//! - **Base:** H.264 at about 480p, always sent. Every device decodes it, so a viewer without AV1
//!   or on a weak link gets it without the publisher noticing (the SFU picks per viewer).
//! - **Middle:** half the top's size, where that is clearly larger than the base (from 1440p).
//!   In the top's codec where every viewer decodes it, else H.264.
//! - **Top:** the share's full picture. AV1 where this device encodes AV1 in hardware; in software
//!   only up to about 1080p and at a reduced rate, else H.264.
//!
//! A [`ShareLadder`] lists what the encoders get, from the full plan down to the least the share
//! is sent at, in the order the issue agreed for CPU pressure:
//!
//! 1. the top layer's frame rate, down to about 5 fps;
//! 2. the middle layer off;
//! 3. a top layer in software AV1 to H.264 at the same size;
//! 4. only then the top layer's size.
//!
//! The base layer stays throughout. A controller of the encoders' load (frames queued or skipped,
//! encode time against the frame interval) walks the ladder down and, after calm, back up in the
//! reverse order. Pure policy: the driver feeds the inputs and builds encoders from the layers.

use teamcraft_proto::{ScreenContentMode, VideoCodec, VideoLayerInfo};

use crate::video::{ScreenProfile, VideoResolution, fit_within, screen_bitrate_bps};

/// The base layer's largest picture: 480p, turned upright for a portrait source.
const BASE_LIMIT: VideoResolution = VideoResolution {
  width: 854,
  height: 480,
};
/// The base layer's frame rate for a detail share: text needs no more.
const BASE_DETAIL_FRAMERATE: u8 = 15;
/// Bits per pixel of the base layer: H.264 at 480p needs more per pixel than the large layers to
/// keep small text readable at all.
const BASE_BITS_PER_PIXEL: f64 = 0.08;
const BASE_MIN_BITRATE_BPS: u32 = 300_000;
/// A middle layer exists where it has at least this many times the base layer's lines: a 1080p
/// share's half (540p) is hardly more than the base, a 1440p share's (720p) is.
const MIDDLE_MIN_LINES_OVER_BASE: f64 = 1.4;
/// The least frame rate a share is sent at, for the CPU and the uplink alike ("about 5 fps").
pub const MIN_SHARE_FRAMERATE: u8 = 5;
/// The frame rates a detail share's top layer steps down through, highest first; the least is
/// [`MIN_SHARE_FRAMERATE`].
const DETAIL_TOP_FRAMERATES: [u8; 5] = [30, 20, 15, 10, 5];
/// A top layer in software AV1 starts no higher than this ("reduced fps").
const SOFTWARE_AV1_MAX_FRAMERATE: u8 = 15;
/// A top layer in software AV1 has at most this many pixels (1080p and a little): above, the
/// browser's AV1 encoder fell to about 9 fps at 1440p on a loaded machine.
const SOFTWARE_AV1_MAX_PIXELS: u32 = 1920 * 1080 * 11 / 10;
/// What the top layer's size steps down through as the last resort, in per mille of its full
/// size.
const TOP_SCALES_PER_MILLE: [u16; 3] = [1000, 750, 500];

/// How this device encodes AV1, as far as the platform tells (WebCodecs: `isConfigSupported` and
/// `MediaCapabilities.encodingInfo().powerEfficient`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Av1Encoding {
  /// A hardware encoder: AV1 at full size and rate.
  Hardware,
  /// Software only: AV1 up to about 1080p at a reduced rate.
  Software,
  /// No AV1 encoder.
  Unavailable,
}

/// The codecs a share's layers may use, from what the other devices decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareCodecs {
  /// Decoded by every viewer: the base layer's, and the middle's where the top's isn't.
  pub by_all: VideoCodec,
  /// The best codec some viewers decode as a layer in another codec than its track's
  /// ([`crate::call_engine::CallEngine::layer_codec_for`]); `None` where the SFU forwards no such
  /// layers, and the whole share then goes out in `by_all`.
  pub maybe_by_some: Option<VideoCodec>,
}

/// What a share is planned from: what is captured and how, and what the codecs allow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharePlan {
  pub source: VideoResolution,
  /// The rate the screen is captured at.
  pub framerate: u8,
  pub profile: ScreenProfile,
  pub codecs: ShareCodecs,
  pub av1: Av1Encoding,
}

/// What the share's encoders get at one step of the [`ShareLadder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareStep {
  pub top_codec: VideoCodec,
  pub top_framerate: u8,
  /// The top layer's size in per mille of its full size.
  pub top_scale_per_mille: u16,
  pub has_middle: bool,
}

/// Every step a share may be sent at, the full plan first ([`ShareLadder::steps`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareLadder {
  plan: SharePlan,
  steps: Vec<ShareStep>,
}

/// Why a share's top layer is what it is (stats for nerds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareLimit {
  /// Sent as planned.
  Unlimited,
  /// The encoders fell behind: the ladder stepped down (`level` steps of [`ShareLadder::len`]).
  Cpu { level: usize },
}

impl ShareLadder {
  /// The steps for `plan`, the full plan first.
  pub fn new(plan: SharePlan) -> Self {
    let steps = ladder_steps(&plan);
    Self { plan, steps }
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

  /// The step at `level` (0: the full plan), the last one beyond.
  pub fn step(&self, level: usize) -> &ShareStep {
    // Invariant: `ladder_steps` always yields the full plan, so `last` is there.
    &self.steps[level.min(self.steps.len().saturating_sub(1))]
  }

  /// The codec the share is published in at `level`: its base layer's. It changes with the
  /// level only where the SFU takes one codec per track (the whole share falls back to H.264).
  pub fn track_codec(&self, level: usize) -> VideoCodec {
    base_codec(&self.plan.codecs, self.step(level))
  }

  /// The layers at `level`, lowest first, each layer in another codec than
  /// [`Self::track_codec`] naming it.
  pub fn layers(&self, level: usize) -> Vec<VideoLayerInfo> {
    share_layers(&self.plan, self.step(level))
  }
}

/// The base layer's codec: H.264 where the SFU forwards layers in other codecs (every device
/// decodes it), else the one codec of the whole share, the top's.
fn base_codec(codecs: &ShareCodecs, step: &ShareStep) -> VideoCodec {
  match codecs.maybe_by_some {
    Some(_) => VideoCodec::H264,
    None => step.top_codec.clone(),
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

/// The steps of [`ShareLadder::new`].
fn ladder_steps(plan: &SharePlan) -> Vec<ShareStep> {
  let full_top = full_top_size(plan);
  let top_codec = planned_top_codec(plan, full_top);
  let is_software_av1 = top_codec == VideoCodec::Av1 && plan.av1 != Av1Encoding::Hardware;
  let framerate = plan.framerate.clamp(1, plan.profile.limits().1);
  let max_framerate = if is_software_av1 {
    framerate.min(SOFTWARE_AV1_MAX_FRAMERATE)
  } else {
    framerate
  };
  let has_middle = middle_size(full_top, plan.source).is_some();
  let full = ShareStep {
    top_codec: top_codec.clone(),
    top_framerate: max_framerate,
    top_scale_per_mille: 1000,
    has_middle,
  };
  match plan.profile.mode {
    ScreenContentMode::Motion => motion_steps(full, plan),
    ScreenContentMode::Detail | ScreenContentMode::Unknown => detail_steps(full, plan, is_software_av1),
  }
}

/// A detail share: frame rate, middle layer, software AV1, then size.
fn detail_steps(full: ShareStep, plan: &SharePlan, is_software_av1: bool) -> Vec<ShareStep> {
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
  push_smaller_tops(&mut steps, step, plan);
  steps
}

/// A motion share (#530, chosen for video and animation): it keeps its rate longest, so the
/// middle layer goes first, then the size, then the rate down to half.
fn motion_steps(full: ShareStep, plan: &SharePlan) -> Vec<ShareStep> {
  let mut steps = vec![full.clone()];
  let mut step = full;
  if step.has_middle {
    step.has_middle = false;
    steps.push(step.clone());
  }
  push_smaller_tops(&mut steps, step.clone(), plan);
  let mut step = steps.last().cloned().unwrap_or(step);
  let half = (step.top_framerate / 2).max(1);
  if half < step.top_framerate {
    step.top_framerate = half;
    steps.push(step);
  }
  steps
}

/// The top layer's size steps below full, while the top stays clearly larger than the base.
fn push_smaller_tops(steps: &mut Vec<ShareStep>, mut step: ShareStep, plan: &SharePlan) {
  let full_top = full_top_size(plan);
  let base = base_size(plan.source);
  for scale in TOP_SCALES_PER_MILLE.into_iter().skip(1) {
    let top = scaled(full_top, scale);
    if f64::from(top.height.min(top.width))
      < f64::from(base.height.min(base.width)) * MIDDLE_MIN_LINES_OVER_BASE
    {
      break;
    }
    step.top_scale_per_mille = scale;
    steps.push(step.clone());
  }
}

/// The top layer's codec in the full plan: the best codec some viewer decodes, where this device
/// encodes it well enough at this size; else what everyone decodes.
fn planned_top_codec(plan: &SharePlan, full_top: VideoResolution) -> VideoCodec {
  let wanted = plan
    .codecs
    .maybe_by_some
    .clone()
    .unwrap_or_else(|| plan.codecs.by_all.clone());
  // H.264, which every device decodes, is always possible.
  if wanted != VideoCodec::Av1 || !plan.profile.codecs().contains(&VideoCodec::Av1) {
    return if plan.profile.codecs().contains(&wanted) {
      wanted
    } else {
      VideoCodec::H264
    };
  }
  let pixels = u32::from(full_top.width) * u32::from(full_top.height);
  match plan.av1 {
    Av1Encoding::Hardware => VideoCodec::Av1,
    Av1Encoding::Software if pixels <= SOFTWARE_AV1_MAX_PIXELS => VideoCodec::Av1,
    Av1Encoding::Software | Av1Encoding::Unavailable => VideoCodec::H264,
  }
}

/// The layers of `plan` at `step`, lowest first.
fn share_layers(plan: &SharePlan, step: &ShareStep) -> Vec<VideoLayerInfo> {
  let base_codec = base_codec(&plan.codecs, step);
  let size = &plan.profile.size;
  let full_top = full_top_size(plan);
  let top = scaled(full_top, step.top_scale_per_mille);
  let base = base_size(plan.source);
  let mut layers = Vec::new();
  let is_detail = plan.profile.mode != ScreenContentMode::Motion;
  if top.height > base.height || top.width > base.width {
    let framerate = if is_detail {
      step.top_framerate.min(BASE_DETAIL_FRAMERATE).max(1)
    } else {
      step.top_framerate
    };
    layers.push((
      base,
      framerate,
      base_bitrate_bps(base, framerate),
      base_codec.clone(),
    ));
  }
  if step.has_middle
    && let Some(middle) = middle_size(full_top, plan.source)
    && middle.height < top.height
  {
    // Without layer codecs the base's codec is the top's, and so is the middle's.
    let middle_codec = if step.top_codec == plan.codecs.by_all {
      step.top_codec.clone()
    } else {
      base_codec.clone()
    };
    let bitrate = screen_bitrate_bps(top, step.top_framerate, size) / 4;
    layers.push((middle, step.top_framerate, bitrate, middle_codec));
  }
  let top_bitrate = screen_bitrate_bps(top, step.top_framerate, size);
  layers.push((top, step.top_framerate, top_bitrate, step.top_codec.clone()));
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
        maybe_codec: (codec != base_codec).then_some(codec),
      },
    )
    .collect()
}

/// The top layer's full size: the source within the profile's limit.
fn full_top_size(plan: &SharePlan) -> VideoResolution {
  let (limit, _) = plan.profile.limits();
  fit_within(plan.source, oriented_like(limit, plan.source))
}

/// The base layer's size: the source within 480p.
fn base_size(source: VideoResolution) -> VideoResolution {
  fit_within(source, oriented_like(BASE_LIMIT, source))
}

/// The middle layer's size, half the top's, where that is clearly larger than the base.
fn middle_size(full_top: VideoResolution, source: VideoResolution) -> Option<VideoResolution> {
  let middle = scaled(full_top, 500);
  let base = base_size(source);
  let lines = |size: VideoResolution| f64::from(size.height.min(size.width));
  (lines(middle) >= lines(base) * MIDDLE_MIN_LINES_OVER_BASE).then_some(middle)
}

/// `size` at `per_mille` of each side, with even sides.
fn scaled(size: VideoResolution, per_mille: u16) -> VideoResolution {
  let side = |value: u16| {
    let scaled = u32::from(value) * u32::from(per_mille) / 1000;
    u16::try_from(scaled & !1).unwrap_or(value).max(2)
  };
  VideoResolution {
    width: side(size.width),
    height: side(size.height),
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

  mod layers {
    use super::*;

    /// A 1440p share to viewers of whom some decode AV1, from a hardware AV1 encoder: H.264 at
    /// 480p, AV1 at 720p and 1440p, all at the full rate but the base.
    #[test]
    fn hardware_av1_sends_av1_on_top_of_an_h264_base() {
      let ladder = ShareLadder::new(plan(QHD, Av1Encoding::Hardware, both(VideoCodec::Av1)));
      assert_eq!(ladder.track_codec(0), VideoCodec::H264);
      assert_eq!(
        shape(&ladder.layers(0)),
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
        by_all: VideoCodec::H264,
        maybe_by_some: Some(VideoCodec::Av1),
      };
      let ladder = ShareLadder::new(plan(QHD, Av1Encoding::Hardware, codecs));
      assert_eq!(
        shape(&ladder.layers(0)),
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
      let full_hd = ShareLadder::new(plan(FULL_HD, Av1Encoding::Software, both(VideoCodec::Av1)));
      assert_eq!(
        shape(&full_hd.layers(0)),
        vec![(852, 480, 15, None), (1920, 1080, 15, Some(VideoCodec::Av1))]
      );
      let qhd = ShareLadder::new(plan(QHD, Av1Encoding::Software, both(VideoCodec::Av1)));
      assert_eq!(
        shape(&qhd.layers(0)),
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
      let no_av1 = ShareLadder::new(plan(QHD, Av1Encoding::Hardware, both(VideoCodec::H264)));
      assert!(no_av1.layers(0).iter().all(|info| info.maybe_codec.is_none()));
      let older_sfu = ShareCodecs {
        by_all: VideoCodec::Av1,
        maybe_by_some: None,
      };
      let ladder = ShareLadder::new(plan(QHD, Av1Encoding::Hardware, older_sfu.clone()));
      assert_eq!(ladder.track_codec(0), VideoCodec::Av1);
      assert!(ladder.layers(0).iter().all(|info| info.maybe_codec.is_none()));
      // Software AV1 can't take 1440p: the whole share goes out in H.264, which all decode.
      let ladder = ShareLadder::new(plan(QHD, Av1Encoding::Software, older_sfu));
      assert_eq!(ladder.track_codec(0), VideoCodec::H264);
      assert!(ladder.layers(0).iter().all(|info| info.maybe_codec.is_none()));
    }

    /// A small window shared is one layer; a portrait screen keeps its pixels upright.
    #[test]
    fn small_and_portrait_sources() {
      let small = VideoResolution {
        width: 800,
        height: 450,
      };
      let ladder = ShareLadder::new(plan(small, Av1Encoding::Hardware, both(VideoCodec::Av1)));
      assert_eq!(
        shape(&ladder.layers(0)),
        vec![(800, 450, 30, Some(VideoCodec::Av1))]
      );
      let portrait = VideoResolution {
        width: 1440,
        height: 2560,
      };
      let ladder = ShareLadder::new(plan(portrait, Av1Encoding::Hardware, both(VideoCodec::Av1)));
      let layers = ladder.layers(0);
      assert_eq!((layers[0].width, layers[0].height), (480, 852));
      assert_eq!((layers[2].width, layers[2].height), (1440, 2560));
    }
  }

  mod ladder {
    use super::*;

    /// On a limited uplink the share keeps its pictures' quality and lowers its rate.
    #[test]
    fn the_frame_rate_follows_the_uplinks_share_of_the_bitrate() {
      let ladder = ShareLadder::new(plan(QHD, Av1Encoding::Hardware, both(VideoCodec::Av1)));
      let top = ladder.layers(0).pop().unwrap();
      assert_eq!(share_framerate(&top, top.max_bitrate_bps), 30);
      assert_eq!(share_framerate(&top, top.max_bitrate_bps * 6 / 10), 18);
      assert_eq!(share_framerate(&top, top.max_bitrate_bps / 5), 6);
      assert_eq!(share_framerate(&top, 0), MIN_SHARE_FRAMERATE);
      assert_eq!(share_framerate(&top, u32::MAX), 30);
    }

    /// The agreed order under CPU pressure: frame rate down to 5 fps, software AV1 to H.264,
    /// and only then a smaller top (a 1080p share has no middle layer). The base layer stays
    /// throughout.
    #[test]
    fn detail_steps_down_rate_codec_then_size() {
      let ladder = ShareLadder::new(plan(FULL_HD, Av1Encoding::Software, both(VideoCodec::Av1)));
      let steps: Vec<(u8, bool, VideoCodec, u16)> = ladder
        .steps()
        .iter()
        .map(|step| {
          (
            step.top_framerate,
            step.has_middle,
            step.top_codec.clone(),
            step.top_scale_per_mille,
          )
        })
        .collect();
      assert_eq!(
        steps,
        vec![
          (15, false, VideoCodec::Av1, 1000),
          (10, false, VideoCodec::Av1, 1000),
          (5, false, VideoCodec::Av1, 1000),
          (5, false, VideoCodec::H264, 1000),
          (5, false, VideoCodec::H264, 750),
        ]
      );
      for level in 0..ladder.len() {
        let layers = ladder.layers(level);
        assert_eq!((layers[0].width, layers[0].height), (852, 480), "base at {level}");
        assert_eq!(layers[0].maybe_codec, None);
      }
    }

    /// The failure seen on Windows (#1733): a machine without AV1 hardware dropped its 1440p
    /// layer and sent 720p at 27 fps. Along the ladder the top keeps 1440p until the rate is at
    /// 5 fps and the middle layer is off.
    #[test]
    fn a_1440p_share_keeps_its_size_until_the_rate_is_at_the_floor() {
      let ladder = ShareLadder::new(plan(QHD, Av1Encoding::Software, both(VideoCodec::Av1)));
      let rates: Vec<u8> = ladder.steps().iter().map(|step| step.top_framerate).collect();
      assert_eq!(rates[..5], [30, 20, 15, 10, 5]);
      assert!(ladder.steps()[..5].iter().all(|step| step.has_middle));
      let first_smaller = ladder
        .steps()
        .iter()
        .position(|step| step.top_scale_per_mille < 1000)
        .unwrap();
      let before = ladder.step(first_smaller - 1);
      assert_eq!(before.top_framerate, 5);
      assert!(!before.has_middle);
      let top = ladder.layers(first_smaller - 1).pop().unwrap();
      assert_eq!((top.width, top.height), (2560, 1440));
    }

    /// Hardware AV1 never falls back to H.264 for CPU's sake: the codec isn't the load.
    #[test]
    fn hardware_av1_keeps_its_codec() {
      let ladder = ShareLadder::new(plan(QHD, Av1Encoding::Hardware, both(VideoCodec::Av1)));
      assert!(
        ladder
          .steps()
          .iter()
          .all(|step| step.top_codec == VideoCodec::Av1)
      );
      assert_eq!(ladder.step(0).top_framerate, 30);
      assert_eq!(ladder.step(usize::MAX), ladder.steps().last().unwrap());
    }

    /// A motion share keeps its rate longest.
    #[test]
    fn motion_steps_down_size_before_rate() {
      let mut plan = plan(QHD, Av1Encoding::Hardware, both(VideoCodec::H264));
      plan.profile.mode = ScreenContentMode::Motion;
      plan.framerate = 60;
      let ladder = ShareLadder::new(plan);
      let rates: Vec<u8> = ladder.steps().iter().map(|step| step.top_framerate).collect();
      assert_eq!(rates.first(), Some(&60));
      assert_eq!(rates.last(), Some(&30));
      assert!(rates[..rates.len() - 1].iter().all(|rate| *rate == 60));
    }
  }

  fn plan(source: VideoResolution, av1: Av1Encoding, codecs: ShareCodecs) -> SharePlan {
    SharePlan {
      source,
      framerate: 30,
      profile: ScreenProfile {
        mode: ScreenContentMode::Detail,
        size: ScreenShareResolution::Standard,
      },
      codecs,
      av1,
    }
  }

  /// Everyone decodes `codec` and H.264, and the SFU forwards layers in another codec.
  fn both(codec: VideoCodec) -> ShareCodecs {
    ShareCodecs {
      by_all: codec.clone(),
      maybe_by_some: Some(codec),
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
