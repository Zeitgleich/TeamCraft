//! Thumbnails of image attachments. Made by the sender's client and sent encrypted inside the
//! message, so receivers show a preview before (or without) downloading the file.
//!
//! A thumbnail is a JPEG, or a PNG when the picture has transparency. Receivers read the format
//! from the bytes (older clients made PNGs only), so either works everywhere.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};

/// Longest side of a thumbnail in pixels: sharp at the ~300 dp a phone shows it at on a 2–3×
/// screen, and at 360 CSS pixels on a HiDPI desktop.
pub const THUMBNAIL_SIZE: u32 = 640;
/// Side of a space picture in pixels (the rail shows it at up to 48 CSS pixels, ×4 for dense screens
/// and the settings preview).
pub const SPACE_IMAGE_SIZE: u32 = 256;
/// A thumbnail travels inside the message, which the server caps (1 MiB of ciphertext by default)
/// for every file of the message together; larger ones are made smaller or lower in quality.
pub const MAX_THUMBNAIL_BYTES: usize = 96 * 1024;
/// Sizes and JPEG qualities tried in turn until a thumbnail fits [`MAX_THUMBNAIL_BYTES`].
const JPEG_STEPS: [(u32, u8); 4] = [(THUMBNAIL_SIZE, 80), (THUMBNAIL_SIZE, 65), (480, 65), (320, 60)];
/// Sizes tried in turn for pictures with transparency (PNG has no quality to lower).
const PNG_STEPS: [u32; 3] = [THUMBNAIL_SIZE, 480, 320];
/// Images larger than this many pixels aren't decoded (decompression bombs).
const MAX_PIXELS: u64 = 40_000_000;
/// Memory the decoder may allocate.
const MAX_DECODER_BYTES: u64 = 256 * 1024 * 1024;

/// A thumbnail of an image file, upright and at most [`THUMBNAIL_SIZE`] pixels on its longer side,
/// or `None` for anything that isn't a decodable image (or can't be made small enough).
pub fn thumbnail(bytes: &[u8], media_type: &str) -> Option<Vec<u8>> {
  let media_type = essence(media_type);
  if !media_type.starts_with("image/") {
    return None;
  }
  if media_type == "image/svg+xml" {
    return svg_thumbnail(bytes);
  }
  let image = decode_upright(bytes)?;
  if has_transparency(&image) {
    PNG_STEPS
      .iter()
      .filter_map(|&side| encode_png(&fitted(&image, side)))
      .find(|png| png.len() <= MAX_THUMBNAIL_BYTES)
  } else {
    // JPEG has no alpha; an opaque picture loses nothing by dropping the channel.
    JPEG_STEPS
      .iter()
      .filter_map(|&(side, quality)| encode_jpeg(&fitted(&image, side), quality))
      .find(|jpeg| jpeg.len() <= MAX_THUMBNAIL_BYTES)
  }
}

/// A media type without parameters, trimmed and lowercase: senders write whatever they like.
fn essence(media_type: &str) -> String {
  media_type
    .split(';')
    .next()
    .unwrap_or_default()
    .trim()
    .to_ascii_lowercase()
}

/// SVG is drawn, not decoded: `resvg` on native platforms; the browser makes its own (`teamcraft-ui`).
/// A PNG (an SVG may be transparent) at the largest of [`PNG_STEPS`] that fits.
#[cfg(not(target_arch = "wasm32"))]
fn svg_thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
  PNG_STEPS
    .iter()
    .filter_map(|&side| crate::svg_thumbnail::svg_thumbnail_png(bytes, side))
    .find(|png| png.len() <= MAX_THUMBNAIL_BYTES)
}

#[cfg(target_arch = "wasm32")]
fn svg_thumbnail(_bytes: &[u8]) -> Option<Vec<u8>> {
  None
}

/// Decodes within the limits and turns the picture as its EXIF orientation says: phone cameras
/// store portrait photos sideways, and a thumbnail without the turn shows them lying down.
fn decode_upright(bytes: &[u8]) -> Option<DynamicImage> {
  let mut decoder = bounded_reader(bytes)?.into_decoder().ok()?;
  let orientation = decoder.orientation().ok()?;
  let mut image = DynamicImage::from_decoder(decoder).ok()?;
  image.apply_orientation(orientation);
  Some(image)
}

/// The picture scaled down to at most `side` pixels on its longer side; smaller ones stay as they are.
fn fitted(image: &DynamicImage, side: u32) -> DynamicImage {
  if image.width().max(image.height()) <= side {
    image.clone()
  } else {
    image.thumbnail(side, side)
  }
}

fn has_transparency(image: &DynamicImage) -> bool {
  image.color().has_alpha() && image.to_rgba8().pixels().any(|pixel| pixel.0[3] < u8::MAX)
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Option<Vec<u8>> {
  let mut out = Vec::new();
  JpegEncoder::new_with_quality(&mut out, quality)
    .encode_image(&image.to_rgb8())
    .ok()?;
  Some(out)
}

fn encode_png(image: &DynamicImage) -> Option<Vec<u8>> {
  let mut out = Cursor::new(Vec::new());
  image.write_to(&mut out, ImageFormat::Png).ok()?;
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

/// Width and height of an image from its header, without decoding it; `None` for anything else.
/// Turned upright like the thumbnail: a portrait photo stored sideways reports portrait dimensions.
pub fn image_dimensions(bytes: &[u8], media_type: &str) -> Option<(u32, u32)> {
  if !essence(media_type).starts_with("image/") {
    return None;
  }
  let mut decoder = ImageReader::new(Cursor::new(bytes))
    .with_guessed_format()
    .ok()?
    .into_decoder()
    .ok()?;
  let (width, height) = decoder.dimensions();
  let is_sideways = matches!(
    decoder.orientation().ok()?,
    Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH
  );
  let (width, height) = if is_sideways {
    (height, width)
  } else {
    (width, height)
  };
  (width > 0 && height > 0).then_some((width, height))
}

/// Decodes an image within [`MAX_PIXELS`] and [`MAX_DECODER_BYTES`].
fn decode_bounded(bytes: &[u8]) -> Option<DynamicImage> {
  bounded_reader(bytes)?.decode().ok()
}

/// A reader for an image whose header is within [`MAX_PIXELS`], with the decoder's memory capped.
fn bounded_reader(bytes: &[u8]) -> Option<ImageReader<Cursor<&[u8]>>> {
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
  reader()
}

#[cfg(test)]
mod tests {
  use image::{ImageBuffer, Rgb, Rgba};

  use super::*;

  #[test]
  fn photos_shrink_to_a_jpeg_and_other_files_have_none() {
    let picture = ImageBuffer::from_fn(1000, 500, |x, _| Rgb([(x % 256) as u8, 40, 80]));
    let made = thumbnail(&encoded(&picture, ImageFormat::Jpeg), "image/jpeg").unwrap();
    assert_eq!(image::guess_format(&made).unwrap(), ImageFormat::Jpeg);
    assert_eq!(dimensions(&made), (THUMBNAIL_SIZE, THUMBNAIL_SIZE / 2));

    assert_eq!(thumbnail(b"%PDF-1.7", "application/pdf"), None);
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="1"/>"#;
    let made = thumbnail(svg, "image/svg+xml").unwrap();
    assert_eq!(
      thumbnail(svg, "Image/SVG+XML; charset=utf-8"),
      Some(made.clone()),
      "parameters and case"
    );
    assert_eq!(image::guess_format(&made).unwrap(), ImageFormat::Png);
    assert_eq!(
      dimensions(&made),
      (THUMBNAIL_SIZE, THUMBNAIL_SIZE / 2),
      "SVG is drawn at the thumbnail size"
    );
    assert_eq!(thumbnail(b"not really a png", "image/png"), None);
    assert_eq!(thumbnail(b"", "image/png"), None);
  }

  #[test]
  fn small_pictures_are_not_scaled_up() {
    let picture = ImageBuffer::from_fn(200, 100, |_, _| Rgb([1u8, 2, 3]));
    let made = thumbnail(&encoded(&picture, ImageFormat::Png), "image/png").unwrap();
    assert_eq!(dimensions(&made), (200, 100));
  }

  #[test]
  fn transparent_pictures_stay_png_with_their_alpha() {
    let picture = ImageBuffer::from_fn(800, 800, |x, _| {
      Rgba([200u8, 0, 0, if x < 400 { 0 } else { 255 }])
    });
    let made = thumbnail(&encoded(&picture, ImageFormat::Png), "image/png").unwrap();
    assert_eq!(image::guess_format(&made).unwrap(), ImageFormat::Png);
    let decoded = image::load_from_memory(&made).unwrap().to_rgba8();
    assert_eq!(decoded.dimensions(), (THUMBNAIL_SIZE, THUMBNAIL_SIZE));
    assert_eq!(decoded.get_pixel(10, 10).0[3], 0);
    assert_eq!(decoded.get_pixel(THUMBNAIL_SIZE - 10, 10).0[3], 255);
  }

  #[test]
  fn an_opaque_alpha_channel_still_makes_a_jpeg() {
    let picture = ImageBuffer::from_fn(800, 600, |x, _| Rgba([(x % 256) as u8, 0, 0, 255]));
    let made = thumbnail(&encoded(&picture, ImageFormat::Png), "image/png").unwrap();
    assert_eq!(image::guess_format(&made).unwrap(), ImageFormat::Jpeg);
  }

  #[test]
  fn noisy_photos_still_fit_the_size_limit() {
    // Noise is the worst case for compression.
    let mut state = 0x2545_f491_u32;
    let picture = ImageBuffer::from_fn(2000, 2000, |_, _| {
      state ^= state << 13;
      state ^= state >> 17;
      state ^= state << 5;
      let [r, g, b, _] = state.to_le_bytes();
      Rgb([r, g, b])
    });
    let made = thumbnail(&encoded(&picture, ImageFormat::Png), "image/png").unwrap();
    assert!(made.len() <= MAX_THUMBNAIL_BYTES, "{} bytes", made.len());
  }

  #[test]
  fn sideways_camera_photos_are_turned_upright() {
    // Stored 1000 × 500 with EXIF orientation 6 ("turn 90° clockwise to view"): a portrait photo.
    let picture = ImageBuffer::from_fn(1000, 500, |_, _| Rgb([90u8, 90, 90]));
    let jpeg = with_exif_orientation(&encoded(&picture, ImageFormat::Jpeg), 6);
    let made = thumbnail(&jpeg, "image/jpeg").unwrap();
    assert_eq!(dimensions(&made), (THUMBNAIL_SIZE / 2, THUMBNAIL_SIZE));
    assert_eq!(
      image_dimensions(&jpeg, "image/jpeg"),
      Some((500, 1000)),
      "dimensions turn too"
    );
    assert_eq!(
      image_dimensions(&encoded(&picture, ImageFormat::Jpeg), "image/jpeg"),
      Some((1000, 500))
    );
    assert_eq!(image_dimensions(b"%PDF-1.7", "application/pdf"), None);
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

  fn encoded<P, C>(picture: &ImageBuffer<P, C>, format: ImageFormat) -> Vec<u8>
  where
    P: image::Pixel + image::PixelWithColorType,
    [P::Subpixel]: image::EncodableLayout,
    C: std::ops::Deref<Target = [P::Subpixel]>,
  {
    let mut out = Cursor::new(Vec::new());
    picture.write_to(&mut out, format).unwrap();
    out.into_inner()
  }

  fn dimensions(bytes: &[u8]) -> (u32, u32) {
    let decoded = image::load_from_memory(bytes).unwrap();
    (decoded.width(), decoded.height())
  }

  /// The JPEG with an EXIF segment holding only the orientation tag, right after its start marker.
  fn with_exif_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
    let mut tiff = b"II*\0\x08\0\0\0".to_vec();
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&0x0112u16.to_le_bytes());
    tiff.extend_from_slice(&3u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&u32::from(orientation).to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());
    let mut segment = b"Exif\0\0".to_vec();
    segment.extend_from_slice(&tiff);
    let length = u16::try_from(segment.len() + 2).unwrap();
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&segment);
    out.extend_from_slice(&jpeg[2..]);
    out
  }
}
