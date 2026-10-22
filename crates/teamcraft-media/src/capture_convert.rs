//! Pixel formats cameras and screen captures deliver, converted to I420 for the encoder.
//!
//! Cameras mostly give YUYV (4:2:2) or MJPEG, sometimes NV12; screen captures give 32-bit RGB
//! (BGRx on little-endian PipeWire). RGB uses BT.601 limited range, like the encoders expect.
//! Odd widths or heights lose their last column or row (I420 needs even dimensions).

use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::video::{VideoFrame, VideoResolution};
use crate::video_capture::CaptureError;

/// A captured picture's memory layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
  I420,
  Nv12,
  /// Packed 4:2:2, Y0 U Y1 V.
  Yuyv,
  /// 32 bits per pixel, bytes B, G, R, unused.
  Bgrx,
  /// 32 bits per pixel, bytes R, G, B, unused.
  Rgbx,
  Mjpeg,
}

/// One captured picture as the device hands it over.
#[derive(Debug, Clone, Copy)]
pub struct RawPicture<'a> {
  pub format: PixelFormat,
  pub width: u32,
  pub height: u32,
  /// Bytes per row of the first plane (packed formats: of the only plane); 0 means tightly packed.
  pub stride: usize,
  pub data: &'a [u8],
}

impl RawPicture<'_> {
  /// Converts to an I420 frame stamped with `timestamp_us`.
  pub fn to_i420(&self, timestamp_us: u64) -> Result<VideoFrame, CaptureError> {
    if self.format == PixelFormat::Mjpeg {
      return decode_mjpeg(self.data, timestamp_us);
    }
    let resolution = even_resolution(self.width, self.height)?;
    let (width, height) = (usize::from(resolution.width), usize::from(resolution.height));
    let stride = match self.stride {
      0 => self.width as usize * bytes_per_pixel(self.format),
      stride => stride,
    };
    let needed = match self.format {
      PixelFormat::I420 | PixelFormat::Nv12 => stride * self.height as usize * 3 / 2,
      _ => stride * (height - 1) + width * bytes_per_pixel(self.format),
    };
    if self.data.len() < needed {
      return Err(CaptureError::Device(format!(
        "{:?} picture of {}x{} needs {needed} bytes, got {}",
        self.format,
        self.width,
        self.height,
        self.data.len()
      )));
    }
    let mut out = Vec::with_capacity(width * height * 3 / 2);
    match self.format {
      PixelFormat::I420 => i420(self.data, stride, self.height as usize, width, height, &mut out),
      PixelFormat::Nv12 => nv12(self.data, stride, self.height as usize, width, height, &mut out),
      PixelFormat::Yuyv => yuyv(self.data, stride, width, height, &mut out),
      PixelFormat::Bgrx => rgb32(self.data, stride, width, height, [2, 1, 0], &mut out),
      PixelFormat::Rgbx => rgb32(self.data, stride, width, height, [0, 1, 2], &mut out),
      PixelFormat::Mjpeg => unreachable!("handled above"),
    }
    Ok(VideoFrame::new(resolution, timestamp_us, out)?)
  }
}

fn bytes_per_pixel(format: PixelFormat) -> usize {
  match format {
    PixelFormat::I420 | PixelFormat::Nv12 | PixelFormat::Mjpeg => 1,
    PixelFormat::Yuyv => 2,
    PixelFormat::Bgrx | PixelFormat::Rgbx => 4,
  }
}

fn even_resolution(width: u32, height: u32) -> Result<VideoResolution, CaptureError> {
  let even = |value: u32| u16::try_from(value & !1).ok().filter(|value| *value > 0);
  match (even(width), even(height)) {
    (Some(width), Some(height)) => Ok(VideoResolution { width, height }),
    _ => Err(CaptureError::UnsupportedFormat(format!("{width}x{height}"))),
  }
}

fn i420(data: &[u8], stride: usize, rows: usize, width: usize, height: usize, out: &mut Vec<u8>) {
  for row in 0..height {
    out.extend_from_slice(&data[row * stride..][..width]);
  }
  let chroma_stride = stride / 2;
  let u_start = stride * rows;
  let v_start = u_start + chroma_stride * rows / 2;
  for start in [u_start, v_start] {
    for row in 0..height / 2 {
      out.extend_from_slice(&data[start + row * chroma_stride..][..width / 2]);
    }
  }
}

fn nv12(data: &[u8], stride: usize, rows: usize, width: usize, height: usize, out: &mut Vec<u8>) {
  for row in 0..height {
    out.extend_from_slice(&data[row * stride..][..width]);
  }
  let chroma = &data[stride * rows..];
  for offset in [0, 1] {
    for row in 0..height / 2 {
      let line = &chroma[row * stride..][..width];
      out.extend(line.iter().skip(offset).step_by(2));
    }
  }
}

fn yuyv(data: &[u8], stride: usize, width: usize, height: usize, out: &mut Vec<u8>) {
  for row in 0..height {
    out.extend(data[row * stride..][..width * 2].iter().step_by(2));
  }
  // 4:2:2 → 4:2:0: average the chroma of two rows.
  for offset in [1, 3] {
    for row in (0..height).step_by(2) {
      let (top, bottom) = (&data[row * stride..], &data[(row + 1) * stride..]);
      for pair in 0..width / 2 {
        let index = pair * 4 + offset;
        out.push((u16::from(top[index]) + u16::from(bottom[index])).div_ceil(2) as u8);
      }
    }
  }
}

/// 32-bit RGB → I420; `order` gives the byte index of R, G and B.
fn rgb32(data: &[u8], stride: usize, width: usize, height: usize, order: [usize; 3], out: &mut Vec<u8>) {
  let pixel = |x: usize, y: usize| {
    let at = &data[y * stride + x * 4..];
    (
      i32::from(at[order[0]]),
      i32::from(at[order[1]]),
      i32::from(at[order[2]]),
    )
  };
  for y in 0..height {
    for x in 0..width {
      let (r, g, b) = pixel(x, y);
      out.push((((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8);
    }
  }
  let mut v_plane = Vec::with_capacity(width * height / 4);
  for y in (0..height).step_by(2) {
    for x in (0..width).step_by(2) {
      let mut sum = (0, 0, 0);
      for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let (r, g, b) = pixel(x + dx, y + dy);
        sum = (sum.0 + r, sum.1 + g, sum.2 + b);
      }
      let (r, g, b) = ((sum.0 + 2) / 4, (sum.1 + 2) / 4, (sum.2 + 2) / 4);
      out.push((((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8);
      v_plane.push((((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8);
    }
  }
  out.extend(v_plane);
}

fn decode_mjpeg(data: &[u8], timestamp_us: u64) -> Result<VideoFrame, CaptureError> {
  let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
  let mut decoder = JpegDecoder::new_with_options(data, options);
  let pixels = decoder
    .decode()
    .map_err(|error| CaptureError::Device(format!("MJPEG: {error:?}")))?;
  let (width, height) = decoder
    .dimensions()
    .ok_or_else(|| CaptureError::Device("MJPEG without dimensions".into()))?;
  let too_large = |_| CaptureError::UnsupportedFormat(format!("{width}x{height} JPEG"));
  RawPicture {
    format: PixelFormat::Rgbx,
    width: u32::try_from(width).map_err(too_large)?,
    height: u32::try_from(height).map_err(too_large)?,
    stride: width * 4,
    data: &pixels,
  }
  .to_i420(timestamp_us)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn yuyv_keeps_luma_and_averages_chroma() {
    // 2x2 pixels: rows Y0 U Y1 V.
    let data = [10, 100, 20, 200, 30, 110, 40, 210];
    let frame = RawPicture {
      format: PixelFormat::Yuyv,
      width: 2,
      height: 2,
      stride: 0,
      data: &data,
    }
    .to_i420(5)
    .unwrap();
    assert_eq!(frame.data(), &[10, 20, 30, 40, 105, 205]);
    assert_eq!(frame.timestamp_us(), 5);
  }

  #[test]
  fn nv12_with_stride_deinterleaves_chroma() {
    // 2x2 picture, rows padded to 4 bytes.
    let data = [1, 2, 0, 0, 3, 4, 0, 0, 50, 60, 0, 0];
    let frame = RawPicture {
      format: PixelFormat::Nv12,
      width: 2,
      height: 2,
      stride: 4,
      data: &data,
    }
    .to_i420(0)
    .unwrap();
    assert_eq!(frame.data(), &[1, 2, 3, 4, 50, 60]);
  }

  #[test]
  fn bgrx_white_and_black_map_to_limited_range() {
    let white = [255u8; 16];
    let frame = RawPicture {
      format: PixelFormat::Bgrx,
      width: 2,
      height: 2,
      stride: 0,
      data: &white,
    }
    .to_i420(0)
    .unwrap();
    assert_eq!(frame.data(), &[235, 235, 235, 235, 128, 128]);
    // Pure red (B, G, R, x).
    let red: Vec<u8> = [0, 0, 255, 0].repeat(4);
    let frame = RawPicture {
      format: PixelFormat::Bgrx,
      width: 2,
      height: 2,
      stride: 0,
      data: &red,
    }
    .to_i420(0)
    .unwrap();
    assert_eq!(frame.data(), &[82, 82, 82, 82, 90, 240]);
  }

  #[test]
  fn odd_sizes_are_cropped_and_short_buffers_rejected() {
    let data = vec![0u8; 3 * 3 * 4];
    let frame = RawPicture {
      format: PixelFormat::Rgbx,
      width: 3,
      height: 3,
      stride: 0,
      data: &data,
    }
    .to_i420(0)
    .unwrap();
    assert_eq!(frame.resolution(), VideoResolution { width: 2, height: 2 });
    let short = RawPicture {
      format: PixelFormat::Yuyv,
      width: 4,
      height: 4,
      stride: 0,
      data: &[0; 10],
    };
    assert!(matches!(short.to_i420(0), Err(CaptureError::Device(_))));
    let empty = RawPicture {
      format: PixelFormat::Bgrx,
      width: 1,
      height: 1,
      stride: 0,
      data: &[0; 4],
    };
    assert!(matches!(
      empty.to_i420(0),
      Err(CaptureError::UnsupportedFormat(_))
    ));
  }

  #[test]
  fn garbage_mjpeg_is_an_error() {
    let picture = RawPicture {
      format: PixelFormat::Mjpeg,
      width: 640,
      height: 480,
      stride: 0,
      data: &[0xff, 0xd8, 1, 2, 3],
    };
    assert!(picture.to_i420(0).is_err());
  }
}
