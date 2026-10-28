//! Video types: raw and encoded frames, simulcast layer plans and codec choice.
//!
//! Capture and the actual encoders/decoders come later (platform specific: WebCodecs, VideoToolbox,
//! MediaCodec, VA-API); these types are what they plug into.

use teamcraft_proto::{
  MAX_SIMULCAST_LAYERS, MediaCapabilities, ScreenContentMode, VideoCodec, VideoLayerInfo, VideoTrackKind,
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

/// The largest screen share picture of each content mode (#530): detail up to 1440p at 30 fps,
/// motion up to 1080p at 60 fps.
pub fn screen_limits(mode: ScreenContentMode) -> (VideoResolution, u8) {
  match mode {
    ScreenContentMode::Motion => (
      VideoResolution {
        width: 1920,
        height: 1080,
      },
      60,
    ),
    ScreenContentMode::Detail | ScreenContentMode::Unknown => (
      VideoResolution {
        width: 2560,
        height: 1440,
      },
      30,
    ),
  }
}

/// Bits per pixel of a screen share's top layer: enough for text to stay sharp in H.264.
const SCREEN_BITS_PER_PIXEL: f64 = 0.06;
const SCREEN_MIN_BITRATE_BPS: u32 = 2_000_000;
const SCREEN_MAX_BITRATE_BPS: u32 = 8_000_000;

/// The top layer's bitrate for a screen share of `resolution` at `framerate`: it follows the
/// pixels per second, between 2 and 8 Mbit/s (1080p30 about 4, 1440p30 about 6.6).
pub fn screen_bitrate_bps(resolution: VideoResolution, framerate: u8) -> u32 {
  let pixels_per_second = f64::from(resolution.width) * f64::from(resolution.height) * f64::from(framerate);
  // Clamped to the u32 range above, so the cast cannot truncate.
  (pixels_per_second * SCREEN_BITS_PER_PIXEL).clamp(
    f64::from(SCREEN_MIN_BITRATE_BPS),
    f64::from(SCREEN_MAX_BITRATE_BPS),
  ) as u32
}

/// Layers of a screen share captured at `source`: the picture within the mode's limits, plus a
/// half-size layer at a quarter of the bitrate for viewers on weak links (the uplink budget drops
/// the big layer first, so a slow link or viewer doesn't lower the quality for the others).
pub fn screen_layers(source: VideoResolution, framerate: u8, mode: ScreenContentMode) -> Vec<VideoLayerInfo> {
  let (limit, max_framerate) = screen_limits(mode);
  let resolution = fit_within(source, limit);
  let framerate = framerate.clamp(1, max_framerate);
  simulcast_layers(
    resolution,
    framerate,
    screen_bitrate_bps(resolution, framerate),
    2,
  )
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
    let mbps = |resolution, framerate| screen_bitrate_bps(resolution, framerate) as f64 / 1e6;
    assert!((3.5..4.0).contains(&mbps(res(1920, 1080), 30)));
    assert!((6.0..7.0).contains(&mbps(res(2560, 1440), 30)));
    assert!((7.0..=8.0).contains(&mbps(res(1920, 1080), 60)));
    assert_eq!(screen_bitrate_bps(res(2560, 1440), 60), 8_000_000);
    assert_eq!(screen_bitrate_bps(res(640, 360), 15), 2_000_000);
  }

  #[test]
  fn screen_layers_keep_text_sharp_or_motion_smooth() {
    let res = |width, height| VideoResolution { width, height };
    let detail = screen_layers(res(3840, 2160), 60, ScreenContentMode::Detail);
    let top = detail.last().unwrap();
    assert_eq!((top.width, top.height, top.max_framerate), (2560, 1440, 30));
    assert_eq!(detail.len(), 2);
    assert_eq!((detail[0].width, detail[0].height), (1280, 720));
    let motion = screen_layers(res(2560, 1440), 60, ScreenContentMode::Motion);
    let top = motion.last().unwrap();
    assert_eq!((top.width, top.height, top.max_framerate), (1920, 1080, 60));
    // A small window is not enlarged.
    let small = screen_layers(res(800, 600), 30, ScreenContentMode::Detail);
    assert_eq!(
      (small.last().unwrap().width, small.last().unwrap().height),
      (800, 600)
    );
  }

  const HD: VideoResolution = VideoResolution {
    width: 1280,
    height: 720,
  };

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
      let configured = screen_layers(res(2560, 1440), 30, ScreenContentMode::Detail);
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
      let configured = screen_layers(limit, 30, ScreenContentMode::Detail);
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
