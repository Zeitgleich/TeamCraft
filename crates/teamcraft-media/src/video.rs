//! Video types: raw and encoded frames, simulcast layer plans and codec choice.
//!
//! Capture and the actual encoders/decoders come later (platform specific: WebCodecs, VideoToolbox,
//! MediaCodec, VA-API); these types are what they plug into.

use teamcraft_proto::{MAX_SIMULCAST_LAYERS, MediaCapabilities, VideoCodec, VideoLayerInfo};
use thiserror::Error;

use crate::datagram::SimulcastLayer;
use crate::rtp::RtpTimestamp;

/// RTP clock rate of video.
pub const VIDEO_CLOCK_RATE_HZ: u32 = 90_000;

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
fn scale_plane(
  plane: &[u8],
  width: usize,
  height: usize,
  target_width: usize,
  target_height: usize,
  out: &mut Vec<u8>,
) {
  // 16.16 fixed point source positions of the target pixel centres.
  let step_x = ((width << 16) / target_width.max(1)) as i64;
  let step_y = ((height << 16) / target_height.max(1)) as i64;
  let sample = |x: usize, y: usize| i64::from(plane[y.min(height - 1) * width + x.min(width - 1)]);
  for target_y in 0..target_height {
    let source_y = ((target_y as i64 * step_y) + step_y / 2 - (1 << 15)).max(0);
    let (y0, fraction_y) = ((source_y >> 16) as usize, source_y & 0xffff);
    for target_x in 0..target_width {
      let source_x = ((target_x as i64 * step_x) + step_x / 2 - (1 << 15)).max(0);
      let (x0, fraction_x) = ((source_x >> 16) as usize, source_x & 0xffff);
      let top = sample(x0, y0) * (0x10000 - fraction_x) + sample(x0 + 1, y0) * fraction_x;
      let bottom = sample(x0, y0 + 1) * (0x10000 - fraction_x) + sample(x0 + 1, y0 + 1) * fraction_x;
      let value = (top * (0x10000 - fraction_y) + bottom * fraction_y + (1 << 31)) >> 32;
      out.push(value.clamp(0, 255) as u8);
    }
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
