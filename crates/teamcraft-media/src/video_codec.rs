//! Video encoders and decoders for native clients, and the simulcast encoder that feeds the
//! engine.
//!
//! Encoders sit behind [`VideoEncoder`] so a hardware encoder (VA-API, VideoToolbox, MediaCodec)
//! replaces the software one without touching the rest; [`OpenH264Encoder`] is the software
//! fallback every platform has, and H.264 is decodable in every browser. Browsers use WebCodecs
//! instead of this module.

use openh264::Timestamp;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::encoder::{
  BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode, UsageType,
};
use openh264::formats::YUVSource;
use teamcraft_proto::{VideoCodec, VideoLayerInfo};
use thiserror::Error;

use crate::datagram::SimulcastLayer;
use crate::openh264_library::{self, OpenH264LibraryError};
use crate::video::{EncodedVideoFrame, VideoError, VideoFrame, VideoResolution};

/// A keyframe at least this often, so receivers recover from losses without asking.
const KEYFRAME_INTERVAL_FRAMES: u32 = 300;

/// Encodes raw frames of one fixed resolution.
pub trait VideoEncoder {
  fn codec(&self) -> VideoCodec;
  /// Encodes one frame; `None` when the encoder dropped it (rate control).
  fn encode(
    &mut self,
    frame: &VideoFrame,
    is_keyframe_forced: bool,
  ) -> Result<Option<EncodedPicture>, VideoCodecError>;
}

/// Decodes one stream.
pub trait VideoDecoder {
  /// Decodes one frame; `None` while the decoder waits for more data (e.g. a keyframe).
  fn decode(&mut self, data: &[u8], timestamp_us: u64) -> Result<Option<VideoFrame>, VideoCodecError>;
}

/// One encoded picture of one layer.
#[derive(Clone, PartialEq, Eq)]
pub struct EncodedPicture {
  pub is_keyframe: bool,
  pub data: Vec<u8>,
}

/// What kind of picture an encoder is tuned for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoContent {
  Camera,
  /// Sharp text, few changes: screen shares.
  Screen,
}

/// Software H.264 (OpenH264).
pub struct OpenH264Encoder {
  encoder: Encoder,
  resolution: VideoResolution,
}

pub struct OpenH264Decoder {
  decoder: Decoder,
}

/// A hardware encoder that hands over to OpenH264 for good when it fails (a lost GPU, a driver
/// error mid-call), with a keyframe so receivers recover right away.
pub struct FallbackEncoder {
  maybe_hardware: Option<Box<dyn VideoEncoder>>,
  maybe_software: Option<OpenH264Encoder>,
  layer: VideoLayerInfo,
  content: VideoContent,
}

/// Encodes every simulcast layer of one track from the full-size frames, skipping layers no
/// receiver needs (the SFU's `LayersNeeded`) and honouring keyframe requests per layer.
pub struct SimulcastEncoder {
  layers: Vec<LayerEncoder>,
}

struct LayerEncoder {
  info: VideoLayerInfo,
  encoder: Box<dyn VideoEncoder>,
  is_needed: bool,
  wants_keyframe: bool,
}

#[derive(Debug, Error)]
pub enum VideoCodecError {
  #[error("video codec: {0}")]
  Codec(String),
  #[error(transparent)]
  Frame(#[from] VideoError),
  #[error("layer {0} does not exist")]
  UnknownLayer(u8),
  #[error(transparent)]
  Library(#[from] OpenH264LibraryError),
}

impl OpenH264Encoder {
  pub fn new(layer: &VideoLayerInfo, content: VideoContent) -> Result<Self, VideoCodecError> {
    let usage = match content {
      VideoContent::Camera => UsageType::CameraVideoRealTime,
      VideoContent::Screen => UsageType::ScreenContentRealTime,
    };
    let config = EncoderConfig::new()
      .bitrate(BitRate::from_bps(layer.max_bitrate_bps))
      .max_frame_rate(FrameRate::from_hz(f32::from(layer.max_framerate.max(1))))
      .usage_type(usage)
      .rate_control_mode(RateControlMode::Bitrate)
      .intra_frame_period(IntraFramePeriod::from_num_frames(KEYFRAME_INTERVAL_FRAMES));
    let encoder = Encoder::with_api_config(openh264_library::api()?, config).map_err(codec_error)?;
    Ok(Self {
      encoder,
      resolution: VideoResolution {
        width: layer.width,
        height: layer.height,
      },
    })
  }
}

impl VideoEncoder for OpenH264Encoder {
  fn codec(&self) -> VideoCodec {
    VideoCodec::H264
  }

  fn encode(
    &mut self,
    frame: &VideoFrame,
    is_keyframe_forced: bool,
  ) -> Result<Option<EncodedPicture>, VideoCodecError> {
    let frame = frame.scaled(self.resolution)?;
    if is_keyframe_forced {
      self.encoder.force_intra_frame();
    }
    let timestamp = Timestamp::from_millis(frame.timestamp_us() / 1000);
    let bitstream = self
      .encoder
      .encode_at(&I420(&frame), timestamp)
      .map_err(codec_error)?;
    let is_keyframe = match bitstream.frame_type() {
      FrameType::IDR | FrameType::I => true,
      FrameType::P | FrameType::IPMixed => false,
      FrameType::Skip | FrameType::Invalid => return Ok(None),
    };
    Ok(Some(EncodedPicture {
      is_keyframe,
      data: bitstream.to_vec(),
    }))
  }
}

impl OpenH264Decoder {
  pub fn new() -> Result<Self, VideoCodecError> {
    let decoder =
      Decoder::with_api_config(openh264_library::api()?, DecoderConfig::new()).map_err(codec_error)?;
    Ok(Self { decoder })
  }
}

impl VideoDecoder for OpenH264Decoder {
  fn decode(&mut self, data: &[u8], timestamp_us: u64) -> Result<Option<VideoFrame>, VideoCodecError> {
    let Some(picture) = self.decoder.decode(data).map_err(codec_error)? else {
      return Ok(None);
    };
    let (width, height) = picture.dimensions();
    let resolution = VideoResolution {
      width: u16::try_from(width).map_err(|_| VideoCodecError::Codec("frame too wide".into()))?,
      height: u16::try_from(height).map_err(|_| VideoCodecError::Codec("frame too high".into()))?,
    };
    let (y_stride, u_stride, v_stride) = picture.strides();
    let mut data = Vec::with_capacity(width * height * 3 / 2);
    copy_plane(picture.y(), y_stride, width, height, &mut data);
    copy_plane(picture.u(), u_stride, width / 2, height / 2, &mut data);
    copy_plane(picture.v(), v_stride, width / 2, height / 2, &mut data);
    Ok(Some(VideoFrame::new(resolution, timestamp_us, data)?))
  }
}

impl FallbackEncoder {
  pub fn new(hardware: Box<dyn VideoEncoder>, layer: &VideoLayerInfo, content: VideoContent) -> Self {
    Self {
      maybe_hardware: Some(hardware),
      maybe_software: None,
      layer: *layer,
      content,
    }
  }
}

impl VideoEncoder for FallbackEncoder {
  fn codec(&self) -> VideoCodec {
    VideoCodec::H264
  }

  fn encode(
    &mut self,
    frame: &VideoFrame,
    is_keyframe_forced: bool,
  ) -> Result<Option<EncodedPicture>, VideoCodecError> {
    let mut is_switching = false;
    if let Some(hardware) = &mut self.maybe_hardware {
      match hardware.encode(frame, is_keyframe_forced) {
        Ok(maybe_picture) => return Ok(maybe_picture),
        Err(error) => {
          tracing::warn!(%error, "hardware encoder failed; continuing in software");
          self.maybe_hardware = None;
          is_switching = true;
        },
      }
    }
    let software = match &mut self.maybe_software {
      Some(software) => software,
      None => self
        .maybe_software
        .insert(OpenH264Encoder::new(&self.layer, self.content)?),
    };
    software.encode(frame, is_keyframe_forced || is_switching)
  }
}

impl SimulcastEncoder {
  /// One encoder per layer from `make_encoder` (a hardware one where available).
  pub fn new(
    layers: &[VideoLayerInfo],
    mut make_encoder: impl FnMut(&VideoLayerInfo) -> Result<Box<dyn VideoEncoder>, VideoCodecError>,
  ) -> Result<Self, VideoCodecError> {
    let layers = layers
      .iter()
      .map(|info| {
        Ok(LayerEncoder {
          info: *info,
          encoder: make_encoder(info)?,
          is_needed: true,
          wants_keyframe: true,
        })
      })
      .collect::<Result<_, VideoCodecError>>()?;
    Ok(Self { layers })
  }

  /// Only `layers` are encoded from now on (the SFU's `LayersNeeded`); a layer coming back starts
  /// with a keyframe.
  pub fn set_needed_layers(&mut self, layers: &[u8]) {
    for layer in &mut self.layers {
      let is_needed = layers.contains(&layer.info.layer);
      if is_needed && !layer.is_needed {
        layer.wants_keyframe = true;
      }
      layer.is_needed = is_needed;
    }
  }

  pub fn request_keyframe(&mut self, layer: u8) -> Result<(), VideoCodecError> {
    let encoder = self
      .layers
      .iter_mut()
      .find(|encoder| encoder.info.layer == layer)
      .ok_or(VideoCodecError::UnknownLayer(layer))?;
    encoder.wants_keyframe = true;
    Ok(())
  }

  /// Encodes `frame` for every needed layer.
  pub fn encode(&mut self, frame: &VideoFrame) -> Result<Vec<EncodedVideoFrame>, VideoCodecError> {
    let timestamp = EncodedVideoFrame::rtp_timestamp(frame.timestamp_us());
    let mut encoded = Vec::new();
    for layer in self.layers.iter_mut().filter(|layer| layer.is_needed) {
      let Some(picture) = layer
        .encoder
        .encode(frame, std::mem::take(&mut layer.wants_keyframe))?
      else {
        continue;
      };
      encoded.push(EncodedVideoFrame {
        codec: layer.encoder.codec(),
        layer: SimulcastLayer::new(layer.info.layer)
          .map_err(|_| VideoCodecError::UnknownLayer(layer.info.layer))?,
        timestamp,
        is_keyframe: picture.is_keyframe,
        data: picture.data,
      });
    }
    Ok(encoded)
  }
}

/// [`VideoFrame`] as OpenH264's input (contiguous I420, no padding).
struct I420<'a>(&'a VideoFrame);

impl YUVSource for I420<'_> {
  fn dimensions(&self) -> (usize, usize) {
    let resolution = self.0.resolution();
    (usize::from(resolution.width), usize::from(resolution.height))
  }

  fn strides(&self) -> (usize, usize, usize) {
    let width = usize::from(self.0.resolution().width);
    (width, width / 2, width / 2)
  }

  fn y(&self) -> &[u8] {
    self.0.planes().0
  }

  fn u(&self) -> &[u8] {
    self.0.planes().1
  }

  fn v(&self) -> &[u8] {
    self.0.planes().2
  }
}

/// Copies a plane with row padding (`stride`) into a tight buffer.
fn copy_plane(plane: &[u8], stride: usize, width: usize, height: usize, out: &mut Vec<u8>) {
  for row in plane.chunks(stride).take(height) {
    out.extend_from_slice(&row[..width.min(row.len())]);
  }
}

fn codec_error(error: impl std::fmt::Display) -> VideoCodecError {
  VideoCodecError::Codec(error.to_string())
}

// Encoded bytes don't belong in logs.
impl std::fmt::Debug for EncodedPicture {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("EncodedPicture")
      .field("is_keyframe", &self.is_keyframe)
      .field("bytes", &self.data.len())
      .finish()
  }
}

/// An I420 frame as NV12 with the encoder's stride and slice height (rows padded with zeros).
// Used by the hardware encoders (MediaCodec, GStreamer); tested on every platform.
#[cfg_attr(not(any(target_os = "android", feature = "gstreamer")), allow(dead_code))]
pub(crate) fn nv12_bytes(frame: &VideoFrame, stride: usize, slice_height: usize) -> Vec<u8> {
  let width = usize::from(frame.resolution().width);
  let height = usize::from(frame.resolution().height);
  let chroma_width = width.div_ceil(2);
  let chroma_height = height.div_ceil(2);
  let mut nv12 = vec![0; stride * slice_height + stride * chroma_height];
  let (luma, cb, cr) = frame.planes();
  for row in 0..height {
    nv12[row * stride..row * stride + width].copy_from_slice(&luma[row * width..(row + 1) * width]);
  }
  let chroma_start = stride * slice_height;
  for row in 0..chroma_height {
    let line = &mut nv12[chroma_start + row * stride..chroma_start + row * stride + 2 * chroma_width];
    for column in 0..chroma_width {
      line[2 * column] = cb[row * chroma_width + column];
      line[2 * column + 1] = cr[row * chroma_width + column];
    }
  }
  nv12
}

/// Whether an Annex B picture already carries its SPS (NAL type 7) up front.
// Used by the Android MediaCodec encoder; tested on every platform.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn starts_with_sps(data: &[u8]) -> bool {
  let header = match data {
    [0, 0, 0, 1, header, ..] | [0, 0, 1, header, ..] => *header,
    _ => return false,
  };
  header & 0x1f == 7
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::video::simulcast_layers;

  const HD: VideoResolution = VideoResolution {
    width: 1280,
    height: 720,
  };

  #[test]
  #[cfg_attr(
    not(feature = "openh264-source"),
    ignore = "needs OpenH264: --features openh264-source"
  )]
  fn h264_round_trip_keeps_the_picture() {
    let layer = simulcast_layers(HD, 30, 2_000_000, 1)[0];
    let mut encoder = OpenH264Encoder::new(&layer, VideoContent::Camera).unwrap();
    let mut decoder = OpenH264Decoder::new().unwrap();
    let mut last = None;
    for index in 0..10 {
      let frame = test_pattern(HD, index);
      let picture = encoder.encode(&frame, false).unwrap().unwrap();
      assert_eq!(
        picture.is_keyframe,
        index == 0,
        "only the first frame is a keyframe"
      );
      if let Some(decoded) = decoder.decode(&picture.data, frame.timestamp_us()).unwrap() {
        last = Some((frame, decoded));
      }
    }
    let (original, decoded) = last.unwrap();
    assert_eq!(decoded.resolution(), HD);
    let psnr = luma_psnr(&original, &decoded);
    assert!(psnr > 30.0, "PSNR {psnr:.1} dB");
  }

  #[test]
  #[cfg_attr(
    not(feature = "openh264-source"),
    ignore = "needs OpenH264: --features openh264-source"
  )]
  fn simulcast_encodes_needed_layers_and_keyframes_on_request() {
    let layers = simulcast_layers(HD, 30, 1_600_000, 3);
    let mut simulcast = SimulcastEncoder::new(&layers, |info| {
      Ok(Box::new(OpenH264Encoder::new(info, VideoContent::Camera)?) as Box<dyn VideoEncoder>)
    })
    .unwrap();
    let mut index = 0;
    let first = encode_frames(&mut simulcast, &mut index, 1);
    assert_eq!(layers_of(&first), vec![0, 1, 2]);
    assert!(first.iter().all(|frame| frame.is_keyframe));
    assert!(
      first[0].data.len() < first[2].data.len(),
      "lower layers are smaller"
    );

    simulcast.set_needed_layers(&[0, 1]);
    let paused = encode_frames(&mut simulcast, &mut index, 30);
    assert!(
      layers_of(&paused).iter().all(|layer| *layer < 2),
      "layer 2 is paused"
    );
    assert!(paused.iter().all(|frame| !frame.is_keyframe));
    assert!(!paused.is_empty());

    simulcast.request_keyframe(1).unwrap();
    simulcast.set_needed_layers(&[0, 1, 2]);
    let resumed = encode_frames(&mut simulcast, &mut index, 1);
    for frame in &resumed {
      let expected = frame.layer.value() != 0;
      assert_eq!(frame.is_keyframe, expected, "layer {}", frame.layer.value());
    }
    assert!(layers_of(&resumed).contains(&2));
    assert!(matches!(
      simulcast.request_keyframe(7),
      Err(VideoCodecError::UnknownLayer(7))
    ));
  }

  #[test]
  fn i420_frames_become_nv12_with_the_encoders_stride() {
    // 4x2 frame: luma 0..8, one chroma row of two samples each.
    let mut data: Vec<u8> = (0..8).collect();
    data.extend([100, 101, 200, 201]);
    let frame = VideoFrame::new(VideoResolution { width: 4, height: 2 }, 0, data).unwrap();
    let nv12 = nv12_bytes(&frame, 6, 2);
    assert_eq!(nv12.len(), 18);
    assert_eq!(&nv12[0..6], &[0, 1, 2, 3, 0, 0]);
    assert_eq!(&nv12[6..12], &[4, 5, 6, 7, 0, 0]);
    assert_eq!(&nv12[12..16], &[100, 200, 101, 201]);
  }

  #[test]
  fn sps_is_found_at_the_start_of_a_picture() {
    assert!(starts_with_sps(&[0, 0, 0, 1, 0x67, 1]));
    assert!(starts_with_sps(&[0, 0, 1, 0x27]));
    assert!(!starts_with_sps(&[0, 0, 0, 1, 0x65]));
    assert!(!starts_with_sps(&[]));
  }

  fn encode_frames(simulcast: &mut SimulcastEncoder, index: &mut u32, count: u32) -> Vec<EncodedVideoFrame> {
    let mut frames = Vec::new();
    for _ in 0..count {
      frames.extend(simulcast.encode(&test_pattern(HD, *index)).unwrap());
      *index += 1;
    }
    frames
  }

  fn layers_of(frames: &[EncodedVideoFrame]) -> Vec<u8> {
    frames.iter().map(|frame| frame.layer.value()).collect()
  }

  /// A moving gradient: compressible, but not trivially.
  fn test_pattern(resolution: VideoResolution, index: u32) -> VideoFrame {
    let (width, height) = (usize::from(resolution.width), usize::from(resolution.height));
    let mut data = Vec::with_capacity(width * height * 3 / 2);
    for y in 0..height {
      for x in 0..width {
        data.push(((x + y + index as usize * 4) % 256) as u8);
      }
    }
    data.extend(std::iter::repeat_n(110, width * height / 4));
    data.extend(std::iter::repeat_n(150, width * height / 4));
    VideoFrame::new(resolution, u64::from(index) * 33_333, data).unwrap()
  }

  fn luma_psnr(a: &VideoFrame, b: &VideoFrame) -> f64 {
    let (ya, yb) = (a.planes().0, b.planes().0);
    let mse: f64 = ya
      .iter()
      .zip(yb)
      .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
      .sum::<f64>()
      / ya.len() as f64;
    10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10()
  }
}
