//! Pixel formats cameras and screen captures deliver, converted to I420 for the encoder.
//!
//! Cameras mostly give YUYV (4:2:2) or MJPEG, sometimes NV12; screen captures give 32-bit RGB
//! (BGRx on little-endian PipeWire). RGB uses BT.601 limited range, like the encoders expect.
//! Odd widths or heights lose their last column or row (I420 needs even dimensions).

use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
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

/// A tightly packed RGBx picture (a canvas' `getImageData`) to I420, like [`RawPicture::to_i420`]
/// without the other formats: the browser's background effects need only this one (#439), and
/// leave the MJPEG decoder out of the call module that way.
pub fn rgbx_to_i420(
  data: &[u8],
  width: u32,
  height: u32,
  timestamp_us: u64,
) -> Result<VideoFrame, CaptureError> {
  let resolution = even_resolution(width, height)?;
  let (even_width, even_height) = (usize::from(resolution.width), usize::from(resolution.height));
  let stride = width as usize * 4;
  let needed = stride * (even_height - 1) + even_width * 4;
  if data.len() < needed {
    return Err(CaptureError::Device(format!(
      "RGBx picture of {width}x{height} needs {needed} bytes, got {}",
      data.len()
    )));
  }
  // Two rows at a time, two pixels across: each 2x2 block gives four luma samples and one chroma
  // pair. Slices of whole rows keep the bounds checks out of the inner loop (the call module is
  // built for size, which leaves rgb32's per-pixel indexing about five times slower).
  let mut out = vec![0; even_width * even_height * 3 / 2];
  let (luma, chroma) = out.split_at_mut(even_width * even_height);
  let (u_plane, v_plane) = chroma.split_at_mut(even_width * even_height / 4);
  let chroma_width = even_width / 2;
  let rows = luma
    .chunks_exact_mut(even_width * 2)
    .zip(u_plane.chunks_exact_mut(chroma_width))
    .zip(v_plane.chunks_exact_mut(chroma_width))
    .enumerate();
  for (pair, ((luma_rows, u_row), v_row)) in rows {
    let top = &data[pair * 2 * stride..][..even_width * 4];
    let bottom = &data[(pair * 2 + 1) * stride..][..even_width * 4];
    let (luma_top, luma_bottom) = luma_rows.split_at_mut(even_width);
    let blocks = top
      .as_chunks::<8>()
      .0
      .iter()
      .zip(bottom.as_chunks::<8>().0)
      .zip(
        luma_top
          .as_chunks_mut::<2>()
          .0
          .iter_mut()
          .zip(luma_bottom.as_chunks_mut::<2>().0),
      )
      .zip(u_row.iter_mut().zip(v_row.iter_mut()));
    for (((top, bottom), (luma_top, luma_bottom)), (u, v)) in blocks {
      let rgb = |pixel: &[u8]| [i32::from(pixel[0]), i32::from(pixel[1]), i32::from(pixel[2])];
      let pixels = [
        rgb(&top[..4]),
        rgb(&top[4..]),
        rgb(&bottom[..4]),
        rgb(&bottom[4..]),
      ];
      let luma_of = |[r, g, b]: [i32; 3]| (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
      luma_top[0] = luma_of(pixels[0]);
      luma_top[1] = luma_of(pixels[1]);
      luma_bottom[0] = luma_of(pixels[2]);
      luma_bottom[1] = luma_of(pixels[3]);
      let sum = |channel: usize| (pixels.iter().map(|pixel| pixel[channel]).sum::<i32>() + 2) / 4;
      let (r, g, b) = (sum(0), sum(1), sum(2));
      *u = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
      *v = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
    }
  }
  Ok(VideoFrame::new(resolution, timestamp_us, out)?)
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

/// Packed 4:2:2 → I420. Whole rows through fixed-size chunks, so the compiler vectorizes it: macOS
/// cameras deliver this, at 4K too (#537).
fn yuyv(data: &[u8], stride: usize, width: usize, height: usize, out: &mut Vec<u8>) {
  let start = out.len();
  let (luma, chroma) = (width * height, width / 2 * (height / 2));
  out.resize(start + luma + 2 * chroma, 0);
  let (y, uv) = out[start..].split_at_mut(luma);
  let (u, v) = uv.split_at_mut(chroma);
  let row_of = |row: usize| &data[row * stride..][..width * 2];
  for (row, y_row) in y.chunks_exact_mut(width).enumerate() {
    for (value, pair) in y_row.iter_mut().zip(row_of(row).as_chunks::<2>().0) {
      *value = pair[0];
    }
  }
  // 4:2:2 → 4:2:0: average the chroma of two rows.
  let average = |top: u8, bottom: u8| (u16::from(top) + u16::from(bottom)).div_ceil(2) as u8;
  for ((row, u_row), v_row) in (0..height)
    .step_by(2)
    .zip(u.chunks_exact_mut(width / 2))
    .zip(v.chunks_exact_mut(width / 2))
  {
    let pairs = row_of(row)
      .as_chunks::<4>()
      .0
      .iter()
      .zip(row_of(row + 1).as_chunks::<4>().0);
    for ((u, v), (top, bottom)) in u_row.iter_mut().zip(v_row.iter_mut()).zip(pairs) {
      *u = average(top[1], bottom[1]);
      *v = average(top[3], bottom[3]);
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
  let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
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
  fn rgbx_alone_converts_like_the_general_path() {
    for (width, height) in [(6, 4), (7, 5), (16, 2)] {
      let data: Vec<u8> = (0..width * height * 4)
        .map(|value| (value * 7 % 256) as u8)
        .collect();
      let general = RawPicture {
        format: PixelFormat::Rgbx,
        width,
        height,
        stride: 0,
        data: &data,
      }
      .to_i420(5)
      .unwrap();
      assert_eq!(
        rgbx_to_i420(&data, width, height, 5).unwrap(),
        general,
        "{width}x{height}"
      );
    }
    let data: Vec<u8> = (0..6 * 4 * 4).map(|value| (value * 7 % 256) as u8).collect();
    assert!(matches!(
      rgbx_to_i420(&data[..20], 6, 4, 5),
      Err(CaptureError::Device(_))
    ));
    assert!(matches!(
      rgbx_to_i420(&data, 1, 1, 5),
      Err(CaptureError::UnsupportedFormat(_))
    ));
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
