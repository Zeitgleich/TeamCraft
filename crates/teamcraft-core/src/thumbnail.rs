//! Thumbnails of image attachments. Made by the sender's client and sent encrypted inside the
//! message, so receivers show a preview before (or without) downloading the file.
//!
//! A thumbnail is a JPEG, or a PNG when the picture has transparency. Receivers read the format
//! from the bytes (older clients made PNGs only), so either works everywhere.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};

/// Longest side of a thumbnail in pixels: sharp at the ~300 dp a phone shows it at on a 2–3×
/// screen, and at 360 CSS pixels on a HiDPI desktop.
pub const THUMBNAIL_SIZE: u32 = 640;
/// Side of a space picture in pixels (the rail shows it at up to 48 CSS pixels, ×4 for dense screens
/// and the settings preview).
pub const SPACE_IMAGE_SIZE: u32 = 256;
/// Side of an avatar in pixels (shown at up to 64 CSS pixels in profiles, ×4 for dense screens and
/// the profile card).
pub const AVATAR_IMAGE_SIZE: u32 = 512;
/// Longest side of the crop dialog's copy of a picture: plenty for a 512 px result even when zoomed
/// in, and a 40 MP photo still fits in a few MB of memory.
pub const EDITOR_IMAGE_MAX_SIDE: u32 = 2048;
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
  if !media_type.starts_with("image/") {
    return None;
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

/// The editor's copy of a picked picture (crop dialog, issue #323): upright (EXIF orientation
/// applied), scaled to at most [`EDITOR_IMAGE_MAX_SIDE`] pixels per side, as a quickly encoded PNG.
/// The dialog shows it and crops from it, so a huge photo is decoded once and never held in full.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorImage {
  pub png: Vec<u8>,
  pub width: u32,
  pub height: u32,
}

/// The square a user chose in the crop dialog, in pixels of the [`EditorImage`] after turning it
/// clockwise by `quarter_turns` × 90°.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SquareCrop {
  pub quarter_turns: u8,
  pub x: u32,
  pub y: u32,
  pub side: u32,
}

/// The editor's copy of a picked image file; `None` for anything that isn't a decodable image.
pub fn editor_image(bytes: &[u8]) -> Option<EditorImage> {
  let image = decode_upright(bytes)?;
  let image = if image.width().max(image.height()) > EDITOR_IMAGE_MAX_SIDE {
    image.resize(
      EDITOR_IMAGE_MAX_SIDE,
      EDITOR_IMAGE_MAX_SIDE,
      image::imageops::FilterType::Triangle,
    )
  } else {
    image
  };
  let mut out = Vec::new();
  // Fast compression: the PNG only travels to a `blob:` URL and back into `cropped_square_png`.
  let encoder = PngEncoder::new_with_quality(&mut out, CompressionType::Fast, PngFilter::Adaptive);
  image.write_with_encoder(encoder).ok()?;
  Some(EditorImage {
    png: out,
    width: image.width(),
    height: image.height(),
  })
}

/// Cuts `crop` out of an editor image (see [`editor_image`]) and scales it to at most `size`
/// pixels per side, as PNG; a smaller square is not scaled up. The rectangle is clamped to the
/// picture. The server re-encodes the upload again (it trusts no client); this keeps it small.
pub fn cropped_square_png(editor_png: &[u8], crop: SquareCrop, size: u32) -> Option<Vec<u8>> {
  let image = decode_bounded(editor_png)?;
  let image = match crop.quarter_turns % 4 {
    1 => image.rotate90(),
    2 => image.rotate180(),
    3 => image.rotate270(),
    _ => image,
  };
  let side = crop.side.min(image.width()).min(image.height()).max(1);
  let x = crop.x.min(image.width() - side);
  let y = crop.y.min(image.height() - side);
  let square = image.crop_imm(x, y, side, side);
  let small = if side > size {
    square.resize_exact(size, size, image::imageops::FilterType::Lanczos3)
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
  if !media_type.starts_with("image/") {
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
  fn the_editor_copy_is_bounded_and_unreadable_files_have_none() {
    let big = encode(ImageBuffer::from_fn(4096, 1024, |x, _| {
      Rgb([(x % 256) as u8, 1, 2])
    }));
    let editor = editor_image(&big).unwrap();
    assert_eq!((editor.width, editor.height), (EDITOR_IMAGE_MAX_SIDE, 512));
    let decoded = image::load_from_memory(&editor.png).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (editor.width, editor.height));

    let small = encode(ImageBuffer::from_fn(40, 30, |_, _| Rgb([1u8, 2, 3])));
    let editor = editor_image(&small).unwrap();
    assert_eq!((editor.width, editor.height), (40, 30));

    assert_eq!(editor_image(b"<svg></svg>"), None);
    assert_eq!(editor_image(b""), None);
  }

  #[test]
  fn the_chosen_square_is_cut_and_scaled() {
    // Left half red, right half blue.
    let picture = encode(halves(1000, 500));
    let editor = editor_image(&picture).unwrap();
    let left = crop(
      &editor.png,
      SquareCrop {
        quarter_turns: 0,
        x: 0,
        y: 0,
        side: 500,
      },
      256,
    );
    assert_eq!(left.dimensions(), (256, 256));
    assert_eq!(left.get_pixel(250, 128).0, [255, 0, 0]);
    let right = crop(
      &editor.png,
      SquareCrop {
        quarter_turns: 0,
        x: 500,
        y: 0,
        side: 500,
      },
      256,
    );
    assert_eq!(right.get_pixel(5, 128).0, [0, 0, 255]);

    // A small square is not scaled up.
    let small = crop(
      &editor.png,
      SquareCrop {
        quarter_turns: 0,
        x: 10,
        y: 10,
        side: 100,
      },
      256,
    );
    assert_eq!(small.dimensions(), (100, 100));
  }

  #[test]
  fn the_crop_follows_the_rotation_and_stays_inside() {
    // Turned clockwise, the red left half is on top: 500 wide, 1000 high.
    let editor = editor_image(&encode(halves(1000, 500))).unwrap();
    let top = crop(
      &editor.png,
      SquareCrop {
        quarter_turns: 1,
        x: 0,
        y: 0,
        side: 500,
      },
      500,
    );
    assert_eq!(top.get_pixel(250, 250).0, [255, 0, 0]);
    let bottom = crop(
      &editor.png,
      SquareCrop {
        quarter_turns: 1,
        x: 0,
        y: 500,
        side: 500,
      },
      500,
    );
    assert_eq!(bottom.get_pixel(250, 250).0, [0, 0, 255]);

    // Out-of-range rectangles are clamped, not refused.
    let clamped = crop(
      &editor.png,
      SquareCrop {
        quarter_turns: 0,
        x: 9000,
        y: 9000,
        side: 9000,
      },
      600,
    );
    assert_eq!(clamped.dimensions(), (500, 500));
    assert_eq!(clamped.get_pixel(499, 0).0, [0, 0, 255]);
    assert_eq!(
      cropped_square_png(
        b"nope",
        SquareCrop {
          quarter_turns: 0,
          x: 0,
          y: 0,
          side: 1
        },
        1
      ),
      None
    );
  }

  fn halves(width: u32, height: u32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    ImageBuffer::from_fn(width, height, |x, _| {
      if x < width / 2 {
        Rgb([255u8, 0, 0])
      } else {
        Rgb([0, 0, 255])
      }
    })
  }

  fn encode(picture: ImageBuffer<Rgb<u8>, Vec<u8>>) -> Vec<u8> {
    let mut png = Cursor::new(Vec::new());
    picture.write_to(&mut png, ImageFormat::Png).unwrap();
    png.into_inner()
  }

  fn crop(editor_png: &[u8], square: SquareCrop, size: u32) -> image::RgbImage {
    image::load_from_memory(&cropped_square_png(editor_png, square, size).unwrap())
      .unwrap()
      .to_rgb8()
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
