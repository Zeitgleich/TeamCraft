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
use tracing::warn;

use crate::datagram::SimulcastLayer;
use crate::openh264_library::{self, OpenH264LibraryError};
use crate::video::{EncodedVideoFrame, VideoError, VideoFrame, VideoResolution};

/// A keyframe at least this often, so receivers recover from losses without asking.
const KEYFRAME_INTERVAL_FRAMES: u32 = 300;
/// The frame rate OpenH264's rate control plans a screen share's pictures with. A screen
/// delivers a picture only when something changes, mostly a few a second; planned for the
/// layer's 30, every picture got a thirtieth of a second's bits: after a keyframe of text the
/// encoder skipped pictures for seconds, and a keyframe asked for later came out blurred and
/// took ten seconds to sharpen (issue #941). The rate control follows the pictures' timestamps,
/// so a screen that does change 30 times a second still stays within the layer's bitrate.
const SCREEN_PLANNED_FRAMERATE: u8 = 5;
/// A hardware encoder that took this many frames in a row without giving a picture...
const STALL_MIN_FRAMES: u32 = 10;
/// ...over at least this much capture time has stalled; software takes over. Short enough that
/// the others barely notice, long enough for an encoder's start-up latency.
const STALL_MIN_SPAN_US: u64 = 1_500_000;

/// Encodes raw frames of one fixed resolution.
pub trait VideoEncoder {
  fn codec(&self) -> VideoCodec;
  /// Which encoder this is (`OpenH264`, the GStreamer element, `MediaCodec`), for logs and for
  /// the detail of a track that encodes nothing (#935).
  fn name(&self) -> &'static str;
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

/// The name of [`OpenH264Encoder`].
const OPENH264: &str = "OpenH264";
/// The name of the software encoder every platform has; the others are hardware encoders.
pub const SOFTWARE_ENCODER_NAME: &str = OPENH264;

/// Software H.264 (OpenH264).
pub struct OpenH264Encoder {
  encoder: Encoder,
  resolution: VideoResolution,
}

pub struct OpenH264Decoder {
  decoder: Decoder,
}

/// A hardware encoder that hands over to software for good when it fails (a lost GPU, a driver
/// error mid-call) or stalls (takes frames but gives no pictures, issue #683), with a keyframe so
/// receivers recover right away.
pub struct FallbackEncoder {
  maybe_hardware: Option<Box<dyn VideoEncoder>>,
  /// The hardware encoder's name, also once it is gone.
  hardware_name: &'static str,
  /// Why the hardware encoder was given up, for the error when software can't take over either.
  maybe_hardware_failure: Option<String>,
  maybe_software: Option<Box<dyn VideoEncoder>>,
  make_software: SoftwareEncoderFactory,
  layer: VideoLayerInfo,
  content: VideoContent,
  stall: StallWatch,
}

/// Builds the software encoder a [`FallbackEncoder`] switches to (OpenH264; a fake in tests).
pub type SoftwareEncoderFactory =
  Box<dyn Fn(&VideoLayerInfo, VideoContent) -> Result<Box<dyn VideoEncoder>, VideoCodecError> + Send>;

/// Tells a stalled encoder from one that drops a frame now and then: no picture for
/// [`STALL_MIN_FRAMES`] frames spanning [`STALL_MIN_SPAN_US`] of capture time. Measured with the
/// frames' own timestamps, so it needs no clock of its own.
#[derive(Debug, Default)]
struct StallWatch {
  maybe_first_unanswered_us: Option<u64>,
  unanswered_frames: u32,
}

/// What a [`FallbackEncoder`]'s hardware made of one frame.
enum HardwareAnswer {
  /// A picture, or none for this frame (encoder latency, a dropped frame).
  Encoded(Option<EncodedPicture>),
  /// The hardware is gone (failed or stalled), or was already.
  Gone,
}

/// Encodes every simulcast layer of one track from the full-size frames, skipping layers no
/// receiver needs (the SFU's `LayersNeeded`) and honouring keyframe requests per layer.
pub struct SimulcastEncoder {
  layers: Vec<LayerEncoder>,
}

struct LayerEncoder {
  info: VideoLayerInfo,
  /// What the encoder is built for: the layer's bitrate, or less on a limited uplink (#939).
  bitrate_bps: u32,
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
    let (usage, rate_control, framerate) = match content {
      VideoContent::Camera => (
        UsageType::CameraVideoRealTime,
        RateControlMode::Bitrate,
        layer.max_framerate,
      ),
      VideoContent::Screen => (
        UsageType::ScreenContentRealTime,
        RateControlMode::Timestamp,
        layer.max_framerate.min(SCREEN_PLANNED_FRAMERATE),
      ),
    };
    let config = EncoderConfig::new()
      .bitrate(BitRate::from_bps(layer.max_bitrate_bps))
      .max_frame_rate(FrameRate::from_hz(f32::from(framerate.max(1))))
      .usage_type(usage)
      .rate_control_mode(rate_control)
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

  fn name(&self) -> &'static str {
    OPENH264
  }

  fn encode(
    &mut self,
    frame: &VideoFrame,
    is_keyframe_forced: bool,
  ) -> Result<Option<EncodedPicture>, VideoCodecError> {
    let frame = frame.at_size(self.resolution)?;
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
  /// Falls back to OpenH264.
  pub fn new(hardware: Box<dyn VideoEncoder>, layer: &VideoLayerInfo, content: VideoContent) -> Self {
    Self::with_software(
      hardware,
      layer,
      content,
      Box::new(|layer, content| Ok(Box::new(OpenH264Encoder::new(layer, content)?) as Box<dyn VideoEncoder>)),
    )
  }

  /// Falls back to what `make_software` builds.
  pub fn with_software(
    hardware: Box<dyn VideoEncoder>,
    layer: &VideoLayerInfo,
    content: VideoContent,
    make_software: SoftwareEncoderFactory,
  ) -> Self {
    Self {
      hardware_name: hardware.name(),
      maybe_hardware: Some(hardware),
      maybe_hardware_failure: None,
      maybe_software: None,
      make_software,
      layer: *layer,
      content,
      stall: StallWatch::default(),
    }
  }

  /// Whether the hardware encoder still does the work.
  pub fn is_hardware(&self) -> bool {
    self.maybe_hardware.is_some()
  }

  /// What the hardware made of `frame`; [`HardwareAnswer::Gone`] once it failed or stalled (it
  /// is then dropped, and software takes over).
  fn encode_in_hardware(&mut self, frame: &VideoFrame, is_keyframe_forced: bool) -> HardwareAnswer {
    let Some(hardware) = self.maybe_hardware.as_mut() else {
      return HardwareAnswer::Gone;
    };
    let layer = (self.layer.width, self.layer.height);
    match hardware.encode(frame, is_keyframe_forced) {
      Ok(Some(picture)) => {
        self.stall = StallWatch::default();
        HardwareAnswer::Encoded(Some(picture))
      },
      Ok(None) if self.stall.is_stalled_after(frame.timestamp_us()) => {
        let encoder = self.hardware_name;
        warn!(
          encoder,
          ?layer,
          "hardware encoder gives no pictures; continuing in software"
        );
        self.give_up_hardware("it gave no pictures".into())
      },
      Ok(None) => HardwareAnswer::Encoded(None),
      Err(error) => {
        let encoder = self.hardware_name;
        warn!(%error, encoder, ?layer, "hardware encoder failed; continuing in software");
        self.give_up_hardware(error.to_string())
      },
    }
  }

  /// Drops the hardware encoder, which failed for `reason`; software takes over.
  fn give_up_hardware(&mut self, reason: String) -> HardwareAnswer {
    self.maybe_hardware = None;
    self.maybe_hardware_failure = Some(reason);
    HardwareAnswer::Gone
  }

  /// Why neither encoder works: what became of the hardware one, and `error` of the software one
  /// that couldn't be built (#935: the failure the user sees names both).
  fn without_software(&self, error: &VideoCodecError) -> VideoCodecError {
    let hardware = self.hardware_name;
    let why = self.maybe_hardware_failure.as_deref().unwrap_or("it is gone");
    VideoCodecError::Codec(format!(
      "{hardware} failed ({why}), and {OPENH264} could not take over: {error}"
    ))
  }
}

impl VideoEncoder for FallbackEncoder {
  fn codec(&self) -> VideoCodec {
    VideoCodec::H264
  }

  /// The encoder doing the work: the hardware's until it is gone, then the software's.
  fn name(&self) -> &'static str {
    match (&self.maybe_hardware, &self.maybe_software) {
      (Some(hardware), _) => hardware.name(),
      (None, Some(software)) => software.name(),
      (None, None) => OPENH264,
    }
  }

  fn encode(
    &mut self,
    frame: &VideoFrame,
    is_keyframe_forced: bool,
  ) -> Result<Option<EncodedPicture>, VideoCodecError> {
    let was_hardware = self.is_hardware();
    if let HardwareAnswer::Encoded(maybe_picture) = self.encode_in_hardware(frame, is_keyframe_forced) {
      return Ok(maybe_picture);
    }
    let is_switching = was_hardware;
    let software = match self.maybe_software.take() {
      Some(software) => software,
      None => {
        (self.make_software)(&self.layer, self.content).map_err(|error| self.without_software(&error))?
      },
    };
    let software = self.maybe_software.insert(software);
    software.encode(frame, is_keyframe_forced || is_switching)
  }
}

impl StallWatch {
  /// Counts a frame that gave no picture; `true` once that makes a stall.
  fn is_stalled_after(&mut self, timestamp_us: u64) -> bool {
    let first_us = *self.maybe_first_unanswered_us.get_or_insert(timestamp_us);
    self.unanswered_frames = self.unanswered_frames.saturating_add(1);
    self.unanswered_frames >= STALL_MIN_FRAMES && timestamp_us.saturating_sub(first_us) >= STALL_MIN_SPAN_US
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
          bitrate_bps: info.max_bitrate_bps,
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

  /// Whether a needed layer waits for a keyframe (asked for, not yet encoded).
  pub fn wants_keyframe(&self) -> bool {
    self
      .layers
      .iter()
      .any(|layer| layer.is_needed && layer.wants_keyframe)
  }

  /// Whether anybody receives a layer; with none, encoding is wasted.
  pub fn has_needed_layers(&self) -> bool {
    self.layers.iter().any(|layer| layer.is_needed)
  }

  /// Encodes `frame` on the lowest layer although no layer is needed, only to learn whether the
  /// encoders work: a started track's first picture says so, also while nobody takes a layer yet
  /// or the uplink holds them all back (#935: such a track was taken for one whose encoder
  /// failed). The picture is dropped; `true` if one came out. The layer starts with a keyframe
  /// once it is needed.
  pub fn encode_unsent(&mut self, frame: &VideoFrame) -> Result<bool, VideoCodecError> {
    let Some(layer) = self.layers.first_mut() else {
      return Ok(false);
    };
    layer.wants_keyframe = true;
    Ok(layer.encoder.encode(frame, false)?.is_some())
  }

  /// The encoders at work, e.g. `vtenc_h264`, or `vtenc_h264, OpenH264` once a layer fell back.
  pub fn encoder_names(&self) -> String {
    let mut names: Vec<&'static str> = Vec::new();
    for layer in &self.layers {
      let name = layer.encoder.name();
      if !names.contains(&name) {
        names.push(name);
      }
    }
    names.join(", ")
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

  /// The same layers at a new size (the source changed size, #931): every layer gets a new
  /// encoder from `make_encoder`, which starts with a keyframe. On an error the old encoders stay.
  pub fn resize(
    &mut self,
    layers: &[VideoLayerInfo],
    mut make_encoder: impl FnMut(&VideoLayerInfo) -> Result<Box<dyn VideoEncoder>, VideoCodecError>,
  ) -> Result<(), VideoCodecError> {
    let mut rebuilt = Vec::with_capacity(self.layers.len());
    for layer in &self.layers {
      let info = layers
        .iter()
        .find(|info| info.layer == layer.info.layer)
        .ok_or(VideoCodecError::UnknownLayer(layer.info.layer))?;
      // A bitrate lowered for the uplink stays lowered.
      let bitrate_bps = if layer.bitrate_bps == layer.info.max_bitrate_bps {
        info.max_bitrate_bps
      } else {
        layer.bitrate_bps
      };
      let built = VideoLayerInfo {
        max_bitrate_bps: bitrate_bps,
        ..*info
      };
      rebuilt.push((*info, bitrate_bps, make_encoder(&built)?));
    }
    for (layer, (info, bitrate_bps, encoder)) in self.layers.iter_mut().zip(rebuilt) {
      layer.info = info;
      layer.bitrate_bps = bitrate_bps;
      layer.encoder = encoder;
      layer.wants_keyframe = true;
    }
    Ok(())
  }

  /// Encodes the layers at `bitrates` (layer, bit/s) from now on (#939: the top layer of a camera
  /// at less than its bitrate on a limited uplink). A layer whose bitrate changes gets a new
  /// encoder from `make_encoder` and starts with a keyframe (not every encoder can change its
  /// bitrate on the fly); the others go on. On an error that layer keeps its encoder.
  pub fn set_bitrates(
    &mut self,
    bitrates: &[(u8, u32)],
    mut make_encoder: impl FnMut(&VideoLayerInfo) -> Result<Box<dyn VideoEncoder>, VideoCodecError>,
  ) -> Result<(), VideoCodecError> {
    for layer in &mut self.layers {
      let Some(&(_, bitrate_bps)) = bitrates.iter().find(|(id, _)| *id == layer.info.layer) else {
        continue;
      };
      if bitrate_bps == layer.bitrate_bps || bitrate_bps == 0 {
        continue;
      }
      let built = VideoLayerInfo {
        max_bitrate_bps: bitrate_bps,
        ..layer.info
      };
      layer.encoder = make_encoder(&built)?;
      layer.bitrate_bps = bitrate_bps;
      layer.wants_keyframe = true;
    }
    Ok(())
  }

  /// Encodes `frame` for every needed layer, lowest layer first. A layer half the size of a
  /// larger one is shrunk from that one's picture (2x2 averages): a 1080p camera's 360p layer
  /// from its 720p one rather than by three from 1080p, which is sharper and cheaper (#967).
  pub fn encode(&mut self, frame: &VideoFrame) -> Result<Vec<EncodedVideoFrame>, VideoCodecError> {
    let timestamp = EncodedVideoFrame::rtp_timestamp(frame.timestamp_us());
    let mut encoded = Vec::new();
    // The pictures scaled so far, largest first: each needed layer, from the top down.
    let mut scaled: Vec<VideoFrame> = Vec::new();
    for layer in self.layers.iter_mut().rev().filter(|layer| layer.is_needed) {
      let size = VideoResolution {
        width: layer.info.width,
        height: layer.info.height,
      };
      let source = scaled
        .iter()
        .find(|picture| {
          let larger = picture.resolution();
          larger.width == size.width * 2 && larger.height == size.height * 2
        })
        .unwrap_or(frame);
      let maybe_shrunk = if source.resolution() == size {
        None
      } else {
        Some(source.scaled(size)?)
      };
      let wants_keyframe = std::mem::take(&mut layer.wants_keyframe);
      let maybe_picture = layer
        .encoder
        .encode(maybe_shrunk.as_ref().unwrap_or(source), wants_keyframe)?;
      scaled.extend(maybe_shrunk);
      let Some(picture) = maybe_picture else {
        // A dropped frame doesn't satisfy a keyframe request: the next frame asks again.
        layer.wants_keyframe = wants_keyframe;
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
    encoded.reverse();
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
  use std::sync::{Arc, Mutex};

  use teamcraft_proto::ScreenContentMode;

  use super::*;
  use crate::video::{screen_layers, simulcast_layers};

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

  /// Issue #939: a layer whose bitrate changes gets an encoder built for it, which starts with a
  /// keyframe and stays within the new bitrate; the other layers go on as they were, and a resize
  /// keeps the lowered bitrate.
  #[test]
  #[cfg_attr(
    not(feature = "openh264-source"),
    ignore = "needs OpenH264: --features openh264-source"
  )]
  fn a_layer_bitrate_change_rebuilds_only_that_layer() {
    let layers = simulcast_layers(HD, 30, 1_600_000, 3);
    let built: Arc<Mutex<Vec<(u8, u32)>>> = Arc::default();
    let make = |built: Arc<Mutex<Vec<(u8, u32)>>>| {
      move |info: &VideoLayerInfo| {
        built.lock().unwrap().push((info.layer, info.max_bitrate_bps));
        Ok(Box::new(OpenH264Encoder::new(info, VideoContent::Camera)?) as Box<dyn VideoEncoder>)
      }
    };
    let mut simulcast = SimulcastEncoder::new(&layers, make(Arc::clone(&built))).unwrap();
    let mut index = 0;
    encode_frames(&mut simulcast, &mut index, 30);
    built.lock().unwrap().clear();
    let lowered = [
      (0, layers[0].max_bitrate_bps),
      (1, layers[1].max_bitrate_bps),
      (2, 560_000),
    ];
    simulcast
      .set_bitrates(&lowered, make(Arc::clone(&built)))
      .unwrap();
    assert_eq!(*built.lock().unwrap(), vec![(2, 560_000)]);
    let next = encode_frames(&mut simulcast, &mut index, 1);
    for frame in &next {
      let expected = frame.layer.value() == 2;
      assert_eq!(frame.is_keyframe, expected, "layer {}", frame.layer.value());
    }
    // The same bitrates again rebuild nothing.
    simulcast
      .set_bitrates(&lowered, make(Arc::clone(&built)))
      .unwrap();
    assert_eq!(built.lock().unwrap().len(), 1);
    // A source of another size rebuilds every layer, the lowered one at its lowered bitrate.
    built.lock().unwrap().clear();
    simulcast.resize(&layers, make(Arc::clone(&built))).unwrap();
    assert_eq!(
      *built.lock().unwrap(),
      vec![
        (0, layers[0].max_bitrate_bps),
        (1, layers[1].max_bitrate_bps),
        (2, 560_000)
      ]
    );
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

  /// #967: a 1080p camera's 360p layer is shrunk from its 720p picture (2x2 averages), not by
  /// three from 1080p, and (#978) its 180p layer from the 360p one; each encoder gets its layer's
  /// size, and a layer on its own the frame.
  #[test]
  fn lower_layers_are_shrunk_from_a_layer_twice_their_size() {
    let full_hd = VideoResolution {
      width: 1920,
      height: 1080,
    };
    let layers = crate::video::camera_layers(full_hd, 30, crate::video::camera_limit(None));
    let seen: Arc<Mutex<Vec<(u8, VideoFrame)>>> = Arc::default();
    let mut simulcast = SimulcastEncoder::new(&layers, |info| {
      Ok(Box::new(Seeing {
        layer: info.layer,
        seen: Arc::clone(&seen),
      }) as Box<dyn VideoEncoder>)
    })
    .unwrap();
    let frame = crate::video_capture::test_pattern(full_hd, 3, 0).unwrap();
    let encoded = simulcast.encode(&frame).unwrap();
    assert_eq!(layers_of(&encoded), vec![0, 1, 2, 3], "lowest first, as before");
    let first = std::mem::take(&mut *seen.lock().unwrap());
    let input = |layer: u8| &first.iter().find(|(id, _)| *id == layer).unwrap().1;
    let hd = frame.scaled(input(2).resolution()).unwrap();
    assert_eq!(input(3), &frame);
    assert_eq!(input(2), &hd);
    let quarter = hd.scaled(input(1).resolution()).unwrap();
    assert_eq!(input(1), &quarter);
    assert_eq!(input(0), &quarter.scaled(input(0).resolution()).unwrap());
    let sides = |layer: u8| (input(layer).resolution().width, input(layer).resolution().height);
    assert_eq!((sides(1), sides(0)), ((640, 360), (320, 180)));

    // Only the 360p layer needed: shrunk from the frame itself.
    simulcast.set_needed_layers(&[1]);
    simulcast.encode(&frame).unwrap();
    let alone = std::mem::take(&mut *seen.lock().unwrap());
    let small = VideoResolution {
      width: 640,
      height: 360,
    };
    assert_eq!(alone, vec![(1, frame.scaled(small).unwrap())]);
  }

  /// Records the frames it is given.
  struct Seeing {
    layer: u8,
    seen: Arc<Mutex<Vec<(u8, VideoFrame)>>>,
  }

  impl VideoEncoder for Seeing {
    fn codec(&self) -> VideoCodec {
      VideoCodec::H264
    }

    fn name(&self) -> &'static str {
      "seeing"
    }

    fn encode(
      &mut self,
      frame: &VideoFrame,
      is_keyframe_forced: bool,
    ) -> Result<Option<EncodedPicture>, VideoCodecError> {
      self.seen.lock().unwrap().push((self.layer, frame.clone()));
      Ok(Some(EncodedPicture {
        is_keyframe: is_keyframe_forced,
        data: vec![0, 0, 0, 1, 0x65],
      }))
    }
  }

  #[test]
  #[cfg_attr(
    not(feature = "openh264-source"),
    ignore = "needs OpenH264: --features openh264-source"
  )]
  fn a_resized_source_is_encoded_and_decoded_at_its_new_shape() {
    let layers = simulcast_layers(HD, 30, 2_000_000, 2);
    let open = |info: &VideoLayerInfo| {
      Ok(Box::new(OpenH264Encoder::new(info, VideoContent::Screen)?) as Box<dyn VideoEncoder>)
    };
    let mut simulcast = SimulcastEncoder::new(&layers, open).unwrap();
    let mut decoders = [OpenH264Decoder::new().unwrap(), OpenH264Decoder::new().unwrap()];
    for index in 0..5 {
      for decoded in decode_layers(&mut simulcast, &mut decoders, &test_pattern(HD, index)) {
        assert_eq!(decoded.0.resolution(), sizes_of(&layers)[usize::from(decoded.1)]);
      }
    }
    // The shared window turns portrait: the same layers at its shape, starting with keyframes.
    let portrait = VideoResolution {
      width: 360,
      height: 640,
    };
    let resized = crate::video::resized_layers(&layers, portrait);
    simulcast.resize(&resized, open).unwrap();
    assert!(simulcast.wants_keyframe());
    let mut last = None;
    for index in 5..10 {
      let frame = test_pattern(portrait, index);
      let encoded = simulcast.encode(&frame).unwrap();
      assert_eq!(layers_of(&encoded), vec![0, 1]);
      assert!(encoded.iter().all(|picture| picture.is_keyframe == (index == 5)));
      for picture in encoded {
        let layer = usize::from(picture.layer.value());
        let decoded = decoders[layer]
          .decode(&picture.data, frame.timestamp_us())
          .unwrap()
          .unwrap();
        assert_eq!(decoded.resolution(), sizes_of(&resized)[layer]);
        if layer == 1 {
          last = Some((frame.clone(), decoded));
        }
      }
    }
    // Not stretched: the decoded picture matches what was captured.
    let (original, decoded) = last.unwrap();
    let psnr = luma_psnr(&original, &decoded);
    assert!(psnr > 30.0, "PSNR {psnr:.1} dB");
  }

  /// Issue #941: a still screen delivers a picture only when something changes, here twice a
  /// second. Planned for 30 pictures a second, OpenH264 skipped the pictures after a keyframe of
  /// text for seconds, and a keyframe asked for later came out blurred.
  #[test]
  #[cfg_attr(
    not(feature = "openh264-source"),
    ignore = "needs OpenH264: --features openh264-source"
  )]
  fn a_still_screen_of_text_stays_sharp_through_keyframes() {
    let layer = *screen_layers(
      HD,
      30,
      ScreenContentMode::Detail,
      &teamcraft_proto::ScreenShareResolution::Standard,
    )
    .last()
    .unwrap();
    let mut encoder = OpenH264Encoder::new(&layer, VideoContent::Screen).unwrap();
    let mut decoder = OpenH264Decoder::new().unwrap();
    let mut skipped = Vec::new();
    for index in 0..12u32 {
      let frame = text_page(HD, index % 2 == 0).with_timestamp_us(u64::from(index) * 500_000);
      let is_keyframe_forced = index == 0 || index == 6;
      let Some(picture) = encoder.encode(&frame, is_keyframe_forced).unwrap() else {
        skipped.push(index);
        continue;
      };
      assert_eq!(picture.is_keyframe, is_keyframe_forced, "picture {index}");
      if let Some(decoded) = decoder.decode(&picture.data, frame.timestamp_us()).unwrap() {
        let psnr = luma_psnr(&frame, &decoded);
        // Dense 1 pixel strokes are the hardest case for H.264; 32 dB keeps them readable.
        assert!(psnr > 32.0, "picture {index}: PSNR {psnr:.1} dB");
      }
    }
    // At most the picture right after the first keyframe gives way to it (before: every
    // picture but the keyframes was skipped, and the second keyframe came out at 31 dB).
    assert!(skipped.iter().all(|index| *index == 1), "skipped {skipped:?}");
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

  mod fallback {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// 30 fps.
    const FRAME_INTERVAL_US: u64 = 33_334;

    #[test]
    fn a_stalled_hardware_encoder_hands_over_to_software_with_a_keyframe() {
      let (mut encoder, software_log) = fallback_encoder(FakeEncoder::Stalled);
      let outputs = encode_many(&mut encoder, 60);
      // 10 frames are not enough on their own: the stall also has to last 1.5 s of capture time.
      let first = outputs.iter().position(Option::is_some).unwrap();
      assert_eq!(first, 45, "1.5 s at 30 fps");
      assert!(!encoder.is_hardware());
      assert!(
        outputs[first].as_ref().unwrap().is_keyframe,
        "receivers recover at once"
      );
      assert!(outputs[first..].iter().all(Option::is_some));
      assert_eq!(software_log.lock().unwrap().first(), Some(&true));
    }

    #[test]
    fn a_hardware_encoder_that_drops_some_frames_keeps_the_work() {
      let (mut encoder, software_log) = fallback_encoder(FakeEncoder::EveryOther(0));
      let outputs = encode_many(&mut encoder, 300);
      assert!(encoder.is_hardware());
      assert_eq!(outputs.iter().filter(|output| output.is_some()).count(), 150);
      assert!(software_log.lock().unwrap().is_empty());
    }

    #[test]
    fn a_slow_starting_hardware_encoder_keeps_the_work() {
      // No picture for the first second (start-up latency), then one per frame.
      let (mut encoder, _) = fallback_encoder(FakeEncoder::StartsAfter(30));
      let outputs = encode_many(&mut encoder, 90);
      assert!(encoder.is_hardware());
      assert!(outputs[30..].iter().all(Option::is_some));
    }

    #[test]
    fn a_failing_hardware_encoder_hands_over_at_once() {
      let (mut encoder, software_log) = fallback_encoder(FakeEncoder::Failing);
      let outputs = encode_many(&mut encoder, 3);
      assert!(!encoder.is_hardware());
      assert!(outputs.iter().all(Option::is_some));
      assert_eq!(*software_log.lock().unwrap(), vec![true, false, false]);
      assert_eq!(encoder.name(), "fake software");
    }

    /// Issue #935: when software can't take over either, the error names both encoders and why
    /// each one failed.
    #[test]
    fn a_failure_without_software_names_both_encoders() {
      let layer = simulcast_layers(
        VideoResolution {
          width: 32,
          height: 32,
        },
        30,
        100_000,
        1,
      )[0];
      let mut encoder = FallbackEncoder::with_software(
        Box::new(FakeEncoder::Failing),
        &layer,
        VideoContent::Camera,
        Box::new(|_, _| Err(VideoCodecError::Library(OpenH264LibraryError::NotInstalled))),
      );
      assert_eq!(encoder.name(), "fake hardware");
      let error = encoder.encode(&frame(0), true).unwrap_err();
      assert_eq!(
        error.to_string(),
        "video codec: fake hardware failed (video codec: device lost), and OpenH264 could not \
         take over: OpenH264 is not installed; video needs Cisco's OpenH264 library"
      );
    }

    /// Issue #935: with no layer needed, the lowest one is still encoded to learn whether the
    /// encoder works; it starts with a keyframe once it is needed.
    #[test]
    fn an_unsent_picture_proves_the_encoder_and_the_layer_starts_with_a_keyframe() {
      let layers = simulcast_layers(
        VideoResolution {
          width: 320,
          height: 180,
        },
        30,
        100_000,
        2,
      );
      assert_eq!(layers.len(), 2);
      let encoded: Arc<Mutex<Vec<bool>>> = Arc::default();
      let log = Arc::clone(&encoded);
      let mut simulcast = SimulcastEncoder::new(&layers, |_| {
        Ok(Box::new(RecordingEncoder(Arc::clone(&log))) as Box<dyn VideoEncoder>)
      })
      .unwrap();
      assert_eq!(simulcast.encoder_names(), "fake software");
      simulcast.set_needed_layers(&[]);
      assert!(simulcast.encode(&frame(0)).unwrap().is_empty());
      assert!(simulcast.encode_unsent(&frame(1)).unwrap());
      assert_eq!(*encoded.lock().unwrap(), vec![false], "the lowest layer only");
      simulcast.set_needed_layers(&[0]);
      let pictures = simulcast.encode(&frame(2)).unwrap();
      assert_eq!(pictures.len(), 1);
      assert!(pictures[0].is_keyframe);
    }

    #[test]
    fn a_dropped_frame_keeps_a_keyframe_request_for_the_next() {
      let layers = simulcast_layers(
        VideoResolution {
          width: 32,
          height: 32,
        },
        30,
        100_000,
        1,
      );
      let mut simulcast = SimulcastEncoder::new(&layers, |_| {
        Ok(Box::new(FakeEncoder::StartsAfter(2)) as Box<dyn VideoEncoder>)
      })
      .unwrap();
      let mut keyframes = Vec::new();
      for index in 0..4 {
        for picture in simulcast.encode(&frame(index)).unwrap() {
          keyframes.push(picture.is_keyframe);
        }
      }
      // The start's keyframe request survives the two frames the encoder gave nothing for.
      assert_eq!(keyframes, vec![true, false]);
    }

    /// Hardware stand-ins; pictures are keyframes when forced.
    enum FakeEncoder {
      Stalled,
      Failing,
      /// A picture for every other frame (the counter is the frame number).
      EveryOther(u32),
      /// No picture for this many frames, then one per frame.
      StartsAfter(u32),
    }

    impl VideoEncoder for FakeEncoder {
      fn codec(&self) -> VideoCodec {
        VideoCodec::H264
      }

      fn name(&self) -> &'static str {
        "fake hardware"
      }

      fn encode(
        &mut self,
        _frame: &VideoFrame,
        is_keyframe_forced: bool,
      ) -> Result<Option<EncodedPicture>, VideoCodecError> {
        let picture = Some(EncodedPicture {
          is_keyframe: is_keyframe_forced,
          data: vec![0, 0, 0, 1, 0x65],
        });
        match self {
          Self::Stalled => Ok(None),
          Self::Failing => Err(VideoCodecError::Codec("device lost".into())),
          Self::EveryOther(count) => {
            *count += 1;
            Ok(if *count % 2 == 0 { picture } else { None })
          },
          Self::StartsAfter(remaining) => {
            if *remaining == 0 {
              return Ok(picture);
            }
            *remaining -= 1;
            Ok(None)
          },
        }
      }
    }

    /// A fallback encoder over `hardware` whose software side records its keyframe flags.
    fn fallback_encoder(hardware: FakeEncoder) -> (FallbackEncoder, Arc<Mutex<Vec<bool>>>) {
      let log: Arc<Mutex<Vec<bool>>> = Arc::default();
      let software_log = Arc::clone(&log);
      let layer = simulcast_layers(
        VideoResolution {
          width: 32,
          height: 32,
        },
        30,
        100_000,
        1,
      )[0];
      let encoder = FallbackEncoder::with_software(
        Box::new(hardware),
        &layer,
        VideoContent::Camera,
        Box::new(move |_, _| {
          Ok(Box::new(RecordingEncoder(Arc::clone(&software_log))) as Box<dyn VideoEncoder>)
        }),
      );
      (encoder, log)
    }

    /// Software stand-in: a picture per frame, recording whether a keyframe was forced.
    struct RecordingEncoder(Arc<Mutex<Vec<bool>>>);

    impl VideoEncoder for RecordingEncoder {
      fn codec(&self) -> VideoCodec {
        VideoCodec::H264
      }

      fn name(&self) -> &'static str {
        "fake software"
      }

      fn encode(
        &mut self,
        _frame: &VideoFrame,
        is_keyframe_forced: bool,
      ) -> Result<Option<EncodedPicture>, VideoCodecError> {
        self.0.lock().unwrap().push(is_keyframe_forced);
        Ok(Some(EncodedPicture {
          is_keyframe: is_keyframe_forced,
          data: vec![0, 0, 0, 1, 0x65],
        }))
      }
    }

    fn encode_many(encoder: &mut FallbackEncoder, count: u64) -> Vec<Option<EncodedPicture>> {
      (0..count)
        .map(|index| encoder.encode(&frame(index), index == 0).unwrap())
        .collect()
    }

    fn frame(index: u64) -> VideoFrame {
      let resolution = VideoResolution {
        width: 32,
        height: 32,
      };
      VideoFrame::new(resolution, index * FRAME_INTERVAL_US, vec![128; 32 * 32 * 3 / 2]).unwrap()
    }
  }

  fn encode_frames(simulcast: &mut SimulcastEncoder, index: &mut u32, count: u32) -> Vec<EncodedVideoFrame> {
    let mut frames = Vec::new();
    for _ in 0..count {
      frames.extend(simulcast.encode(&test_pattern(HD, *index)).unwrap());
      *index += 1;
    }
    frames
  }

  /// Encodes `frame` and decodes every layer's picture with that layer's decoder.
  fn decode_layers(
    simulcast: &mut SimulcastEncoder,
    decoders: &mut [OpenH264Decoder],
    frame: &VideoFrame,
  ) -> Vec<(VideoFrame, u8)> {
    let mut decoded = Vec::new();
    for picture in simulcast.encode(frame).unwrap() {
      let layer = picture.layer.value();
      if let Some(picture) = decoders[usize::from(layer)]
        .decode(&picture.data, frame.timestamp_us())
        .unwrap()
      {
        decoded.push((picture, layer));
      }
    }
    decoded
  }

  fn sizes_of(layers: &[VideoLayerInfo]) -> Vec<VideoResolution> {
    layers
      .iter()
      .map(|layer| VideoResolution {
        width: layer.width,
        height: layer.height,
      })
      .collect()
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

  /// Lines of small print on white: pseudo-random 5×7 glyphs with 1 pixel strokes and gaps
  /// between words; a cursor blinks.
  fn text_page(resolution: VideoResolution, has_cursor: bool) -> VideoFrame {
    let (width, height) = (usize::from(resolution.width), usize::from(resolution.height));
    let mut luma = vec![235u8; width * height];
    let mut random = 0x9E37_79B9_7F4A_7C15u64;
    for top in (24..height - 24).step_by(18) {
      for left in (40..width - 40).step_by(6) {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        if random.is_multiple_of(5) {
          continue;
        }
        for row in 0..7 {
          for column in 0..5 {
            if column == 0 || (random >> (row * 5 + column)) & 1 == 1 {
              luma[(top + row) * width + left + column] = 30;
            }
          }
        }
      }
    }
    if has_cursor {
      for row in 10..20 {
        luma[row * width + 40] = 16;
      }
    }
    let mut data = luma;
    data.extend(std::iter::repeat_n(128, width * height / 2));
    VideoFrame::new(resolution, 0, data).unwrap()
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
