//! Hardware H.264 encoding on Android through the NDK's MediaCodec (no FFmpeg needed).
//!
//! Byte-buffer input in NV12 (`COLOR_FormatYUV420SemiPlanar`, which hardware AVC encoders
//! accept), Constrained Baseline like OpenH264, Annex B output. MediaCodec hands out the stream's
//! SPS/PPS once as "codec config"; they are kept and put in front of every keyframe so receivers
//! can join at any keyframe. Untested on devices so far: Android calls are audio-only.

use std::time::Duration;

use ndk::media::media_codec::{
  DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection,
};
use ndk::media::media_format::MediaFormat;
use teamcraft_proto::{VideoCodec, VideoLayerInfo};

use crate::video::{VideoFrame, VideoResolution};
use crate::video_codec::{
  EncodedPicture, VideoCodecError, VideoContent, VideoEncoder, nv12_bytes, starts_with_sps,
};

const MIME_AVC: &str = "video/avc";
/// `MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420SemiPlanar` (NV12).
const COLOR_FORMAT_NV12: i32 = 21;
/// `MediaCodecInfo.CodecProfileLevel.AVCProfileConstrainedBaseline`.
const PROFILE_CONSTRAINED_BASELINE: i32 = 0x10000;
/// `MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR`.
const BITRATE_MODE_CBR: i32 = 2;
/// Seconds between keyframes, like OpenH264's 300 frames at 30 fps.
const KEYFRAME_INTERVAL_SECONDS: i32 = 10;
const BUFFER_FLAG_KEY_FRAME: u32 = 1;
const BUFFER_FLAG_CODEC_CONFIG: u32 = 2;
/// How long one frame may wait for an input buffer or its output; a real-time encoder that is
/// slower than this drops the frame instead of building up latency.
const INPUT_TIMEOUT: Duration = Duration::from_millis(10);
const OUTPUT_TIMEOUT: Duration = Duration::from_millis(20);

/// A hardware H.264 encoder for one layer.
pub struct MediaCodecEncoder {
  codec: MediaCodec,
  resolution: VideoResolution,
  stride: usize,
  slice_height: usize,
  codec_config: Vec<u8>,
}

impl MediaCodecEncoder {
  /// `None` when the device has no AVC encoder or it refuses the configuration (then the caller
  /// falls back to software).
  pub fn new(layer: &VideoLayerInfo, content: VideoContent) -> Option<Self> {
    let codec = MediaCodec::from_encoder_type(MIME_AVC)?;
    let mut format = MediaFormat::new();
    format.set_str("mime", MIME_AVC);
    format.set_i32("width", i32::from(layer.width));
    format.set_i32("height", i32::from(layer.height));
    format.set_i32("color-format", COLOR_FORMAT_NV12);
    format.set_i32(
      "bitrate",
      i32::try_from(layer.max_bitrate_bps).unwrap_or(i32::MAX),
    );
    format.set_i32("bitrate-mode", BITRATE_MODE_CBR);
    format.set_i32("frame-rate", i32::from(layer.max_framerate.max(1)));
    format.set_i32("i-frame-interval", KEYFRAME_INTERVAL_SECONDS);
    format.set_i32("profile", PROFILE_CONSTRAINED_BASELINE);
    // Real-time priority; screen content tolerates a lower frame rate rather than blur.
    format.set_i32("priority", 0);
    if content == VideoContent::Screen {
      format.set_i32("max-fps-to-encoder", i32::from(layer.max_framerate.max(1)));
    }
    codec
      .configure(&format, None, MediaCodecDirection::Encoder)
      .ok()?;
    codec.start().ok()?;
    // Byte-buffer input is tightly packed: the encoder's input format (stride, slice height) is
    // only readable from API 28, and the app supports 26.
    let stride = usize::from(layer.width);
    let slice_height = usize::from(layer.height);
    Some(Self {
      codec,
      resolution: VideoResolution {
        width: layer.width,
        height: layer.height,
      },
      stride,
      slice_height,
      codec_config: Vec::new(),
    })
  }

  fn queue(&self, frame: &VideoFrame) -> Result<bool, VideoCodecError> {
    let DequeuedInputBufferResult::Buffer(mut buffer) = self
      .codec
      .dequeue_input_buffer(INPUT_TIMEOUT)
      .map_err(media_error)?
    else {
      return Ok(false);
    };
    let nv12 = nv12_bytes(frame, self.stride, self.slice_height);
    let target = buffer
      .buffer_mut()
      .get_mut(..nv12.len())
      .ok_or_else(|| VideoCodecError::Codec("the encoder's input buffer is too small".into()))?;
    target.write_copy_of_slice(&nv12);
    self
      .codec
      .queue_input_buffer(buffer, 0, nv12.len(), frame.timestamp_us(), 0)
      .map_err(media_error)?;
    Ok(true)
  }

  fn next_picture(&mut self) -> Result<Option<EncodedPicture>, VideoCodecError> {
    loop {
      match self
        .codec
        .dequeue_output_buffer(OUTPUT_TIMEOUT)
        .map_err(media_error)?
      {
        DequeuedOutputBufferInfoResult::Buffer(buffer) => {
          let info = *buffer.info();
          let start = usize::try_from(info.offset()).unwrap_or(0);
          let end = start + usize::try_from(info.size()).unwrap_or(0);
          let data = buffer.buffer().get(start..end).unwrap_or_default().to_vec();
          self
            .codec
            .release_output_buffer(buffer, false)
            .map_err(media_error)?;
          if info.flags() & BUFFER_FLAG_CODEC_CONFIG != 0 {
            self.codec_config = data;
            continue;
          }
          let is_keyframe = info.flags() & BUFFER_FLAG_KEY_FRAME != 0;
          let data = if is_keyframe && !starts_with_sps(&data) {
            [self.codec_config.as_slice(), &data].concat()
          } else {
            data
          };
          return Ok(Some(EncodedPicture { is_keyframe, data }));
        },
        DequeuedOutputBufferInfoResult::OutputFormatChanged
        | DequeuedOutputBufferInfoResult::OutputBuffersChanged => {},
        DequeuedOutputBufferInfoResult::TryAgainLater => return Ok(None),
      }
    }
  }
}

impl VideoEncoder for MediaCodecEncoder {
  fn codec(&self) -> VideoCodec {
    VideoCodec::H264
  }

  fn name(&self) -> &'static str {
    "MediaCodec"
  }

  fn encode(
    &mut self,
    frame: &VideoFrame,
    is_keyframe_forced: bool,
  ) -> Result<Option<EncodedPicture>, VideoCodecError> {
    let frame = frame.scaled(self.resolution)?;
    if is_keyframe_forced {
      let mut request = MediaFormat::new();
      request.set_i32("request-sync", 0);
      self.codec.set_parameters(request).map_err(media_error)?;
    }
    if !self.queue(&frame)? {
      // No input buffer free: the encoder is behind; drop this frame.
      return Ok(None);
    }
    self.next_picture()
  }
}

impl Drop for MediaCodecEncoder {
  fn drop(&mut self) {
    // Nothing to do about a failing stop while tearing down; the codec is released either way.
    let _ = self.codec.stop();
  }
}

fn media_error(error: ndk::media_error::MediaError) -> VideoCodecError {
  VideoCodecError::Codec(format!("MediaCodec: {error:?}"))
}
