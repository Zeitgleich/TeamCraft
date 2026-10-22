//! Remote video as JPEG images, for webviews without WebCodecs (WebKitGTK): the received H.264
//! is decoded natively and every few frames one is re-encoded as a self-contained JPEG.

use std::time::Duration;

use jpeg_encoder::{Encoder, ImageBuffer, JpegColorType, SamplingFactor};

use crate::video::VideoFrame;
use crate::video_codec::{VideoCodecError, VideoDecoder};

/// JPEG quality: video tiles, not photos.
const DEFAULT_QUALITY: u8 = 70;
/// About 15 images per second: every other frame of a 30 fps source, with room for timestamp jitter.
const DEFAULT_MIN_INTERVAL: Duration = Duration::from_millis(60);

/// Decodes one stream and emits JPEG images at a reduced rate.
pub struct JpegTranscoder {
  decoder: Box<dyn VideoDecoder>,
  quality: u8,
  min_interval_us: u64,
  maybe_last_us: Option<u64>,
}

impl JpegTranscoder {
  pub fn new(decoder: Box<dyn VideoDecoder>) -> Self {
    Self {
      decoder,
      quality: DEFAULT_QUALITY,
      min_interval_us: u64::try_from(DEFAULT_MIN_INTERVAL.as_micros()).unwrap_or(u64::MAX),
      maybe_last_us: None,
    }
  }

  /// Decodes `data` (every frame must be decoded: later frames refer to it); returns a JPEG when
  /// this frame is due.
  pub fn push(&mut self, data: &[u8], timestamp_us: u64) -> Result<Option<Vec<u8>>, VideoCodecError> {
    let Some(frame) = self.decoder.decode(data, timestamp_us)? else {
      return Ok(None);
    };
    let is_due = self
      .maybe_last_us
      .is_none_or(|last| timestamp_us < last || timestamp_us - last >= self.min_interval_us);
    if !is_due {
      return Ok(None);
    }
    self.maybe_last_us = Some(timestamp_us);
    encode_jpeg(&frame, self.quality).map(Some)
  }
}

/// One I420 frame as a baseline JPEG (4:2:0, like the source).
pub fn encode_jpeg(frame: &VideoFrame, quality: u8) -> Result<Vec<u8>, VideoCodecError> {
  let mut jpeg = Vec::new();
  let mut encoder = Encoder::new(&mut jpeg, quality);
  encoder.set_sampling_factor(SamplingFactor::F_2_2);
  encoder
    .encode_image(I420Image(frame))
    .map_err(|error| VideoCodecError::Codec(error.to_string()))?;
  Ok(jpeg)
}

/// Feeds the encoder rows of an I420 frame as YCbCr; chroma samples cover two pixels and rows.
struct I420Image<'a>(&'a VideoFrame);

impl ImageBuffer for I420Image<'_> {
  fn get_jpeg_color_type(&self) -> JpegColorType {
    JpegColorType::Ycbcr
  }

  fn width(&self) -> u16 {
    self.0.resolution().width
  }

  fn height(&self) -> u16 {
    self.0.resolution().height
  }

  fn fill_buffers(&self, y: u16, buffers: &mut [Vec<u8>; 4]) {
    let width = usize::from(self.width());
    let chroma_width = width.div_ceil(2);
    let row = usize::from(y);
    let (luma, cb, cr) = self.0.planes();
    let luma_row = &luma[row * width..(row + 1) * width];
    let chroma_start = (row / 2) * chroma_width;
    let cb_row = &cb[chroma_start..chroma_start + chroma_width];
    let cr_row = &cr[chroma_start..chroma_start + chroma_width];
    for (x, value) in luma_row.iter().enumerate() {
      buffers[0].push(*value);
      buffers[1].push(cb_row[x / 2]);
      buffers[2].push(cr_row[x / 2]);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::video::VideoResolution;

  #[test]
  fn frames_become_decodable_jpegs() {
    let frame = gray_frame(64, 48, 0);
    let jpeg = encode_jpeg(&frame, 70).unwrap();
    assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
    let mut decoder = zune_jpeg::JpegDecoder::new(jpeg.as_slice());
    let pixels = decoder.decode().unwrap();
    let (width, height) = decoder.dimensions().unwrap();
    assert_eq!((width, height), (64, 48));
    // Mid gray in, mid gray out (within JPEG loss).
    assert!(pixels.iter().all(|value| value.abs_diff(128) <= 3));
  }

  #[test]
  fn images_are_rate_limited_but_every_frame_is_decoded() {
    let mut transcoder = JpegTranscoder::new(Box::new(PassThrough));
    let mut images = 0;
    for index in 0..30u64 {
      if transcoder.push(&[], index * 33_333).unwrap().is_some() {
        images += 1;
      }
    }
    assert_eq!(images, 15);
  }

  /// A "decoder" that returns a gray frame for every call.
  struct PassThrough;

  impl VideoDecoder for PassThrough {
    fn decode(&mut self, _data: &[u8], timestamp_us: u64) -> Result<Option<VideoFrame>, VideoCodecError> {
      Ok(Some(gray_frame(16, 16, timestamp_us)))
    }
  }

  fn gray_frame(width: u16, height: u16, timestamp_us: u64) -> VideoFrame {
    let size = usize::from(width) * usize::from(height);
    VideoFrame::new(
      VideoResolution { width, height },
      timestamp_us,
      vec![128; size * 3 / 2],
    )
    .unwrap()
  }
}
