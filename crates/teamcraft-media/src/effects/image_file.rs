//! The user's own background pictures: decoding JPEG and PNG files, and normalizing them before
//! upload (scaled down to what a call can show, re-encoded as JPEG). Re-encoding also drops the
//! file's metadata, such as the camera's EXIF data with the place the photo was taken.

use jpeg_encoder::{ColorType, Encoder};
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_png::PngDecoder;

use super::{BackgroundImage, EffectsError};

/// Largest picture accepted for decoding (width and height); larger ones are rejected before
/// their pixels are allocated.
pub const MAX_DECODED_DIMENSION: u32 = 8192;
/// Largest pixel count accepted (36 MP, about 144 MB as RGBA): a narrow limit per side still
/// allows 8192 x 8192.
pub const MAX_DECODED_PIXELS: u64 = 36_000_000;
/// Largest picture file accepted at all, wherever it comes from (the server's limit too).
pub const MAX_BACKGROUND_FILE_BYTES: usize = 8 * 1024 * 1024;
/// Uploaded backgrounds are scaled to fit this (Full HD; calls send at most 720p).
pub const BACKGROUND_MAX_WIDTH: u32 = 1920;
pub const BACKGROUND_MAX_HEIGHT: u32 = 1080;
const BACKGROUND_JPEG_QUALITY: u8 = 85;

impl BackgroundImage {
  /// Decodes a JPEG or PNG file (detected by its signature) to RGBA.
  pub fn decode(file: &[u8]) -> Result<Self, EffectsError> {
    if file.len() > MAX_BACKGROUND_FILE_BYTES {
      return Err(EffectsError::UnsupportedImage(format!(
        "larger than {MAX_BACKGROUND_FILE_BYTES} bytes"
      )));
    }
    let options = DecoderOptions::default()
      .set_max_width(MAX_DECODED_DIMENSION as usize)
      .set_max_height(MAX_DECODED_DIMENSION as usize)
      .jpeg_set_out_colorspace(ColorSpace::RGBA)
      .png_set_add_alpha_channel(true)
      .png_set_strip_to_8bit(true);
    let (pixels, width, height) = if file.starts_with(&[0xff, 0xd8, 0xff]) {
      let mut decoder = JpegDecoder::new_with_options(ZCursor::new(file), options);
      decoder.decode_headers().map_err(file_error)?;
      check_pixels(decoder.dimensions())?;
      let pixels = decoder.decode().map_err(file_error)?;
      let (width, height) = decoder.dimensions().ok_or_else(|| file_error("no dimensions"))?;
      (pixels, width, height)
    } else if file.starts_with(b"\x89PNG\r\n\x1a\n") {
      let mut decoder = PngDecoder::new_with_options(ZCursor::new(file), options);
      decoder.decode_headers().map_err(file_error)?;
      check_pixels(decoder.dimensions())?;
      let pixels = decoder.decode_raw().map_err(file_error)?;
      let (width, height) = decoder.dimensions().ok_or_else(|| file_error("no dimensions"))?;
      let colorspace = decoder
        .colorspace()
        .ok_or_else(|| file_error("no colour space"))?;
      (to_rgba(pixels, colorspace)?, width, height)
    } else {
      return Err(EffectsError::UnsupportedImage(
        "only JPEG and PNG pictures".into(),
      ));
    };
    let size = |value: usize| u32::try_from(value).map_err(|_| file_error("too large"));
    Self::new(size(width)?, size(height)?, pixels)
  }

  /// Scaled down (keeping the aspect ratio) to fit `max_width` x `max_height`; unchanged if it
  /// fits already.
  pub fn fit_within(self, max_width: u32, max_height: u32) -> Self {
    let (width, height) = fitted_size(self.width, self.height, max_width, max_height);
    if (width, height) == (self.width, self.height) {
      return self;
    }
    let rgba = downscale(&self, width, height);
    Self { width, height, rgba }
  }

  /// The image as a baseline JPEG (alpha dropped).
  pub fn to_jpeg(&self, quality: u8) -> Result<Vec<u8>, EffectsError> {
    let too_large = |_| EffectsError::UnsupportedImage("larger than JPEG allows".into());
    let width = u16::try_from(self.width).map_err(too_large)?;
    let height = u16::try_from(self.height).map_err(too_large)?;
    let mut jpeg = Vec::new();
    Encoder::new(&mut jpeg, quality)
      .encode(&self.rgba, width, height, ColorType::Rgba)
      .map_err(|error| EffectsError::UnsupportedImage(error.to_string()))?;
    Ok(jpeg)
  }
}

/// The size of a `width` x `height` picture scaled down (keeping the aspect ratio) to fit
/// `max_width` x `max_height`; its own size if it fits already.
pub fn fitted_size(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
  if width <= max_width && height <= max_height {
    return (width, height);
  }
  let scale = (f64::from(max_width) / f64::from(width)).min(f64::from(max_height) / f64::from(height));
  // Both factors are below 1 here, so the results fit in u32 and are at least 1.
  let scaled = |side: u32| ((f64::from(side) * scale).round() as u32).max(1);
  (scaled(width), scaled(height))
}

/// A user's picture as it is stored and synced: decoded (JPEG or PNG), scaled to fit Full HD and
/// re-encoded as JPEG without metadata.
pub fn normalize_background_file(file: &[u8]) -> Result<Vec<u8>, EffectsError> {
  BackgroundImage::decode(file)?
    .fit_within(BACKGROUND_MAX_WIDTH, BACKGROUND_MAX_HEIGHT)
    .to_jpeg(BACKGROUND_JPEG_QUALITY)
}

/// Refuses pictures with more than [`MAX_DECODED_PIXELS`] before their pixels are allocated.
fn check_pixels(maybe_dimensions: Option<(usize, usize)>) -> Result<(), EffectsError> {
  let (width, height) = maybe_dimensions.ok_or_else(|| file_error("no dimensions"))?;
  if (width as u64).saturating_mul(height as u64) > MAX_DECODED_PIXELS {
    return Err(EffectsError::UnsupportedImage(format!(
      "{width} x {height} is more than {MAX_DECODED_PIXELS} pixels"
    )));
  }
  Ok(())
}

/// Area-averaging downscale: each target pixel is the mean of the source pixels it covers.
fn downscale(image: &BackgroundImage, width: u32, height: u32) -> Vec<u8> {
  let (source_width, source_height) = (image.width as usize, image.height as usize);
  let (width, height) = (width as usize, height as usize);
  let mut rgba = Vec::with_capacity(width * height * 4);
  for y in 0..height {
    let top = y * source_height / height;
    let bottom = ((y + 1) * source_height / height).max(top + 1);
    for x in 0..width {
      let left = x * source_width / width;
      let right = ((x + 1) * source_width / width).max(left + 1);
      let mut sum = [0u32; 4];
      for row in top..bottom {
        for column in left..right {
          let pixel = &image.rgba[(row * source_width + column) * 4..][..4];
          for channel in 0..4 {
            sum[channel] += u32::from(pixel[channel]);
          }
        }
      }
      let count = ((bottom - top) * (right - left)) as u32;
      // A mean of bytes is a byte.
      rgba.extend(sum.map(|value| (value / count) as u8));
    }
  }
  rgba
}

/// PNG output in any of its colour spaces as RGBA.
fn to_rgba(pixels: Vec<u8>, colorspace: ColorSpace) -> Result<Vec<u8>, EffectsError> {
  let rgba = match colorspace {
    ColorSpace::RGBA => pixels,
    ColorSpace::RGB => pixels
      .as_chunks::<3>()
      .0
      .iter()
      .flat_map(|p| [p[0], p[1], p[2], 255])
      .collect(),
    ColorSpace::LumaA => pixels
      .as_chunks::<2>()
      .0
      .iter()
      .flat_map(|p| [p[0], p[0], p[0], p[1]])
      .collect(),
    ColorSpace::Luma => pixels.iter().flat_map(|&v| [v, v, v, 255]).collect(),
    other => {
      return Err(EffectsError::UnsupportedImage(format!(
        "PNG colour space {other:?}"
      )));
    },
  };
  Ok(rgba)
}

fn file_error(error: impl std::fmt::Debug) -> EffectsError {
  EffectsError::UnsupportedImage(format!("{error:?}"))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn jpegs_round_trip_and_large_pictures_shrink_to_full_hd() {
    let image = BackgroundImage::new(3840, 2160, vec![120; 3840 * 2160 * 4]).unwrap();
    let jpeg = image.to_jpeg(80).unwrap();
    let normalized = normalize_background_file(&jpeg).unwrap();
    let decoded = BackgroundImage::decode(&normalized).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (1920, 1080));
    assert!(
      decoded
        .rgba()
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| pixel[0].abs_diff(120) <= 3)
    );
  }

  #[test]
  fn pictures_keep_their_aspect_ratio_when_scaled() {
    let portrait = BackgroundImage::new(1000, 4000, vec![0; 1000 * 4000 * 4]).unwrap();
    let fitted = portrait.fit_within(BACKGROUND_MAX_WIDTH, BACKGROUND_MAX_HEIGHT);
    assert_eq!((fitted.width(), fitted.height()), (270, 1080));
    let small = BackgroundImage::new(640, 360, vec![0; 640 * 360 * 4]).unwrap();
    assert_eq!(small.fit_within(1920, 1080).width(), 640);
  }

  #[test]
  fn pngs_decode() {
    // A 1x1 red PNG.
    let png = [
      0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00,
      0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xde, 0x00,
      0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01,
      0x01, 0x00, 0x18, 0xdd, 0x8d, 0xb0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60,
      0x82,
    ];
    let image = BackgroundImage::decode(&png).unwrap();
    assert_eq!((image.width(), image.height()), (1, 1));
    assert_eq!(image.rgba(), &[255, 0, 0, 255]);
  }

  #[test]
  fn other_and_broken_files_are_rejected() {
    assert!(matches!(
      BackgroundImage::decode(b"GIF89a"),
      Err(EffectsError::UnsupportedImage(_))
    ));
    assert!(BackgroundImage::decode(&[0xff, 0xd8, 0xff, 0xe0, 0, 1]).is_err());
    assert!(BackgroundImage::decode(b"\x89PNG\r\n\x1a\ntruncated").is_err());
  }

  #[test]
  fn too_many_pixels_or_bytes_are_rejected() {
    // 8000 x 5000 = 40 MP: within 8192 per side, over the pixel limit.
    let large = BackgroundImage::new(8000, 5000, vec![0; 8000 * 5000 * 4]).unwrap();
    let jpeg = large.to_jpeg(10).unwrap();
    assert!(matches!(
      BackgroundImage::decode(&jpeg),
      Err(EffectsError::UnsupportedImage(message)) if message.contains("pixels")
    ));
    let mut huge_file = vec![0xff, 0xd8, 0xff];
    huge_file.resize(MAX_BACKGROUND_FILE_BYTES + 1, 0);
    assert!(matches!(
      normalize_background_file(&huge_file),
      Err(EffectsError::UnsupportedImage(message)) if message.contains("bytes")
    ));
  }

  #[test]
  fn oversized_pictures_are_rejected_before_decoding() {
    let wide = BackgroundImage::new(8193, 1, vec![0; 8193 * 4]).unwrap();
    // JPEG can't even hold it beyond 65535; 8193 fits JPEG but not the decoder's limit.
    let jpeg = wide.to_jpeg(50).unwrap();
    assert!(BackgroundImage::decode(&jpeg).is_err());
  }
}
