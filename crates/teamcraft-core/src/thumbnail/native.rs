//! The `image` crate's side of [`super::PictureCodec`]: what desktop, mobile and the integration
//! bridge make thumbnails and screenshots with. The browser has its own (teamcraft-ui
//! `picture_codec.rs`) and never links this.

use std::io::Cursor;

use image::codecs::bmp::BmpDecoder;
use image::codecs::gif::GifDecoder;
use image::codecs::ico::IcoDecoder;
use image::codecs::jpeg::{JpegDecoder, JpegEncoder};
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngDecoder, PngEncoder};
use image::codecs::webp::WebPDecoder;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};

use super::{
  CodecFuture, JPEG_STEPS, MAX_PIXELS, MAX_THUMBNAIL_BYTES, PNG_STEPS, PictureCodec, SCREENSHOT_JPEG_QUALITY,
  essence,
};

/// Memory the decoder may allocate.
const MAX_DECODER_BYTES: u64 = 256 * 1024 * 1024;

/// The [`PictureCodec`] of the native platforms: [`thumbnail`] and [`without_metadata`], done
/// right away.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeCodec;

impl PictureCodec for NativeCodec {
  fn thumbnail<'a>(&'a self, bytes: &'a [u8], media_type: &'a str) -> CodecFuture<'a, Option<Vec<u8>>> {
    Box::pin(std::future::ready(thumbnail(bytes, media_type)))
  }

  fn without_metadata<'a>(&'a self, bytes: &'a [u8]) -> CodecFuture<'a, Option<(Vec<u8>, &'static str)>> {
    Box::pin(std::future::ready(without_metadata(bytes)))
  }
}

/// A thumbnail of an image file, as [`PictureCodec::thumbnail`] describes it.
pub fn thumbnail(bytes: &[u8], media_type: &str) -> Option<Vec<u8>> {
  thumbnail_within(bytes, media_type, MAX_PIXELS)
}

/// [`thumbnail`] of a picture with at most `max_pixels` (and never more than the usual limit): a
/// host that makes thumbnails of untrusted pictures unattended, like the integration bridge, keeps
/// the memory a decode takes lower.
pub fn thumbnail_within(bytes: &[u8], media_type: &str, max_pixels: u64) -> Option<Vec<u8>> {
  let media_type = essence(media_type);
  if !media_type.starts_with("image/") {
    return None;
  }
  if media_type == "image/svg+xml" {
    return svg_thumbnail(bytes);
  }
  let image = decode_upright(bytes, max_pixels.min(MAX_PIXELS))?;
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

/// SVG is drawn, not decoded: `resvg` (the browser draws its own, `teamcraft-ui`).
/// A PNG (an SVG may be transparent) at the largest of [`PNG_STEPS`] that fits.
fn svg_thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
  PNG_STEPS
    .iter()
    .filter_map(|&side| crate::svg_thumbnail::svg_thumbnail_png(bytes, side))
    .find(|png| png.len() <= MAX_THUMBNAIL_BYTES)
}

/// Decodes within the limits and turns the picture as its EXIF orientation says: phone cameras
/// store portrait photos sideways, and a thumbnail without the turn shows them lying down.
fn decode_upright(bytes: &[u8], max_pixels: u64) -> Option<DynamicImage> {
  let mut decoder = bounded_reader(bytes, max_pixels)?.into_decoder().ok()?;
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

/// Looks at the alpha channel in place: a converted copy would take another 4 bytes a pixel.
fn has_transparency(image: &DynamicImage) -> bool {
  match image {
    DynamicImage::ImageRgba8(buffer) => buffer.pixels().any(|pixel| pixel.0[3] < u8::MAX),
    DynamicImage::ImageLumaA8(buffer) => buffer.pixels().any(|pixel| pixel.0[1] < u8::MAX),
    DynamicImage::ImageRgba16(buffer) => buffer.pixels().any(|pixel| pixel.0[3] < u16::MAX),
    DynamicImage::ImageLumaA16(buffer) => buffer.pixels().any(|pixel| pixel.0[1] < u16::MAX),
    DynamicImage::ImageRgba32F(buffer) => buffer.pixels().any(|pixel| pixel.0[3] < 1.0),
    // Any other layout with alpha (a newer `image`) is taken as transparent: a PNG is always right.
    other => other.color().has_alpha(),
  }
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

/// A picture encoded again from its pixels without its metadata, as
/// [`PictureCodec::without_metadata`] describes it.
pub fn without_metadata(bytes: &[u8]) -> Option<(Vec<u8>, &'static str)> {
  let is_jpeg = image::guess_format(bytes).ok()? == ImageFormat::Jpeg;
  let image = decode_upright(bytes, MAX_PIXELS)?;
  let mut out = Vec::new();
  if is_jpeg {
    image
      .to_rgb8()
      .write_with_encoder(JpegEncoder::new_with_quality(&mut out, SCREENSHOT_JPEG_QUALITY))
      .ok()?;
    Some((out, "image/jpeg"))
  } else {
    let encoder = PngEncoder::new_with_quality(&mut out, CompressionType::Default, PngFilter::Adaptive);
    image.write_with_encoder(encoder).ok()?;
    Some((out, "image/png"))
  }
}

/// [`super::image_dimensions`] of a picture's bytes.
pub(super) fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
  // The format's decoder called directly, as `ImageReader::into_decoder` makes it (the same
  // decoders, limits and orientation): that one is a `dyn ImageDecoder`, which would link every
  // decoder's whole decoding into each page of the browser, where only the thumbnails need it.
  let format = ImageReader::new(Cursor::new(bytes))
    .with_guessed_format()
    .ok()?
    .format()?;
  let reader = Cursor::new(bytes);
  match format {
    ImageFormat::Png => upright_size(PngDecoder::with_limits(reader, Limits::default()).ok()?),
    ImageFormat::Jpeg => upright_size(JpegDecoder::new(reader).ok()?),
    ImageFormat::Gif => upright_size(GifDecoder::new(reader).ok()?),
    ImageFormat::WebP => upright_size(WebPDecoder::new(reader).ok()?),
    ImageFormat::Bmp => upright_size(BmpDecoder::new(reader).ok()?),
    ImageFormat::Ico => upright_size(IcoDecoder::new(reader).ok()?),
    _ => None,
  }
}

/// Width and height from a decoder's header, turned upright; `None` for an empty picture.
fn upright_size(mut decoder: impl ImageDecoder) -> Option<(u32, u32)> {
  decoder.set_limits(Limits::default()).ok()?;
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

/// A reader for an image whose header is within `max_pixels`, with the decoder's memory capped.
fn bounded_reader(bytes: &[u8], max_pixels: u64) -> Option<ImageReader<Cursor<&[u8]>>> {
  let reader = || {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODER_BYTES);
    reader.limits(limits);
    Some(reader)
  };
  let (width, height) = reader()?.into_dimensions().ok()?;
  if u64::from(width) * u64::from(height) > max_pixels || width == 0 || height == 0 {
    return None;
  }
  reader()
}

#[cfg(test)]
mod tests {
  use image::{ImageBuffer, Rgb, Rgba};

  use super::*;
  use crate::thumbnail::{THUMBNAIL_SIZE, image_dimensions, make_thumbnail, make_without_metadata};

  #[test]
  fn metadata_is_dropped_from_a_screenshot() {
    let picture = ImageBuffer::from_fn(64, 32, |x, _| Rgb([(x * 4) as u8, 40, 80]));
    let jpeg = encoded(&picture, ImageFormat::Jpeg);
    // An APP1 (EXIF) segment right after the start marker, as cameras write it.
    let exif = b"Exif\0\0GPS 52.5200N 13.4050E";
    let length = u16::try_from(exif.len() + 2).unwrap().to_be_bytes();
    let mut tagged = jpeg[..2].to_vec();
    tagged.extend_from_slice(&[0xFF, 0xE1, length[0], length[1]]);
    tagged.extend_from_slice(exif);
    tagged.extend_from_slice(&jpeg[2..]);
    assert!(tagged.windows(4).any(|window| window == b"52.5"));

    let (clean, media_type) = without_metadata(&tagged).unwrap();
    assert_eq!(media_type, "image/jpeg");
    assert!(!clean.windows(4).any(|window| window == b"52.5"));
    assert!(!clean.windows(4).any(|window| window == b"Exif"));
    assert_eq!(dimensions(&clean), (64, 32));

    let (png, media_type) = without_metadata(&encoded(&picture, ImageFormat::Png)).unwrap();
    assert_eq!((media_type, dimensions(&png)), ("image/png", (64, 32)));
    assert_eq!(without_metadata(b"%PDF-1.7"), None);
  }

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
  fn pictures_over_the_pixel_limit_are_not_decoded() {
    // A small file whose header claims 20000 x 20000 pixels: refused from the header alone.
    let bomb = with_png_size(&encoded(&halves(4, 4), ImageFormat::Png), 20_000, 20_000);
    assert_eq!(dimensions_from_header(&bomb), (20_000, 20_000));
    assert!(thumbnail(&bomb, "image/png").is_none());
    // A host may set a lower limit.
    let picture = encoded(&halves(100, 100), ImageFormat::Png);
    assert!(thumbnail_within(&picture, "image/png", 9_999).is_none());
    assert!(thumbnail_within(&picture, "image/png", 10_000).is_some());
    assert!(
      thumbnail_within(&picture, "image/png", u64::MAX).is_some(),
      "never above the usual"
    );
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
  fn the_native_codec_is_the_image_crate_right_here() {
    use futures_util::FutureExt;
    let picture = ImageBuffer::from_fn(1000, 500, |x, _| Rgb([(x % 256) as u8, 40, 80]));
    let jpeg = encoded(&picture, ImageFormat::Jpeg);
    let codec: &dyn PictureCodec = &NativeCodec;
    assert_eq!(
      codec.thumbnail(&jpeg, "image/jpeg").now_or_never(),
      Some(thumbnail(&jpeg, "image/jpeg"))
    );
    assert_eq!(
      codec.without_metadata(&jpeg).now_or_never(),
      Some(without_metadata(&jpeg))
    );
    assert_eq!(
      make_without_metadata(&jpeg).now_or_never(),
      Some(without_metadata(&jpeg))
    );
    assert_eq!(make_without_metadata(b"%PDF-1.7").now_or_never(), Some(None));
  }

  #[test]
  fn natively_the_engine_makes_thumbnails_right_here() {
    use futures_util::FutureExt;
    let picture = ImageBuffer::from_fn(1000, 500, |x, _| Rgb([(x % 256) as u8, 40, 80]));
    let jpeg = encoded(&picture, ImageFormat::Jpeg);
    let made = make_thumbnail(&jpeg, "image/jpeg").now_or_never();
    assert_eq!(made, Some(thumbnail(&jpeg, "image/jpeg")));
    assert!(made.flatten().is_some());
    assert_eq!(
      make_thumbnail(b"%PDF-1.7", "application/pdf").now_or_never(),
      Some(None)
    );
  }

  /// The browser scales to [`fitted_size`]; it is the size `image` gives a thumbnail.
  #[test]
  fn the_fitted_size_is_the_one_image_makes() {
    for (width, height) in [
      (1000, 500),
      (641, 3),
      (3, 641),
      (1999, 1001),
      (640, 640),
      (12, 7),
      (20_000, 1),
    ] {
      let picture = DynamicImage::new_rgb8(width, height);
      let made = fitted(&picture, THUMBNAIL_SIZE);
      assert_eq!(
        crate::thumbnail::fitted_size(width, height, THUMBNAIL_SIZE),
        (made.width(), made.height()),
        "{width} × {height}"
      );
    }
  }

  /// The header reader the browser uses (`image_header.rs`) reads what `image` reads, for every
  /// format, orientation and junk; for a truncated file it may give up earlier, never read another
  /// size.
  #[test]
  fn the_browsers_header_reader_reads_what_image_reads() {
    let opaque = ImageBuffer::from_fn(30, 20, |x, y| Rgb([x as u8, y as u8, 7]));
    let translucent = ImageBuffer::from_fn(30, 20, |x, _| Rgba([x as u8, 9, 9, 128]));
    let big = ImageBuffer::from_fn(256, 3, |x, _| Rgba([x as u8, 9, 9, 255]));
    let mut samples = vec![
      encoded(&translucent, ImageFormat::Png),
      encoded(&opaque, ImageFormat::Png),
      encoded(&opaque, ImageFormat::Jpeg),
      encoded(&translucent, ImageFormat::Gif),
      encoded(&translucent, ImageFormat::WebP),
      encoded(&opaque, ImageFormat::WebP),
      encoded(&opaque, ImageFormat::Bmp),
      encoded(&translucent, ImageFormat::Bmp),
      encoded(&translucent, ImageFormat::Ico),
      encoded(&big, ImageFormat::Ico),
      with_png_size(&encoded(&halves(4, 4), ImageFormat::Png), 20_000, 20_000),
    ];
    for orientation in 1..=8 {
      samples.push(with_exif_orientation(
        &encoded(&opaque, ImageFormat::Jpeg),
        orientation,
      ));
      samples.push(with_png_exif_orientation(
        &encoded(&opaque, ImageFormat::Png),
        orientation,
      ));
    }
    samples.push(b"not really a png".to_vec());
    samples.push(Vec::new());
    for (index, bytes) in samples.iter().enumerate() {
      let expected = generic_dimensions(bytes);
      assert_eq!(
        crate::image_header::upright_size(bytes),
        expected,
        "sample {index}"
      );
      if !bytes.is_empty() {
        assert!(
          expected.is_some() || index >= samples.len() - 2,
          "sample {index} is a picture"
        );
      }
      for end in 0..bytes.len() {
        let prefix = &bytes[..end];
        if let Some(read) = crate::image_header::upright_size(prefix) {
          assert_eq!(Some(read), expected, "sample {index} cut at {end}");
        }
      }
    }
    // Sideways orientations turn the size.
    let sideways = with_png_exif_orientation(&encoded(&opaque, ImageFormat::Png), 6);
    assert_eq!(crate::image_header::upright_size(&sideways), Some((20, 30)));
  }

  /// The decoders called directly read the same sizes as `ImageReader::into_decoder` (what this
  /// function used before the web app split its codecs off), for every format and for junk.
  #[test]
  fn dimensions_match_the_generic_decoder_for_every_format() {
    let opaque = ImageBuffer::from_fn(30, 20, |x, y| Rgb([x as u8, y as u8, 7]));
    let translucent = ImageBuffer::from_fn(30, 20, |x, _| Rgba([x as u8, 9, 9, 128]));
    let png = encoded(&translucent, ImageFormat::Png);
    let samples = [
      (png.clone(), "image/png"),
      (encoded(&opaque, ImageFormat::Jpeg), "image/jpeg"),
      (
        with_exif_orientation(&encoded(&opaque, ImageFormat::Jpeg), 6),
        "image/jpeg",
      ),
      (encoded(&translucent, ImageFormat::Gif), "image/gif"),
      (encoded(&translucent, ImageFormat::WebP), "image/webp"),
      (encoded(&opaque, ImageFormat::Bmp), "image/bmp"),
      (encoded(&translucent, ImageFormat::Ico), "image/x-icon"),
      (png[..png.len() / 2].to_vec(), "image/png"),
      (png[..24].to_vec(), "image/png"),
      (b"not really a png".to_vec(), "image/png"),
      (Vec::new(), "image/png"),
    ];
    for (bytes, media_type) in samples {
      let generic = generic_dimensions(&bytes);
      assert_eq!(
        image_dimensions(&bytes, media_type),
        generic,
        "{media_type}, {} bytes",
        bytes.len()
      );
    }
    assert_eq!(generic_dimensions(&png), Some((30, 20)));
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

  /// The PNG with its header's size replaced (and the header's checksum fixed), not its pixels.
  fn with_png_size(png: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut patched = png.to_vec();
    patched[16..20].copy_from_slice(&width.to_be_bytes());
    patched[20..24].copy_from_slice(&height.to_be_bytes());
    let crc = crc32(&patched[12..29]);
    patched[29..33].copy_from_slice(&crc.to_be_bytes());
    patched
  }

  fn dimensions_from_header(png: &[u8]) -> (u32, u32) {
    ImageReader::new(Cursor::new(png))
      .with_guessed_format()
      .unwrap()
      .into_dimensions()
      .unwrap()
  }

  /// CRC-32 (ISO-HDLC), as PNG chunks use it.
  fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
      crc ^= u32::from(*byte);
      for _ in 0..8 {
        crc = if crc & 1 == 1 {
          (crc >> 1) ^ 0xedb8_8320
        } else {
          crc >> 1
        };
      }
    }
    !crc
  }

  /// [`image_dimensions`] through `ImageReader::into_decoder`, as it read them before.
  fn generic_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut decoder = ImageReader::new(Cursor::new(bytes))
      .with_guessed_format()
      .ok()?
      .into_decoder()
      .ok()?;
    let (width, height) = decoder.dimensions();
    let is_sideways = matches!(
      decoder.orientation().ok()?,
      Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH
    );
    let (width, height) = if is_sideways {
      (height, width)
    } else {
      (width, height)
    };
    (width > 0 && height > 0).then_some((width, height))
  }

  fn dimensions(bytes: &[u8]) -> (u32, u32) {
    let decoded = image::load_from_memory(bytes).unwrap();
    (decoded.width(), decoded.height())
  }

  /// The PNG with an `eXIf` chunk holding only the orientation tag, right after its header.
  fn with_png_exif_orientation(png: &[u8], orientation: u16) -> Vec<u8> {
    let tiff = orientation_tiff(orientation);
    let mut chunk = (tiff.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(b"eXIf");
    chunk.extend_from_slice(&tiff);
    let crc = crc32(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    let mut out = png[..33].to_vec();
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[33..]);
    out
  }

  /// A TIFF header and one directory with only the orientation tag (big-endian, unlike the JPEG's).
  fn orientation_tiff(orientation: u16) -> Vec<u8> {
    let mut tiff = b"MM\0*\0\0\0\x08".to_vec();
    tiff.extend_from_slice(&1u16.to_be_bytes());
    tiff.extend_from_slice(&0x0112u16.to_be_bytes());
    tiff.extend_from_slice(&3u16.to_be_bytes());
    tiff.extend_from_slice(&1u32.to_be_bytes());
    tiff.extend_from_slice(&orientation.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0u32.to_be_bytes());
    tiff
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
