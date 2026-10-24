//! Sizes and loading rules of image previews in the conversation (issue #316): a picture shows
//! inline within the box of the user's [`ImageSize`] (portrait screenshots get more height than
//! width), in its own aspect ratio, laid out from its thumbnail before anything downloads so the
//! list doesn't jump.

use crate::lightbox::MediaKind;
use crate::prefs::ImageSize;
/// Smallest longest side of the thumbnails senders make (`teamcraft_core::thumbnail::THUMBNAIL_SIZE`,
/// 320 px in 1.0, larger in newer apps). A thumbnail with a shorter longest side is the whole
/// picture: it isn't blown up.
pub const THUMBNAIL_SIDE_PX: u32 = 320;
/// Largest file an inline preview downloads by itself; bigger ones keep their thumbnail until opened.
pub const MAX_INLINE_LOAD_BYTES: u64 = 16 * 1024 * 1024;
/// Smallest side of an inline picture, so a thin strip stays clickable.
const MIN_SIDE_PX: u32 = 48;

/// The media type of thumbnail bytes from their magic bytes: senders made PNGs, newer ones may make
/// JPEGs, and the message doesn't say which.
pub fn thumbnail_type(bytes: &[u8]) -> &'static str {
  match bytes.starts_with(&JPEG_START) {
    true => "image/jpeg",
    false => "image/png",
  }
}

/// Width and height of a PNG or JPEG thumbnail from its header; `None` for anything else.
pub fn thumbnail_size(bytes: &[u8]) -> Option<(u32, u32)> {
  let (width, height) = match bytes.starts_with(&JPEG_START) {
    true => jpeg_size(bytes)?,
    false => png_size(bytes)?,
  };
  (width > 0 && height > 0).then_some((width, height))
}

const JPEG_START: [u8; 3] = [0xff, 0xd8, 0xff];

fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
  const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
  if bytes.get(..8)? != SIGNATURE || bytes.get(12..16)? != b"IHDR" {
    return None;
  }
  let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
  let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
  Some((width, height))
}

/// Walks the JPEG segments to the first start-of-frame marker, which holds the size.
fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
  let mut at = 2;
  loop {
    if *bytes.get(at)? != 0xff {
      return None;
    }
    let marker = *bytes.get(at + 1)?;
    // Padding between segments.
    if marker == 0xff {
      at += 1;
      continue;
    }
    let length = usize::from(u16::from_be_bytes([*bytes.get(at + 2)?, *bytes.get(at + 3)?]));
    // SOF0 to SOF15, without DHT (C4), JPG (C8) and DAC (CC).
    if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
      let height = u16::from_be_bytes([*bytes.get(at + 5)?, *bytes.get(at + 6)?]);
      let width = u16::from_be_bytes([*bytes.get(at + 7)?, *bytes.get(at + 8)?]);
      return Some((u32::from(width), u32::from(height)));
    }
    if length < 2 {
      return None;
    }
    at += 2 + length;
  }
}

/// The box an inline picture fits into, width and height in CSS pixels.
pub fn inline_box(size: ImageSize) -> (u32, u32) {
  match size {
    ImageSize::Standard => (240, 320),
    ImageSize::Large => (360, 480),
  }
}

/// The inline size of a picture whose thumbnail is `width` × `height`: scaled to fit the box,
/// unless the thumbnail already is the whole (small) picture, which is never blown up.
pub fn inline_size(width: u32, height: u32, size: ImageSize) -> (u32, u32) {
  let (width, height) = (u64::from(width.max(1)), u64::from(height.max(1)));
  let is_whole_picture = width.max(height) < u64::from(THUMBNAIL_SIDE_PX);
  let (max_width, max_height) = inline_box(size);
  let (max_width, max_height) = (u64::from(max_width), u64::from(max_height));
  // Scale = min(max_width / width, max_height / height), as a fraction to stay in integers.
  let (numerator, denominator) = match max_width * height <= max_height * width {
    true => (max_width, width),
    false => (max_height, height),
  };
  let (numerator, denominator) = match is_whole_picture && numerator >= denominator {
    true => (1, 1),
    false => (numerator, denominator),
  };
  let scale = |side: u64, max: u64| {
    let scaled = (side * numerator / denominator).clamp(u64::from(MIN_SIDE_PX).min(max), max);
    u32::try_from(scaled).unwrap_or(MIN_SIDE_PX)
  };
  (scale(width, max_width), scale(height, max_height))
}

/// The inline size of a picture of `width` × `height` pixels (the sender's dimensions): fitted into
/// the box, never enlarged.
pub fn fit_picture(width: u32, height: u32, size: ImageSize) -> (u32, u32) {
  let (max_width, max_height) = inline_box(size);
  let (width, height) = (u64::from(width.max(1)), u64::from(height.max(1)));
  let (max_width, max_height) = (u64::from(max_width), u64::from(max_height));
  let (numerator, denominator) = match max_width * height <= max_height * width {
    true => (max_width, width),
    false => (max_height, height),
  };
  let (numerator, denominator) = match numerator >= denominator {
    true => (1, 1),
    false => (numerator, denominator),
  };
  let scale = |side: u64, max: u64| {
    let scaled = (side * numerator / denominator).clamp(u64::from(MIN_SIDE_PX).min(max), max);
    u32::try_from(scaled).unwrap_or(MIN_SIDE_PX)
  };
  (scale(width, max_width), scale(height, max_height))
}

/// Whether an inline preview downloads the full file: pictures within the size limit do, an
/// animated one only when the user doesn't ask for reduced motion (its thumbnail stays still then),
/// and videos never play by themselves.
pub fn loads_inline(kind: MediaKind, size_bytes: u64, prefers_reduced_motion: bool) -> bool {
  let fits = size_bytes <= MAX_INLINE_LOAD_BYTES;
  match kind {
    MediaKind::Image => fits,
    MediaKind::Animated => fits && !prefers_reduced_motion,
    MediaKind::Video => false,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const LARGE: ImageSize = ImageSize::Large;

  #[test]
  fn png_headers_give_the_size_and_anything_else_none() {
    let mut png = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13];
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&320_u32.to_be_bytes());
    png.extend_from_slice(&180_u32.to_be_bytes());
    assert_eq!(thumbnail_size(&png), Some((320, 180)));
    assert_eq!(thumbnail_type(&png), "image/png");
    assert_eq!(thumbnail_size(&png[..20]), None, "truncated");
    assert_eq!(thumbnail_size(b"GIF89a"), None);
    assert_eq!(thumbnail_size(&[]), None);
    let mut empty = png.clone();
    empty[16..20].copy_from_slice(&0_u32.to_be_bytes());
    assert_eq!(thumbnail_size(&empty), None, "zero width");
  }

  #[test]
  fn jpeg_sizes_come_from_the_frame_header_after_other_segments() {
    // SOI, an APP0 segment of 4 bytes, then SOF0 with height 480 and width 640.
    let jpeg = [
      0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x01, 0xe0, 0x02, 0x80,
    ];
    assert_eq!(thumbnail_size(&jpeg), Some((640, 480)));
    assert_eq!(thumbnail_type(&jpeg), "image/jpeg");
    assert_eq!(thumbnail_size(&jpeg[..12]), None, "truncated");
    let broken = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x00];
    assert_eq!(thumbnail_size(&broken), None, "a zero length doesn't loop");
  }

  #[test]
  fn thumbnails_of_big_pictures_fill_the_box_in_their_aspect_ratio() {
    assert_eq!(inline_size(320, 180, LARGE), (360, 202));
    assert_eq!(inline_size(240, 320, LARGE), (360, 480));
    assert_eq!(inline_size(320, 320, LARGE), (360, 360));
    assert_eq!(
      inline_size(148, 320, LARGE),
      (222, 480),
      "a phone screenshot gets the full height"
    );
    assert_eq!(
      inline_size(640, 480, LARGE),
      (360, 270),
      "larger thumbnails of newer apps"
    );
  }

  #[test]
  fn the_standard_size_has_a_smaller_box() {
    assert_eq!(inline_size(320, 180, ImageSize::Standard), (240, 135));
    assert_eq!(inline_size(148, 320, ImageSize::Standard), (148, 320));
  }

  #[test]
  fn sender_dimensions_fit_the_box_without_enlarging() {
    assert_eq!(fit_picture(8000, 6000, LARGE), (360, 270));
    assert_eq!(fit_picture(1170, 2532, LARGE), (221, 480), "a phone screenshot");
    assert_eq!(fit_picture(64, 48, LARGE), (64, 48));
    assert_eq!(fit_picture(8000, 6000, ImageSize::Standard), (240, 180));
  }

  #[test]
  fn small_pictures_keep_their_size_and_thin_ones_stay_clickable() {
    assert_eq!(inline_size(64, 48, LARGE), (64, 48));
    assert_eq!(inline_size(320, 4, LARGE), (360, 48));
    assert_eq!(
      inline_size(4, 300, LARGE),
      (48, 300),
      "a small thin picture stays clickable too"
    );
  }

  #[test]
  fn only_pictures_within_the_limit_load_and_gifs_not_under_reduced_motion() {
    assert!(loads_inline(MediaKind::Image, 2_000_000, true));
    assert!(!loads_inline(MediaKind::Image, MAX_INLINE_LOAD_BYTES + 1, false));
    assert!(loads_inline(MediaKind::Animated, 900_000, false));
    assert!(!loads_inline(MediaKind::Animated, 900_000, true));
    assert!(!loads_inline(MediaKind::Video, 1_000, false));
  }
}
