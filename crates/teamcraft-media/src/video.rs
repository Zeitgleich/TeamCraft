//! Video types: raw and encoded frames, simulcast layer plans and codec choice.
//!
//! Capture and the actual encoders/decoders come later (platform specific: WebCodecs, VideoToolbox,
//! MediaCodec, VA-API); these types are what they plug into.

use std::borrow::Cow;

use teamcraft_proto::{
  MAX_SIMULCAST_LAYERS, MediaCapabilities, ScreenContentMode, ScreenShareResolution, VideoCodec,
  VideoLayerInfo, VideoTrackKind,
};
use thiserror::Error;
use web_time::{Duration, Instant};

use crate::datagram::SimulcastLayer;
use crate::rtp::RtpTimestamp;

/// RTP clock rate of video.
pub const VIDEO_CLOCK_RATE_HZ: u32 = 90_000;
/// A subscription's `max_layer` that means "the top layer", whatever the track has (the SFU caps
/// it by the receiver's downlink).
pub const TOP_LAYER: u8 = u8::MAX;
/// A changed source size is followed once it held this long (#931): dragging a window's edge
/// changes the size with every frame, and every rebuild of the encoders costs a keyframe.
pub const RESIZE_SETTLE: Duration = Duration::from_millis(300);
/// A side differs from the layers' only when it is off by more than this share (per mille) and...
const RESIZE_TOLERANCE_PER_MILLE: u32 = 20;
/// ...this many pixels: rounding to even sides and a window's shadow don't rebuild the encoders.
const RESIZE_TOLERANCE_PIXELS: u32 = 4;

/// One uncompressed frame from capture, in I420 (planar Y, U, V; chroma subsampled 2x2).
#[derive(Clone, PartialEq, Eq)]
pub struct VideoFrame {
  resolution: VideoResolution,
  timestamp_us: u64,
  data: Vec<u8>,
}

/// One encoded frame of one simulcast layer, before DAVE encryption and fragmentation.
#[derive(Clone, PartialEq, Eq)]
pub struct EncodedVideoFrame {
  pub codec: VideoCodec,
  pub layer: SimulcastLayer,
  pub timestamp: RtpTimestamp,
  pub is_keyframe: bool,
  pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VideoResolution {
  pub width: u16,
  pub height: u16,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VideoError {
  #[error("I420 frame of {width}x{height} needs {expected_bytes} bytes, got {actual_bytes}")]
  WrongFrameSize {
    width: u16,
    height: u16,
    expected_bytes: usize,
    actual_bytes: usize,
  },
  #[error("I420 needs even, non-zero dimensions, got {width}x{height}")]
  InvalidDimensions { width: u16, height: u16 },
}

impl VideoFrame {
  pub fn new(resolution: VideoResolution, timestamp_us: u64, data: Vec<u8>) -> Result<Self, VideoError> {
    let expected_bytes = resolution.i420_bytes()?;
    if data.len() != expected_bytes {
      return Err(VideoError::WrongFrameSize {
        width: resolution.width,
        height: resolution.height,
        expected_bytes,
        actual_bytes: data.len(),
      });
    }
    Ok(Self {
      resolution,
      timestamp_us,
      data,
    })
  }

  pub fn resolution(&self) -> VideoResolution {
    self.resolution
  }

  pub fn timestamp_us(&self) -> u64 {
    self.timestamp_us
  }

  /// The same picture at another time (a still screen's picture sent again).
  pub fn with_timestamp_us(mut self, timestamp_us: u64) -> Self {
    self.timestamp_us = timestamp_us;
    self
  }

  /// Y, U and V planes.
  pub fn planes(&self) -> (&[u8], &[u8], &[u8]) {
    let luma = usize::from(self.resolution.width) * usize::from(self.resolution.height);
    let chroma = luma / 4;
    let (y, rest) = self.data.split_at(luma);
    let (u, v) = rest.split_at(chroma);
    (y, u, v)
  }
  /// The whole I420 buffer (Y, then U, then V).
  pub fn data(&self) -> &[u8] {
    &self.data
  }

  /// A copy scaled to `target` (bilinear per plane), e.g. for lower simulcast layers.
  pub fn scaled(&self, target: VideoResolution) -> Result<VideoFrame, VideoError> {
    if target == self.resolution {
      return Ok(self.clone());
    }
    let mut data = Vec::with_capacity(target.i420_bytes()?);
    let (y, u, v) = self.planes();
    let (width, height) = (
      usize::from(self.resolution.width),
      usize::from(self.resolution.height),
    );
    let (target_width, target_height) = (usize::from(target.width), usize::from(target.height));
    scale_plane(y, width, height, target_width, target_height, &mut data);
    for chroma in [u, v] {
      scale_plane(
        chroma,
        width / 2,
        height / 2,
        target_width / 2,
        target_height / 2,
        &mut data,
      );
    }
    VideoFrame::new(target, self.timestamp_us, data)
  }

  /// This frame at `target`: itself when it has that size already (no copy), else scaled.
  pub fn at_size(&self, target: VideoResolution) -> Result<Cow<'_, VideoFrame>, VideoError> {
    if target == self.resolution {
      return Ok(Cow::Borrowed(self));
    }
    self.scaled(target).map(Cow::Owned)
  }
}

/// Bilinear resampling of one 8-bit plane, appended to `out`.
///
/// It runs per frame on the sender (a 4K or 1080p camera shrunk to 720p, and every encoder's lower
/// simulcast layers), so the source positions are worked out once per column and row, the weights
/// are 8-bit so the sums stay in `u32`, and halving, the simulcast step, averages 2x2 blocks
/// (which is what bilinear gives there). docs/performance.md has the numbers (#537).
fn scale_plane(
  plane: &[u8],
  width: usize,
  height: usize,
  target_width: usize,
  target_height: usize,
  out: &mut Vec<u8>,
) {
  let start = out.len();
  out.resize(start + target_width * target_height, 0);
  let target = &mut out[start..];
  if target_width == 0 || target_height == 0 {
    return;
  }
  if width == target_width * 2 && height == target_height * 2 {
    for (target_row, rows) in target
      .chunks_exact_mut(target_width)
      .zip(plane.chunks_exact(width * 2))
    {
      let (top, bottom) = rows.split_at(width);
      for ((value, top), bottom) in target_row
        .iter_mut()
        .zip(top.as_chunks::<2>().0)
        .zip(bottom.as_chunks::<2>().0)
      {
        let sum = u16::from(top[0]) + u16::from(top[1]) + u16::from(bottom[0]) + u16::from(bottom[1]);
        *value = ((sum + 2) >> 2) as u8;
      }
    }
    return;
  }
  let columns = sample_positions(width, target_width);
  let mut top = vec![0u32; target_width];
  let mut bottom = vec![0u32; target_width];
  // The source rows in `top` and `bottom`: neighbouring target rows often share one (enlarging,
  // or shrinking by less than half).
  let mut held = (usize::MAX, usize::MAX);
  for (target_row, (row, next_row, fraction_y)) in target
    .chunks_exact_mut(target_width)
    .zip(sample_positions(height, target_height))
  {
    if row == held.1 && row != held.0 {
      std::mem::swap(&mut top, &mut bottom);
      held = (held.1, held.0);
    }
    if row != held.0 {
      interpolate_row(&plane[row * width..][..width], &columns, &mut top);
    }
    if fraction_y == 0 {
      // On a source row (shrinking by an odd whole factor, like 4K to 720p): no second row.
      for (value, top) in target_row.iter_mut().zip(&top) {
        *value = ((top + 128) >> 8) as u8;
      }
      held = (row, held.1);
      continue;
    }
    if next_row != held.1 {
      interpolate_row(&plane[next_row * width..][..width], &columns, &mut bottom);
    }
    held = (row, next_row);
    for ((value, top), bottom) in target_row.iter_mut().zip(&top).zip(&bottom) {
      // Each input is a pixel times 256, so the blend is a pixel times 65536.
      *value = ((top * (256 - fraction_y) + bottom * fraction_y + (1 << 15)) >> 16) as u8;
    }
  }
}

/// For each of `target` samples along a side of `source` pixels: the two source pixels it lies
/// between and its 8-bit fraction towards the second (target pixel centres mapped onto the source;
/// past the last pixel both are the last).
fn sample_positions(source: usize, target: usize) -> Vec<(usize, usize, u32)> {
  // 16.16 fixed point.
  let step = (source << 16) / target;
  (0..target)
    .map(|index| {
      let position = (index * step + step / 2).saturating_sub(1 << 15);
      let first = (position >> 16).min(source - 1);
      (
        first,
        (first + 1).min(source - 1),
        ((position & 0xffff) >> 8) as u32,
      )
    })
    .collect()
}

/// One source row interpolated horizontally at `columns`, as pixel values times 256.
fn interpolate_row(row: &[u8], columns: &[(usize, usize, u32)], out: &mut [u32]) {
  for (value, &(left, right, fraction)) in out.iter_mut().zip(columns) {
    *value = u32::from(row[left]) * (256 - fraction) + u32::from(row[right]) * fraction;
  }
}

impl EncodedVideoFrame {
  /// RTP timestamp for a capture time in microseconds.
  pub fn rtp_timestamp(timestamp_us: u64) -> RtpTimestamp {
    // Wrapping is intended: RTP timestamps are modulo 2^32.
    RtpTimestamp((timestamp_us * u64::from(VIDEO_CLOCK_RATE_HZ) / 1_000_000) as u32)
  }
}

impl VideoResolution {
  fn i420_bytes(self) -> Result<usize, VideoError> {
    if self.width == 0 || self.height == 0 || !self.width.is_multiple_of(2) || !self.height.is_multiple_of(2)
    {
      return Err(VideoError::InvalidDimensions {
        width: self.width,
        height: self.height,
      });
    }
    let luma = usize::from(self.width) * usize::from(self.height);
    Ok(luma + luma / 2)
  }
}

/// `source` shrunk to fit inside `limit`, keeping its aspect ratio and even sides (I420); never
/// enlarged. A camera may capture at 4K, but what is sent stays within the call's send target.
pub fn fit_within(source: VideoResolution, limit: VideoResolution) -> VideoResolution {
  if source.width <= limit.width && source.height <= limit.height {
    return source;
  }
  let (width, height) = (u32::from(source.width), u32::from(source.height));
  let (limit_width, limit_height) = (u32::from(limit.width), u32::from(limit.height));
  // The tighter of the two ratios decides: compare width*limit_height with height*limit_width.
  let (fitted_width, fitted_height) = if width * limit_height > height * limit_width {
    (limit_width, (height * limit_width / width).max(2))
  } else {
    ((width * limit_height / height).max(2), limit_height)
  };
  VideoResolution {
    // Sides are at most the limit's, so they fit a u16.
    width: u16::try_from(fitted_width & !1).unwrap_or(limit.width),
    height: u16::try_from(fitted_height & !1).unwrap_or(limit.height),
  }
}

/// Simulcast layers for a source resolution: up to three layers at 1/4, 1/2 and full size, with
/// bitrates scaled from `full_bitrate_bps` by pixel count. Layers below 160 px width are skipped.
pub fn simulcast_layers(
  source: VideoResolution,
  max_framerate: u8,
  full_bitrate_bps: u32,
  max_layers: u8,
) -> Vec<VideoLayerInfo> {
  const MIN_WIDTH: u16 = 160;
  let max_layers = usize::from(max_layers.clamp(1, MAX_SIMULCAST_LAYERS));
  // Largest first, so the layer limit keeps the full resolution.
  let mut sizes: Vec<(u16, u16, u32)> = Vec::new();
  for divisor in [1u16, 2, 4] {
    let width = (source.width / divisor) & !1;
    if sizes.len() == max_layers || (divisor > 1 && width < MIN_WIDTH) {
      break;
    }
    // Even dimensions for I420; the pixel count shrinks with the square of the divisor.
    let height = (source.height / divisor) & !1;
    let divisor_squared = u32::from(divisor) * u32::from(divisor);
    sizes.push((width, height, full_bitrate_bps / divisor_squared));
  }
  sizes
    .into_iter()
    .rev()
    .zip(0u8..)
    .map(|((width, height, max_bitrate_bps), layer)| VideoLayerInfo {
      layer,
      width,
      height,
      max_framerate,
      max_bitrate_bps,
    })
    .collect()
}

/// The largest screen share picture of each content mode (#530) and share size (#565): detail
/// up to 1440p at 30 fps, motion up to 1080p at 60 fps; with [`ScreenShareResolution::Uhd`] detail
/// up to 4K at 30 fps and motion up to 1440p at 60 fps.
pub fn screen_limits(mode: ScreenContentMode, size: &ScreenShareResolution) -> (VideoResolution, u8) {
  let is_uhd = *size == ScreenShareResolution::Uhd;
  match (mode, is_uhd) {
    (ScreenContentMode::Motion, false) => (FULL_HD, 60),
    (ScreenContentMode::Motion, true) => (QHD, 60),
    (ScreenContentMode::Detail | ScreenContentMode::Unknown, false) => (QHD, 30),
    (ScreenContentMode::Detail | ScreenContentMode::Unknown, true) => (UHD, 30),
  }
}

const FULL_HD: VideoResolution = VideoResolution {
  width: 1920,
  height: 1080,
};
const QHD: VideoResolution = VideoResolution {
  width: 2560,
  height: 1440,
};
const UHD: VideoResolution = VideoResolution {
  width: 3840,
  height: 2160,
};

/// Bits per pixel of a screen share's top layer: enough for text to stay sharp in H.264.
const SCREEN_BITS_PER_PIXEL: f64 = 0.06;
const SCREEN_MIN_BITRATE_BPS: u32 = 2_000_000;
/// The most a standard share's top layer sends (1440p at 30 fps needs about 6.6 Mbit/s).
const SCREEN_MAX_BITRATE_BPS: u32 = 8_000_000;
/// The most a 4K share's top layer sends (#565): 4K at 30 fps needs about 15 Mbit/s.
const SCREEN_UHD_MAX_BITRATE_BPS: u32 = 16_000_000;

/// The most the top layer of a share of `size` sends.
pub fn screen_max_bitrate_bps(size: &ScreenShareResolution) -> u32 {
  match size {
    ScreenShareResolution::Uhd => SCREEN_UHD_MAX_BITRATE_BPS,
    ScreenShareResolution::Standard | ScreenShareResolution::Unknown(_) => SCREEN_MAX_BITRATE_BPS,
  }
}

/// The top layer's bitrate for a screen share of `resolution` at `framerate`: it follows the
/// pixels per second, from 2 Mbit/s up to the share size's cap (1080p30 about 4, 1440p30 about
/// 6.6, both at most 8; 4K30 about 15 of at most 16).
pub fn screen_bitrate_bps(resolution: VideoResolution, framerate: u8, size: &ScreenShareResolution) -> u32 {
  let pixels_per_second = f64::from(resolution.width) * f64::from(resolution.height) * f64::from(framerate);
  // Clamped to the u32 range above, so the cast cannot truncate.
  (pixels_per_second * SCREEN_BITS_PER_PIXEL).clamp(
    f64::from(SCREEN_MIN_BITRATE_BPS),
    f64::from(screen_max_bitrate_bps(size)),
  ) as u32
}

/// Layers of a screen share captured at `source`: the picture within the mode's and size's limits,
/// plus a half-size layer at a quarter of the bitrate for viewers on weak links (the uplink budget
/// drops the big layer first, so a slow link or viewer doesn't lower the quality for the others).
/// A picture above 1440p (a 4K share) gets a quarter-size layer too: from 4K the half-size one is
/// 1080p at about 4 Mbit/s, and a weak link needs a step below it.
pub fn screen_layers(
  source: VideoResolution,
  framerate: u8,
  mode: ScreenContentMode,
  size: &ScreenShareResolution,
) -> Vec<VideoLayerInfo> {
  let (limit, max_framerate) = screen_limits(mode, size);
  let resolution = fit_within(source, limit);
  let framerate = framerate.clamp(1, max_framerate);
  let layers = if u32::from(resolution.width) * u32::from(resolution.height) > QHD_PIXELS {
    3
  } else {
    2
  };
  simulcast_layers(
    resolution,
    framerate,
    screen_bitrate_bps(resolution, framerate, size),
    layers,
  )
}

const QHD_PIXELS: u32 = 2560 * 1440;

/// How a screen share is sent (#530, #565): what it keeps under pressure and how large it may be.
/// The default is a standard-size detail share.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScreenProfile {
  pub mode: ScreenContentMode,
  pub size: ScreenShareResolution,
}

impl ScreenProfile {
  /// The largest picture and frame rate ([`screen_limits`]).
  pub fn limits(&self) -> (VideoResolution, u8) {
    screen_limits(self.mode, &self.size)
  }

  /// The layers of a share captured at `source` ([`screen_layers`]).
  pub fn layers(&self, source: VideoResolution, framerate: u8) -> Vec<VideoLayerInfo> {
    screen_layers(source, framerate, self.mode, &self.size)
  }

  /// The codecs a share may go out in where the sender encodes them and every viewer decodes
  /// them, best first (#565). A detail share takes AV1 first: with its screen content tools
  /// (WebCodecs `contentHint: "text"`) text came out 7 to 11 dB sharper than H.264 at 1 to 2
  /// Mbit/s, and a still screen needed a fraction of H.264's bitrate (docs/performance.md). A
  /// motion share stays H.264: there AV1 was no sharper than the hardware H.264 encoder and cost
  /// several times its CPU. H.264, which every device decodes, comes last.
  pub fn codecs(&self) -> Vec<VideoCodec> {
    match self.mode {
      ScreenContentMode::Motion => vec![VideoCodec::H264],
      ScreenContentMode::Detail | ScreenContentMode::Unknown => vec![VideoCodec::Av1, VideoCodec::H264],
    }
  }
}

/// The largest picture a camera is sent at (#967): 1080p where the camera delivers it.
pub const CAMERA_MAX_RESOLUTION: VideoResolution = VideoResolution {
  width: 1920,
  height: 1080,
};
/// The frame rate cameras are sent at.
pub const CAMERA_FRAMERATE: u8 = 30;
/// The bitrate of a camera's 720p layer; layers above it get more by their pixel count...
pub const CAMERA_HD_BITRATE_BPS: u32 = 1_500_000;
/// ...up to this (1080p): more pixels need fewer bits each. At 30 fps that is about 0.05 bits
/// per pixel, where a calm camera picture through VideoToolbox measured 46 dB PSNR at 720p (#967).
pub const CAMERA_MAX_BITRATE_BPS: u32 = 3_000_000;
const HD_PIXELS: u64 = 1280 * 720;

/// The box a camera is sent within when the user caps it at `max_lines` lines (720 for 720p),
/// never above [`CAMERA_MAX_RESOLUTION`]; `None` (Auto): the most there is. A box of the lines'
/// 16:9 picture, so a portrait or 4:3 camera fits it by its height like a landscape one.
pub fn camera_limit(maybe_max_lines: Option<u16>) -> VideoResolution {
  let Some(lines) = maybe_max_lines else {
    return CAMERA_MAX_RESOLUTION;
  };
  let lines = lines.clamp(2, CAMERA_MAX_RESOLUTION.height);
  let width = u32::from(lines) * 16 / 9;
  VideoResolution {
    width: u16::try_from(width & !1).unwrap_or(CAMERA_MAX_RESOLUTION.width),
    height: lines & !1,
  }
}

/// The simulcast layers of a camera that delivers `source`, sent within `limit` ([`camera_limit`];
/// a larger camera is scaled down, a smaller one sent as it is). Above 720p the layers are the
/// whole picture, two thirds and one third (1080p, 720p, 360p): viewers of a 720p tile get the
/// 720p layer they always got, weak links the 360p one, and only large tiles take the 1080p one.
/// At 720p and below they halve (720p, 360p, 180p), the top one at [`CAMERA_HD_BITRATE_BPS`].
/// Layer bitrates follow the pixel count against 720p's [`CAMERA_HD_BITRATE_BPS`], up to
/// [`CAMERA_MAX_BITRATE_BPS`].
pub fn camera_layers(source: VideoResolution, framerate: u8, limit: VideoResolution) -> Vec<VideoLayerInfo> {
  let top = even(fit_within(source, limit));
  if top.width.min(top.height) <= 720 {
    // As before #967: the top layer gets the bitrate of 720p.
    return simulcast_layers(top, framerate, CAMERA_HD_BITRATE_BPS, 3);
  }
  let third = |side: u16, thirds: u32| {
    let scaled = u32::from(side) * thirds / 3;
    u16::try_from(scaled & !1).unwrap_or(side).max(2)
  };
  [1, 2, 3]
    .into_iter()
    .zip(0u8..)
    .map(|(thirds, layer)| {
      let size = VideoResolution {
        width: third(top.width, thirds),
        height: third(top.height, thirds),
      };
      VideoLayerInfo {
        layer,
        width: size.width,
        height: size.height,
        max_framerate: framerate,
        max_bitrate_bps: hd_scaled_bitrate(size),
      }
    })
    .collect()
}

/// [`CAMERA_HD_BITRATE_BPS`] scaled by the pixel count of `size` against 720p, at most
/// [`CAMERA_MAX_BITRATE_BPS`].
fn hd_scaled_bitrate(size: VideoResolution) -> u32 {
  let pixels = u64::from(size.width) * u64::from(size.height);
  let scaled = u64::from(CAMERA_HD_BITRATE_BPS) * pixels / HD_PIXELS;
  u32::try_from(scaled)
    .unwrap_or(CAMERA_MAX_BITRATE_BPS)
    .min(CAMERA_MAX_BITRATE_BPS)
}

/// Keeps a track's layers in the shape of its source (#931): a shared window that is resized or
/// turned from landscape to portrait gets layers of its new size, within the size it started with
/// as the limit, instead of being stretched into the old ones. A new size takes over once it held
/// for [`RESIZE_SETTLE`], so a window being dragged rebuilds the encoders once, not every frame;
/// the first picture's size takes over at once (nothing was sent yet).
#[derive(Debug, Clone)]
pub struct LayerSizer {
  configured: Vec<VideoLayerInfo>,
  limit: VideoResolution,
  current: VideoResolution,
  maybe_pending: Option<PendingSize>,
  has_seen_source: bool,
}

/// A source size seen but not followed yet.
#[derive(Debug, Clone, Copy)]
struct PendingSize {
  size: VideoResolution,
  since: Instant,
}

impl LayerSizer {
  /// Follows the size of a source whose layers were planned as `configured`; the top layer never
  /// grows beyond `limit` (the track's largest picture).
  pub fn new(configured: Vec<VideoLayerInfo>, limit: VideoResolution) -> Self {
    let current = largest_layer(&configured).map_or(limit, |top| VideoResolution {
      width: top.width,
      height: top.height,
    });
    Self {
      configured,
      limit,
      current,
      maybe_pending: None,
      has_seen_source: false,
    }
  }

  /// The source delivered a picture of `source` at `now`: the new layers when the encoders have to
  /// follow it, `None` while they fit (or a new size hasn't settled yet).
  pub fn observe(&mut self, source: VideoResolution, now: Instant) -> Option<Vec<VideoLayerInfo>> {
    if source.width < 2 || source.height < 2 || self.configured.is_empty() {
      return None;
    }
    let is_first = !std::mem::replace(&mut self.has_seen_source, true);
    let top = even(fit_within(source, self.limit));
    if !is_resized(self.current, top) {
      self.maybe_pending = None;
      return None;
    }
    let is_settled = match self.maybe_pending {
      Some(pending) if !is_resized(pending.size, top) => {
        now.saturating_duration_since(pending.since) >= RESIZE_SETTLE
      },
      _ => {
        self.maybe_pending = Some(PendingSize {
          size: top,
          since: now,
        });
        false
      },
    };
    if !is_first && !is_settled {
      return None;
    }
    self.current = top;
    self.maybe_pending = None;
    Some(resized_layers(&self.configured, top))
  }
}

/// `configured` layers for a top layer of `top`: every layer keeps its share of the top layer's
/// size, with even sides (I420), its id, rate and bitrate cap.
pub fn resized_layers(configured: &[VideoLayerInfo], top: VideoResolution) -> Vec<VideoLayerInfo> {
  let Some(full) = largest_layer(configured) else {
    return Vec::new();
  };
  let full_width = u32::from(full.width.max(1));
  configured
    .iter()
    .map(|layer| {
      // One factor for both sides keeps the aspect ratio; a layer is at most the top's size.
      let side = |top_side: u16| {
        let scaled = (u32::from(top_side) * u32::from(layer.width) / full_width).min(u32::from(top_side));
        u16::try_from(scaled & !1).unwrap_or(top_side).max(2)
      };
      VideoLayerInfo {
        width: side(top.width),
        height: side(top.height),
        ..*layer
      }
    })
    .collect()
}

fn largest_layer(layers: &[VideoLayerInfo]) -> Option<&VideoLayerInfo> {
  layers
    .iter()
    .max_by_key(|layer| u32::from(layer.width) * u32::from(layer.height))
}

/// Whether `new` differs from `current` by more than the tolerance on either side.
fn is_resized(current: VideoResolution, new: VideoResolution) -> bool {
  let differs = |current: u16, new: u16| {
    let tolerance = (u32::from(current) * RESIZE_TOLERANCE_PER_MILLE / 1000).max(RESIZE_TOLERANCE_PIXELS);
    u32::from(current.abs_diff(new)) > tolerance
  };
  differs(current.width, new.width) || differs(current.height, new.height)
}

/// `resolution` with even sides of at least 2 (I420).
fn even(resolution: VideoResolution) -> VideoResolution {
  VideoResolution {
    width: (resolution.width & !1).max(2),
    height: (resolution.height & !1).max(2),
  }
}

/// The simulcast layer a viewer asks for (a subscription's `max_layer`) for a picture shown
/// `shown` device pixels large (CSS size × `devicePixelRatio`): the smallest layer that covers it,
/// so a picture is never scaled up while a sharper layer exists, and [`TOP_LAYER`] when none does.
/// A tile crops its picture to fill (`object-fit: cover`), so a layer covers it when both its sides
/// are at least the shown ones. Screen shares always get the top layer, as on the desktop: text has
/// to stay readable. A picture not shown (zero size, not laid out yet) gets the lowest layer; a
/// track whose layers are unknown gets the top one (the SFU still caps it by the downlink).
pub fn layer_for_view(kind: &VideoTrackKind, layers: &[VideoLayerInfo], shown: VideoResolution) -> u8 {
  if *kind == VideoTrackKind::Screen {
    return TOP_LAYER;
  }
  let mut by_layer: Vec<&VideoLayerInfo> = layers.iter().collect();
  by_layer.sort_by_key(|info| info.layer);
  let Some(lowest) = by_layer.first() else {
    return TOP_LAYER;
  };
  if shown.width == 0 || shown.height == 0 {
    return lowest.layer;
  }
  by_layer
    .into_iter()
    .find(|info| info.width >= shown.width && info.height >= shown.height)
    .map_or(TOP_LAYER, |info| info.layer)
}

/// The lines of a layer's picture as a quality menu names them (720 for "720p"): its short side,
/// so a portrait camera's 720×1280 is 720p as well.
pub fn layer_lines(info: &VideoLayerInfo) -> u16 {
  info.width.min(info.height)
}

/// The layer a viewer who picked at most `max_lines` (#967: "720p" in the quality menu) asks
/// for: the largest with at most that many lines ([`layer_lines`]), else the lowest. The SFU
/// still caps it by what is sent and by the downlink. A track whose layers are unknown gets
/// [`TOP_LAYER`].
pub fn layer_for_lines(layers: &[VideoLayerInfo], max_lines: u16) -> u8 {
  let mut by_layer: Vec<&VideoLayerInfo> = layers.iter().collect();
  by_layer.sort_by_key(|info| info.layer);
  let Some(lowest) = by_layer.first() else {
    return TOP_LAYER;
  };
  by_layer
    .iter()
    .rev()
    .find(|info| layer_lines(info) <= max_lines)
    .map_or(lowest.layer, |info| info.layer)
}

/// The layer to ask for: by the size the picture is shown at ([`layer_for_view`]) while the
/// viewer leaves it to the call (`None`), else at most the lines they picked
/// ([`layer_for_lines`]).
pub fn chosen_layer(
  kind: &VideoTrackKind,
  layers: &[VideoLayerInfo],
  shown: VideoResolution,
  maybe_max_lines: Option<u16>,
) -> u8 {
  match maybe_max_lines {
    None => layer_for_view(kind, layers, shown),
    Some(lines) => layer_for_lines(layers, lines),
  }
}

/// What a viewer's app knows of how it shows a received track (#967): the size its tile shows it
/// at, once told, and the lines the viewer picked, if they did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewChoice {
  pub maybe_shown: Option<VideoResolution>,
  pub maybe_max_lines: Option<u16>,
}

impl ViewChoice {
  /// The layer to ask for ([`chosen_layer`]); [`TOP_LAYER`] while neither the size nor a pick is
  /// known (the SFU caps it by the downlink).
  pub fn layer(&self, kind: &VideoTrackKind, layers: &[VideoLayerInfo]) -> u8 {
    match (self.maybe_shown, self.maybe_max_lines) {
      (None, None) => TOP_LAYER,
      (maybe_shown, maybe_max_lines) => chosen_layer(
        kind,
        layers,
        maybe_shown.unwrap_or(VideoResolution {
          width: u16::MAX,
          height: u16::MAX,
        }),
        maybe_max_lines,
      ),
    }
  }
}

/// An AV1 temporal unit's first byte when it is a temporal delimiter OBU (type 2, with a size).
const AV1_TEMPORAL_DELIMITER: [u8; 2] = [0x12, 0x00];
const AV1_OBU_HAS_SIZE: u8 = 0b0000_0010;
const AV1_OBU_HAS_EXTENSION: u8 = 0b0000_0100;
const AV1_OBU_TYPE_SHIFT: u8 = 3;
const AV1_OBU_TYPE_MASK: u8 = 0b0111_1000;
const AV1_OBU_TEMPORAL_DELIMITER: u8 = 2;

/// A decrypted AV1 frame as decoders take it (#565). DAVE sends a temporal unit as AV1's RTP
/// format carries it: without its temporal delimiter, and the last OBU without its size field.
/// WebCodecs (and the hardware decoders behind it) take a temporal unit in the low overhead
/// format, as encoders give it: a temporal delimiter first and every OBU with its size. A frame
/// that doesn't parse is returned as it is (the decoder's error asks for a keyframe).
pub fn av1_temporal_unit(frame: &[u8]) -> Vec<u8> {
  let mut unit = Vec::with_capacity(frame.len() + AV1_TEMPORAL_DELIMITER.len() + 8);
  let starts_with_delimiter = frame
    .first()
    .is_some_and(|header| (header & AV1_OBU_TYPE_MASK) >> AV1_OBU_TYPE_SHIFT == AV1_OBU_TEMPORAL_DELIMITER);
  if !starts_with_delimiter {
    unit.extend_from_slice(&AV1_TEMPORAL_DELIMITER);
  }
  let mut rest = frame;
  while let Some((&header, after_header)) = rest.split_first() {
    let extension_bytes = usize::from(header & AV1_OBU_HAS_EXTENSION != 0);
    let Some((extension, after_extension)) = after_header.split_at_checked(extension_bytes) else {
      return frame.to_vec();
    };
    let (payload, next) = if header & AV1_OBU_HAS_SIZE != 0 {
      let Some((size, size_bytes)) = read_leb128(after_extension) else {
        return frame.to_vec();
      };
      match after_extension[size_bytes..].split_at_checked(size) {
        Some(split) => split,
        None => return frame.to_vec(),
      }
    } else {
      (after_extension, &after_extension[after_extension.len()..])
    };
    unit.push(header | AV1_OBU_HAS_SIZE);
    unit.extend_from_slice(extension);
    write_leb128(payload.len(), &mut unit);
    unit.extend_from_slice(payload);
    rest = next;
  }
  unit
}

/// An unsigned LEB128 number (at most 8 bytes, as AV1 allows) and the bytes it took.
fn read_leb128(bytes: &[u8]) -> Option<(usize, usize)> {
  let mut value = 0u64;
  for (index, byte) in bytes.iter().take(8).enumerate() {
    value |= u64::from(byte & 0x7f) << (index * 7);
    if byte & 0x80 == 0 {
      return Some((usize::try_from(value).ok()?, index + 1));
    }
  }
  None
}

fn write_leb128(mut value: usize, out: &mut Vec<u8>) {
  loop {
    // The low seven bits; the cast keeps exactly them.
    let byte = (value & 0x7f) as u8;
    value >>= 7;
    if value == 0 {
      out.push(byte);
      return;
    }
    out.push(byte | 0x80);
  }
}

/// The best video codec a sender can use so that every receiver decodes it: encodable by the
/// sender, decodable by all `receivers`, preferring AV1, then VP9, then H.264.
pub fn choose_video_codec(
  sender: &MediaCapabilities,
  receivers: &[&MediaCapabilities],
) -> Option<VideoCodec> {
  let mut candidates: Vec<&VideoCodec> = sender
    .video_encoders
    .iter()
    .filter(|codec| !matches!(codec, VideoCodec::Unknown(_)))
    .filter(|codec| {
      receivers
        .iter()
        .all(|receiver| receiver.video_decoders.contains(codec))
    })
    .collect();
  candidates.sort_by_key(|codec| codec.preference());
  candidates.first().map(|codec| (*codec).clone())
}

// Frame contents don't belong in logs (and are large).
impl std::fmt::Debug for VideoFrame {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("VideoFrame")
      .field("resolution", &self.resolution)
      .field("timestamp_us", &self.timestamp_us)
      .finish_non_exhaustive()
  }
}

impl std::fmt::Debug for EncodedVideoFrame {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("EncodedVideoFrame")
      .field("codec", &self.codec)
      .field("layer", &self.layer)
      .field("timestamp", &self.timestamp)
      .field("is_keyframe", &self.is_keyframe)
      .field("bytes", &self.data.len())
      .finish()
  }
}

#[cfg(test)]
mod tests {
  #[test]
  fn fit_within_shrinks_to_the_limit_and_never_enlarges() {
    let res = |width, height| VideoResolution { width, height };
    let hd = res(1280, 720);
    assert_eq!(fit_within(res(3840, 2160), hd), hd);
    assert_eq!(fit_within(res(1920, 1080), hd), hd);
    assert_eq!(fit_within(res(1280, 960), hd), res(960, 720));
    assert_eq!(fit_within(res(640, 480), hd), res(640, 480));
    assert_eq!(fit_within(res(720, 1280), hd), res(404, 720));
  }

  use super::*;

  #[test]
  fn screen_bitrate_follows_the_pixel_rate_within_bounds() {
    let res = |width, height| VideoResolution { width, height };
    let standard = ScreenShareResolution::Standard;
    let uhd = ScreenShareResolution::Uhd;
    let mbps = |resolution, framerate| screen_bitrate_bps(resolution, framerate, &standard) as f64 / 1e6;
    assert!((3.5..4.0).contains(&mbps(res(1920, 1080), 30)));
    assert!((6.0..7.0).contains(&mbps(res(2560, 1440), 30)));
    assert!((7.0..=8.0).contains(&mbps(res(1920, 1080), 60)));
    assert_eq!(screen_bitrate_bps(res(2560, 1440), 60, &standard), 8_000_000);
    assert_eq!(screen_bitrate_bps(res(640, 360), 15, &standard), 2_000_000);
    // A 4K share may send more (#565), and a standard-size picture the same either way.
    let uhd_mbps = screen_bitrate_bps(res(3840, 2160), 30, &uhd) as f64 / 1e6;
    assert!((14.5..15.5).contains(&uhd_mbps), "{uhd_mbps}");
    assert_eq!(screen_bitrate_bps(res(3840, 2160), 30, &standard), 8_000_000);
    assert_eq!(screen_bitrate_bps(res(2560, 1440), 60, &uhd), 13_271_040);
    assert_eq!(
      screen_bitrate_bps(res(1920, 1080), 30, &uhd),
      screen_bitrate_bps(res(1920, 1080), 30, &standard)
    );
    assert_eq!(screen_bitrate_bps(res(7680, 4320), 60, &uhd), 16_000_000);
  }

  #[test]
  fn screen_layers_keep_text_sharp_or_motion_smooth() {
    let res = |width, height| VideoResolution { width, height };
    let standard = ScreenShareResolution::Standard;
    let detail = screen_layers(res(3840, 2160), 60, ScreenContentMode::Detail, &standard);
    let top = detail.last().unwrap();
    assert_eq!((top.width, top.height, top.max_framerate), (2560, 1440, 30));
    assert_eq!(detail.len(), 2);
    assert_eq!((detail[0].width, detail[0].height), (1280, 720));
    let motion = screen_layers(res(2560, 1440), 60, ScreenContentMode::Motion, &standard);
    let top = motion.last().unwrap();
    assert_eq!((top.width, top.height, top.max_framerate), (1920, 1080, 60));
    // A small window is not enlarged.
    let small = screen_layers(res(800, 600), 30, ScreenContentMode::Detail, &standard);
    assert_eq!(
      (small.last().unwrap().width, small.last().unwrap().height),
      (800, 600)
    );
  }

  /// A 4K share (#565) keeps a 4K screen's pixels, with two lower layers to step down to.
  #[test]
  fn uhd_screen_layers_keep_4k_and_step_down_in_three_layers() {
    let res = |width, height| VideoResolution { width, height };
    let uhd = ScreenShareResolution::Uhd;
    let detail = screen_layers(res(3840, 2160), 60, ScreenContentMode::Detail, &uhd);
    let sizes: Vec<(u16, u16, u8)> = detail
      .iter()
      .map(|layer| (layer.width, layer.height, layer.max_framerate))
      .collect();
    assert_eq!(sizes, vec![(960, 540, 30), (1920, 1080, 30), (3840, 2160, 30)]);
    assert!(detail[2].max_bitrate_bps > SCREEN_MAX_BITRATE_BPS);
    assert_eq!(detail[1].max_bitrate_bps, detail[2].max_bitrate_bps / 4);
    // A 5K screen is shrunk to 4K; motion keeps 60 fps at 1440p.
    let five_k = screen_layers(res(5120, 2880), 30, ScreenContentMode::Detail, &uhd);
    assert_eq!((five_k[2].width, five_k[2].height), (3840, 2160));
    let motion = screen_layers(res(3840, 2160), 60, ScreenContentMode::Motion, &uhd);
    let top = motion.last().unwrap();
    assert_eq!((top.width, top.height, top.max_framerate), (2560, 1440, 60));
    assert_eq!(motion.len(), 2);
    // A 1440p screen shared in 4K is sent as before: the size option never enlarges.
    let qhd = screen_layers(res(2560, 1440), 30, ScreenContentMode::Detail, &uhd);
    assert_eq!(
      qhd,
      screen_layers(
        res(2560, 1440),
        30,
        ScreenContentMode::Detail,
        &ScreenShareResolution::Standard
      )
    );
  }

  const HD: VideoResolution = VideoResolution {
    width: 1280,
    height: 720,
  };

  /// What DAVE (and AV1's RTP format) leave of a temporal unit becomes one again (#565).
  #[test]
  fn av1_temporal_units_get_their_delimiter_and_sizes_back() {
    // A temporal delimiter, a sequence header (3 bytes) and a frame OBU of 200 bytes (two-byte
    // size), as an encoder gives them.
    let mut original = vec![0x12, 0x00, 0x0a, 0x03, 1, 2, 3, 0x32, 0xc8, 0x01];
    original.extend(std::iter::repeat_n(7u8, 200));
    // As DAVE sends it: no delimiter, the last OBU without its size.
    let mut sent = vec![0x0a, 0x03, 1, 2, 3, 0x30];
    sent.extend(std::iter::repeat_n(7u8, 200));
    assert_eq!(av1_temporal_unit(&sent), original);
    // A complete unit stays as it is; so does one that doesn't parse.
    assert_eq!(av1_temporal_unit(&original), original);
    let truncated = [0x0a, 0x09, 1, 2];
    assert_eq!(av1_temporal_unit(&truncated), truncated);
    // An OBU with an extension byte keeps it.
    assert_eq!(
      av1_temporal_unit(&[0x34, 0x28, 9, 9]),
      vec![0x12, 0x00, 0x36, 0x28, 0x02, 9, 9]
    );
  }

  mod frames {
    use super::*;

    #[test]
    fn i420_frame_size_is_checked() {
      let frame = VideoFrame::new(HD, 0, vec![0; 1280 * 720 * 3 / 2]).unwrap();
      let (y, u, v) = frame.planes();
      assert_eq!((y.len(), u.len(), v.len()), (921_600, 230_400, 230_400));
      assert!(matches!(
        VideoFrame::new(HD, 0, vec![0; 10]),
        Err(VideoError::WrongFrameSize { .. })
      ));
      let odd = VideoResolution {
        width: 641,
        height: 480,
      };
      assert!(matches!(
        VideoFrame::new(odd, 0, Vec::new()),
        Err(VideoError::InvalidDimensions { .. })
      ));
    }

    #[test]
    fn scaling_keeps_flat_colours_and_halves_planes() {
      let mut data = vec![200u8; 1280 * 720];
      data.extend(vec![60u8; 1280 * 720 / 4]);
      data.extend(vec![180u8; 1280 * 720 / 4]);
      let frame = VideoFrame::new(HD, 7, data).unwrap();
      let half = frame
        .scaled(VideoResolution {
          width: 640,
          height: 360,
        })
        .unwrap();
      let (y, u, v) = half.planes();
      assert_eq!((y.len(), u.len(), v.len()), (640 * 360, 320 * 180, 320 * 180));
      assert!(y.iter().all(|value| *value == 200) && u.iter().all(|value| *value == 60));
      assert!(v.iter().all(|value| *value == 180));
      assert_eq!(half.timestamp_us(), 7);
    }

    /// The scaler before #537 (16-bit weights in `i64`, one pixel at a time): the faster one
    /// must give the same pictures, give or take one level of rounding.
    fn reference_scale(
      plane: &[u8],
      width: usize,
      height: usize,
      target_width: usize,
      target_height: usize,
    ) -> Vec<u8> {
      let step_x = ((width << 16) / target_width) as i64;
      let step_y = ((height << 16) / target_height) as i64;
      let sample = |x: usize, y: usize| i64::from(plane[y.min(height - 1) * width + x.min(width - 1)]);
      let mut out = Vec::new();
      for target_y in 0..target_height {
        let source_y = ((target_y as i64 * step_y) + step_y / 2 - (1 << 15)).max(0);
        let (y0, fraction_y) = ((source_y >> 16) as usize, source_y & 0xffff);
        for target_x in 0..target_width {
          let source_x = ((target_x as i64 * step_x) + step_x / 2 - (1 << 15)).max(0);
          let (x0, fraction_x) = ((source_x >> 16) as usize, source_x & 0xffff);
          let top = sample(x0, y0) * (0x10000 - fraction_x) + sample(x0 + 1, y0) * fraction_x;
          let bottom = sample(x0, y0 + 1) * (0x10000 - fraction_x) + sample(x0 + 1, y0 + 1) * fraction_x;
          out.push(((top * (0x10000 - fraction_y) + bottom * fraction_y + (1 << 31)) >> 32) as u8);
        }
      }
      out
    }

    #[test]
    fn scaling_matches_plain_bilinear_within_rounding() {
      let mut seed = 0x2545_f491_u32;
      let mut noise = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as u8
      };
      // 4K and 1080p to 720p, the simulcast halves and quarters, a screen's odd ratio, enlarging.
      for ((width, height), (target_width, target_height)) in [
        ((384, 216), (128, 72)),
        ((192, 108), (128, 72)),
        ((128, 72), (64, 36)),
        ((128, 72), (32, 18)),
        ((150, 94), (64, 40)),
        ((64, 36), (128, 72)),
        ((2, 2), (2, 2)),
      ] {
        let plane: Vec<u8> = (0..width * height).map(|_| noise()).collect();
        let mut scaled = Vec::new();
        scale_plane(&plane, width, height, target_width, target_height, &mut scaled);
        let reference = reference_scale(&plane, width, height, target_width, target_height);
        assert_eq!(scaled.len(), reference.len());
        let worst = scaled
          .iter()
          .zip(&reference)
          .map(|(a, b)| a.abs_diff(*b))
          .max()
          .unwrap();
        assert!(
          worst <= 1,
          "{width}x{height} -> {target_width}x{target_height}: off by {worst}"
        );
      }
    }

    #[test]
    fn rtp_timestamp_uses_90_khz() {
      assert_eq!(EncodedVideoFrame::rtp_timestamp(1_000_000), RtpTimestamp(90_000));
      assert_eq!(EncodedVideoFrame::rtp_timestamp(33_333), RtpTimestamp(2_999));
    }
  }

  mod layers {
    use super::*;

    #[test]
    fn hd_source_gets_three_layers() {
      let layers = simulcast_layers(HD, 30, 1_600_000, 3);
      let summary: Vec<_> = layers
        .iter()
        .map(|layer| (layer.layer, layer.width, layer.height, layer.max_bitrate_bps))
        .collect();
      assert_eq!(
        summary,
        vec![
          (0, 320, 180, 100_000),
          (1, 640, 360, 400_000),
          (2, 1280, 720, 1_600_000)
        ]
      );
    }

    #[test]
    fn small_source_and_layer_limit_reduce_layers() {
      let small = VideoResolution {
        width: 320,
        height: 240,
      };
      assert_eq!(simulcast_layers(small, 15, 300_000, 3).len(), 2);
      let limited = simulcast_layers(HD, 30, 1_600_000, 1);
      assert_eq!(limited.len(), 1);
      assert_eq!((limited[0].layer, limited[0].width), (0, 1280));
    }

    fn summary(layers: &[VideoLayerInfo]) -> Vec<(u8, u16, u16, u32)> {
      layers
        .iter()
        .map(|layer| (layer.layer, layer.width, layer.height, layer.max_bitrate_bps))
        .collect()
    }

    /// #967: a 1080p camera sends 1080p on top of the 720p and 360p layers viewers had.
    #[test]
    fn a_1080p_camera_sends_1080p_720p_and_360p() {
      let full_hd = VideoResolution {
        width: 1920,
        height: 1080,
      };
      let layers = camera_layers(full_hd, 30, camera_limit(None));
      assert_eq!(
        summary(&layers),
        vec![
          (0, 640, 360, 375_000),
          (1, 1280, 720, CAMERA_HD_BITRATE_BPS),
          (2, 1920, 1080, CAMERA_MAX_BITRATE_BPS)
        ]
      );
      assert!(layers.iter().all(|layer| layer.max_framerate == 30));
      // A 4K camera is scaled to the same layers.
      let uhd = VideoResolution {
        width: 3840,
        height: 2160,
      };
      assert_eq!(camera_layers(uhd, 30, camera_limit(None)), layers);
    }

    #[test]
    fn a_720p_camera_or_a_cap_at_720p_keeps_the_layers_from_before() {
      let before = simulcast_layers(HD, 30, CAMERA_HD_BITRATE_BPS, 3);
      assert_eq!(camera_layers(HD, 30, camera_limit(None)), before);
      let full_hd = VideoResolution {
        width: 1920,
        height: 1080,
      };
      assert_eq!(camera_layers(full_hd, 30, camera_limit(Some(720))), before);
      assert_eq!(
        summary(&before),
        vec![
          (0, 320, 180, 93_750),
          (1, 640, 360, 375_000),
          (2, 1280, 720, 1_500_000)
        ]
      );
    }

    #[test]
    fn a_cap_at_360p_sends_360p_and_below() {
      let full_hd = VideoResolution {
        width: 1920,
        height: 1080,
      };
      let layers = camera_layers(full_hd, 30, camera_limit(Some(360)));
      let sides: Vec<_> = layers.iter().map(|layer| (layer.width, layer.height)).collect();
      assert_eq!(sides, vec![(160, 90), (320, 180), (640, 360)]);
    }

    #[test]
    fn other_shapes_fit_the_limit_by_their_lines() {
      let res = |width, height| VideoResolution { width, height };
      // 4:3 at 1080 lines: 1440×1080, 960×720, 480×360.
      let four_by_three = camera_layers(res(1440, 1080), 30, camera_limit(None));
      let sides: Vec<_> = four_by_three
        .iter()
        .map(|layer| (layer.width, layer.height))
        .collect();
      assert_eq!(sides, vec![(480, 360), (960, 720), (1440, 1080)]);
      assert!(four_by_three[2].max_bitrate_bps < CAMERA_MAX_BITRATE_BPS);
      // A portrait phone camera fits by its height, as before.
      let portrait = camera_layers(res(1080, 1920), 30, camera_limit(Some(720)));
      let top = portrait.last().unwrap();
      assert_eq!((top.width, top.height), (404, 720), "{portrait:?}");
      assert_eq!(camera_limit(Some(4000)), CAMERA_MAX_RESOLUTION);
      assert_eq!(camera_limit(Some(360)), res(640, 360));
    }
  }

  mod layer_choice {
    use super::*;

    #[test]
    fn camera_tile_gets_the_smallest_layer_that_covers_it() {
      let layers = camera_layers();
      // A thumbnail on a standard display: the quarter-size layer is enough.
      assert_eq!(layer_for_view(&VideoTrackKind::Camera, &layers, res(240, 135)), 0);
      assert_eq!(layer_for_view(&VideoTrackKind::Camera, &layers, res(320, 180)), 0);
      // The same thumbnail at devicePixelRatio 1.5.
      assert_eq!(layer_for_view(&VideoTrackKind::Camera, &layers, res(360, 203)), 1);
      // A grid tile, then an enlarged speaker.
      assert_eq!(layer_for_view(&VideoTrackKind::Camera, &layers, res(640, 360)), 1);
      assert_eq!(layer_for_view(&VideoTrackKind::Camera, &layers, res(900, 506)), 2);
    }

    #[test]
    fn a_tile_larger_than_every_layer_gets_the_top_layer() {
      let layers = camera_layers();
      assert_eq!(
        layer_for_view(&VideoTrackKind::Camera, &layers, res(1920, 1080)),
        TOP_LAYER
      );
      assert_eq!(
        layer_for_view(&VideoTrackKind::Camera, &layers, res(2560, 1440)),
        TOP_LAYER
      );
    }

    #[test]
    fn a_cropped_tile_needs_both_sides_covered() {
      let layers = camera_layers();
      // A square tile crops the 16:9 picture: its height decides.
      assert_eq!(layer_for_view(&VideoTrackKind::Camera, &layers, res(300, 300)), 1);
      // A tall tile (portrait phone layout).
      assert_eq!(layer_for_view(&VideoTrackKind::Camera, &layers, res(360, 640)), 2);
    }

    #[test]
    fn screen_shares_always_get_the_top_layer() {
      let layers = simulcast_layers(res(2560, 1440), 30, 6_000_000, 2);
      assert_eq!(
        layer_for_view(&VideoTrackKind::Screen, &layers, res(320, 180)),
        TOP_LAYER
      );
      assert_eq!(
        layer_for_view(&VideoTrackKind::Screen, &layers, res(0, 0)),
        TOP_LAYER
      );
    }

    #[test]
    fn hidden_tiles_get_the_lowest_layer_and_unknown_tracks_the_top() {
      let layers = camera_layers();
      assert_eq!(layer_for_view(&VideoTrackKind::Camera, &layers, res(0, 0)), 0);
      assert_eq!(layer_for_view(&VideoTrackKind::Camera, &layers, res(640, 0)), 0);
      assert_eq!(
        layer_for_view(&VideoTrackKind::Camera, &[], res(320, 180)),
        TOP_LAYER
      );
    }

    #[test]
    fn a_single_layer_track_is_received_whole() {
      let layers = simulcast_layers(res(640, 360), 30, 500_000, 1);
      assert_eq!(layer_for_view(&VideoTrackKind::Camera, &layers, res(160, 90)), 0);
      assert_eq!(
        layer_for_view(&VideoTrackKind::Camera, &layers, res(1280, 720)),
        TOP_LAYER
      );
    }

    /// #967: a picked quality asks for the largest layer within its lines, whatever the tile.
    #[test]
    fn a_picked_quality_asks_for_the_layer_within_its_lines() {
      let full_hd = super::camera_layers(res(1920, 1080), 30, camera_limit(None));
      let thumbnail = res(320, 180);
      assert_eq!(
        chosen_layer(&VideoTrackKind::Camera, &full_hd, thumbnail, Some(1080)),
        2
      );
      assert_eq!(
        chosen_layer(&VideoTrackKind::Camera, &full_hd, thumbnail, Some(720)),
        1
      );
      assert_eq!(
        chosen_layer(&VideoTrackKind::Camera, &full_hd, res(2560, 1440), Some(360)),
        0
      );
      // Auto: by the tile.
      assert_eq!(
        chosen_layer(&VideoTrackKind::Camera, &full_hd, thumbnail, None),
        0
      );
      // A sender of 720p and less: 1080p asks for its top, 360p for its middle layer, and less
      // than it has for its lowest.
      let hd = camera_layers();
      assert_eq!(layer_for_lines(&hd, 1080), 2);
      assert_eq!(layer_for_lines(&hd, 360), 1);
      assert_eq!(layer_for_lines(&hd, 100), 0);
      assert_eq!(layer_for_lines(&[], 720), TOP_LAYER);
      // A screen share may be picked lower too.
      let screen = screen_layers(
        res(2560, 1440),
        30,
        ScreenContentMode::Detail,
        &ScreenShareResolution::Standard,
      );
      assert_eq!(
        chosen_layer(&VideoTrackKind::Screen, &screen, thumbnail, Some(720)),
        0
      );
    }

    #[test]
    fn a_view_asks_for_the_top_until_its_size_or_a_pick_is_known() {
      let full_hd = super::camera_layers(res(1920, 1080), 30, camera_limit(None));
      let camera = VideoTrackKind::Camera;
      assert_eq!(ViewChoice::default().layer(&camera, &full_hd), TOP_LAYER);
      let small = ViewChoice {
        maybe_shown: Some(res(640, 360)),
        maybe_max_lines: None,
      };
      assert_eq!(small.layer(&camera, &full_hd), 0);
      let picked = ViewChoice {
        maybe_max_lines: Some(1080),
        ..small
      };
      assert_eq!(picked.layer(&camera, &full_hd), 2);
      let picked_only = ViewChoice {
        maybe_shown: None,
        maybe_max_lines: Some(720),
      };
      assert_eq!(picked_only.layer(&camera, &full_hd), 1);
    }

    fn camera_layers() -> Vec<VideoLayerInfo> {
      // 320×180, 640×360, 1280×720.
      simulcast_layers(res(1280, 720), 30, 1_500_000, 3)
    }

    fn res(width: u16, height: u16) -> VideoResolution {
      VideoResolution { width, height }
    }
  }

  mod layer_sizer {
    use super::*;

    #[test]
    fn resized_layers_keep_the_aspect_ratio_their_shares_and_caps() {
      let configured = screen_layers(
        res(2560, 1440),
        30,
        ScreenContentMode::Detail,
        &ScreenShareResolution::Standard,
      );
      let portrait = resized_layers(&configured, res(810, 1440));
      let summary: Vec<_> = portrait
        .iter()
        .map(|layer| (layer.layer, layer.width, layer.height, layer.max_bitrate_bps))
        .collect();
      assert_eq!(
        summary,
        vec![
          (0, 404, 720, configured[0].max_bitrate_bps),
          (1, 810, 1440, configured[1].max_bitrate_bps)
        ]
      );
      // Tiny sources still give valid I420 sides.
      let tiny = resized_layers(&configured, res(2, 2));
      assert!(tiny.iter().all(|layer| layer.width == 2 && layer.height == 2));
    }

    #[test]
    fn the_first_picture_of_another_shape_is_followed_at_once() {
      let (mut sizer, now) = screen_sizer();
      // A portrait window, captured within the 1440p limit.
      let layers = sizer.observe(res(1000, 1600), now).unwrap();
      assert_eq!(sides(&layers), vec![(450, 720), (900, 1440)]);
      assert_eq!(sizer.observe(res(1000, 1600), now), None);
    }

    #[test]
    fn a_source_within_the_tolerance_or_beyond_the_limit_keeps_the_layers() {
      let (mut sizer, now) = screen_sizer();
      assert_eq!(sizer.observe(res(2550, 1438), now), None);
      // Larger than the limit: shrunk to it, which is what the layers already are.
      assert_eq!(sizer.observe(res(3840, 2160), now), None);
      // Odd sides are rounded to even ones.
      let layers = sizer
        .observe(res(1001, 601), now + RESIZE_SETTLE)
        .or_else(|| sizer.observe(res(1001, 601), now + RESIZE_SETTLE * 2))
        .unwrap();
      assert_eq!(sides(&layers), vec![(500, 300), (1000, 600)]);
    }

    #[test]
    fn a_new_size_is_followed_once_it_held_for_the_settle_time() {
      let (mut sizer, now) = screen_sizer();
      assert_eq!(sizer.observe(res(2560, 1440), now), None);
      assert_eq!(sizer.observe(res(1600, 1200), now), None);
      assert_eq!(sizer.observe(res(1600, 1200), now + RESIZE_SETTLE / 2), None);
      let layers = sizer.observe(res(1600, 1200), now + RESIZE_SETTLE).unwrap();
      assert_eq!(sides(&layers), vec![(800, 600), (1600, 1200)]);
    }

    #[test]
    fn a_window_being_dragged_rebuilds_once_it_rests() {
      let (mut sizer, start) = screen_sizer();
      assert_eq!(sizer.observe(res(2560, 1440), start), None);
      let frame = Duration::from_millis(16);
      let mut now = start;
      for step in 0..60u16 {
        now = start + frame * u32::from(step);
        assert_eq!(sizer.observe(res(2000 - step * 10, 1200), now), None);
      }
      // The last size rests (a still window repeats its picture).
      assert_eq!(sizer.observe(res(1410, 1200), now + RESIZE_SETTLE / 2), None);
      let layers = sizer.observe(res(1410, 1200), now + RESIZE_SETTLE).unwrap();
      assert_eq!(sides(&layers), vec![(704, 600), (1410, 1200)]);
    }

    #[test]
    fn a_window_growing_back_returns_to_the_full_layers() {
      let (mut sizer, now) = screen_sizer();
      sizer.observe(res(800, 600), now).unwrap();
      assert_eq!(sizer.observe(res(2560, 1440), now), None);
      let layers = sizer.observe(res(2560, 1440), now + RESIZE_SETTLE).unwrap();
      assert_eq!(sides(&layers), vec![(1280, 720), (2560, 1440)]);
    }

    fn screen_sizer() -> (LayerSizer, Instant) {
      let limit = res(2560, 1440);
      let configured = screen_layers(
        limit,
        30,
        ScreenContentMode::Detail,
        &ScreenShareResolution::Standard,
      );
      (LayerSizer::new(configured, limit), Instant::now())
    }

    fn sides(layers: &[VideoLayerInfo]) -> Vec<(u16, u16)> {
      layers.iter().map(|layer| (layer.width, layer.height)).collect()
    }

    fn res(width: u16, height: u16) -> VideoResolution {
      VideoResolution { width, height }
    }
  }

  mod codecs {
    use super::*;

    #[test]
    fn best_common_codec_is_chosen() {
      let sender = capabilities(&[VideoCodec::H264, VideoCodec::Vp9, VideoCodec::Av1], &[]);
      let modern = capabilities(&[], &[VideoCodec::Av1, VideoCodec::Vp9, VideoCodec::H264]);
      let older = capabilities(&[], &[VideoCodec::Vp9, VideoCodec::H264]);
      assert_eq!(choose_video_codec(&sender, &[&modern]), Some(VideoCodec::Av1));
      assert_eq!(
        choose_video_codec(&sender, &[&modern, &older]),
        Some(VideoCodec::Vp9)
      );
    }

    #[test]
    fn no_common_codec() {
      let sender = capabilities(&[VideoCodec::Av1], &[]);
      let receiver = capabilities(&[], &[VideoCodec::H264]);
      assert_eq!(choose_video_codec(&sender, &[&receiver]), None);
      let unknown = capabilities(&[VideoCodec::Unknown("vvc".into())], &[]);
      let accepts_unknown = capabilities(&[], &[VideoCodec::Unknown("vvc".into())]);
      assert_eq!(choose_video_codec(&unknown, &[&accepts_unknown]), None);
    }

    fn capabilities(encoders: &[VideoCodec], decoders: &[VideoCodec]) -> MediaCapabilities {
      MediaCapabilities {
        video_encoders: encoders.to_vec(),
        video_decoders: decoders.to_vec(),
        max_simulcast_layers: 3,
        ..MediaCapabilities::default()
      }
    }
  }
}
