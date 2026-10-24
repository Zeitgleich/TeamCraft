//! Thumbnails of image attachments. Made by the sender's client and sent encrypted inside the
//! message, so receivers show a preview before (or without) downloading the file.

use std::io::Cursor;

use image::{ImageFormat, ImageReader, Limits};

/// Longest side of a thumbnail in pixels.
pub const THUMBNAIL_SIZE: u32 = 320;
/// Side of a space picture in pixels (the rail shows it at up to 48 CSS pixels, ×4 for dense screens
/// and the settings preview).
pub const SPACE_IMAGE_SIZE: u32 = 256;
/// Images larger than this many pixels aren't decoded (decompression bombs).
const MAX_PIXELS: u64 = 40_000_000;
/// Memory the decoder may allocate.
const MAX_DECODER_BYTES: u64 = 256 * 1024 * 1024;

/// A PNG thumbnail of an image file, or `None` for anything that isn't a decodable image.
pub fn thumbnail_png(bytes: &[u8], media_type: &str) -> Option<Vec<u8>> {
  if !media_type.starts_with("image/") {
    return None;
  }
  let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
  let mut limits = Limits::default();
  limits.max_alloc = Some(MAX_DECODER_BYTES);
  reader.limits(limits);
  let (width, height) = reader.into_dimensions().ok()?;
  if u64::from(width) * u64::from(height) > MAX_PIXELS {
    return None;
  }
  let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
  let mut limits = Limits::default();
  limits.max_alloc = Some(MAX_DECODER_BYTES);
  reader.limits(limits);
  let image = reader.decode().ok()?;
  let small = image.thumbnail(THUMBNAIL_SIZE, THUMBNAIL_SIZE);
  let mut out = Cursor::new(Vec::new());
  small.write_to(&mut out, ImageFormat::Png).ok()?;
  Some(out.into_inner())
}

/// A space picture from a picked image file: the centered square, scaled to at most
/// [`SPACE_IMAGE_SIZE`] pixels per side, as PNG. `None` for anything that isn't a decodable image.
/// The server re-encodes it again (it trusts no client); this keeps uploads small.
pub fn space_image_png(bytes: &[u8]) -> Option<Vec<u8>> {
  let image = decode_bounded(bytes)?;
  let side = image.width().min(image.height());
  let square = image.crop_imm(
    (image.width() - side) / 2,
    (image.height() - side) / 2,
    side,
    side,
  );
  let small = if side > SPACE_IMAGE_SIZE {
    square.resize_exact(
      SPACE_IMAGE_SIZE,
      SPACE_IMAGE_SIZE,
      image::imageops::FilterType::Lanczos3,
    )
  } else {
    square
  };
  let mut out = Cursor::new(Vec::new());
  small.write_to(&mut out, ImageFormat::Png).ok()?;
  Some(out.into_inner())
}

/// Decodes an image within [`MAX_PIXELS`] and [`MAX_DECODER_BYTES`].
fn decode_bounded(bytes: &[u8]) -> Option<image::DynamicImage> {
  let reader = || {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODER_BYTES);
    reader.limits(limits);
    Some(reader)
  };
  let (width, height) = reader()?.into_dimensions().ok()?;
  if u64::from(width) * u64::from(height) > MAX_PIXELS || width == 0 || height == 0 {
    return None;
  }
  reader()?.decode().ok()
}

#[cfg(test)]
mod tests {
  use image::{ImageBuffer, Rgb};

  use super::*;

  #[test]
  fn images_shrink_to_a_png_and_other_files_have_none() {
    let picture = ImageBuffer::from_fn(1000, 500, |x, _| Rgb([(x % 256) as u8, 40, 80]));
    let mut jpeg = Cursor::new(Vec::new());
    picture.write_to(&mut jpeg, ImageFormat::Jpeg).unwrap();
    let thumbnail = thumbnail_png(jpeg.get_ref(), "image/jpeg").unwrap();
    let decoded = image::load_from_memory(&thumbnail).unwrap();
    assert_eq!(
      (decoded.width(), decoded.height()),
      (THUMBNAIL_SIZE, THUMBNAIL_SIZE / 2)
    );

    assert_eq!(thumbnail_png(b"%PDF-1.7", "application/pdf"), None);
    assert_eq!(thumbnail_png(b"not really a png", "image/png"), None);
  }

  #[test]
  fn space_images_are_the_centered_square_at_most_256_pixels_wide() {
    // Left half red, right half blue: the centered square straddles both.
    let picture = ImageBuffer::from_fn(1000, 500, |x, _| {
      if x < 500 {
        Rgb([255u8, 0, 0])
      } else {
        Rgb([0, 0, 255])
      }
    });
    let mut png = Cursor::new(Vec::new());
    picture.write_to(&mut png, ImageFormat::Png).unwrap();
    let square = image::load_from_memory(&space_image_png(png.get_ref()).unwrap())
      .unwrap()
      .to_rgb8();
    assert_eq!(square.dimensions(), (SPACE_IMAGE_SIZE, SPACE_IMAGE_SIZE));
    assert_eq!(square.get_pixel(10, 10).0, [255, 0, 0]);
    assert_eq!(square.get_pixel(SPACE_IMAGE_SIZE - 10, 10).0, [0, 0, 255]);

    // Small pictures are not scaled up.
    let tiny = ImageBuffer::from_fn(40, 30, |_, _| Rgb([1u8, 2, 3]));
    let mut png = Cursor::new(Vec::new());
    tiny.write_to(&mut png, ImageFormat::Png).unwrap();
    let decoded = image::load_from_memory(&space_image_png(png.get_ref()).unwrap()).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (30, 30));

    assert_eq!(space_image_png(b"<svg></svg>"), None);
    assert_eq!(space_image_png(b""), None);
  }
}
