//! Thumbnails of image attachments. Made by the sender's client and sent encrypted inside the
//! message, so receivers show a preview before (or without) downloading the file.

use std::io::Cursor;

use image::{ImageFormat, ImageReader, Limits};

/// Longest side of a thumbnail in pixels.
pub const THUMBNAIL_SIZE: u32 = 320;
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
}
