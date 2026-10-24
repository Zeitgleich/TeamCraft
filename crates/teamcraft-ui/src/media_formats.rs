//! Image formats beyond the four every client core decodes (issue #337): what a file is when the
//! browser doesn't say, which ones need the browser to draw their thumbnail, and whether a picture
//! moves.

/// Formats the browser draws the sender's thumbnail for: the client core in the page has no decoder
/// for them (SVG is drawn by `resvg` on native platforms only, AVIF and HEIC need C libraries).
const APP_THUMBNAIL_TYPES: [&str; 7] = [
  "image/svg+xml",
  "image/avif",
  "image/heic",
  "image/heif",
  "image/jxl",
  "image/tiff",
  "image/x-icon",
];

/// Extensions of image files that browsers often hand over without a type (HEIC on Windows and
/// Linux, AVIF and JPEG XL in older browsers).
const TYPES_BY_EXTENSION: [(&str, &str); 9] = [
  ("svg", "image/svg+xml"),
  ("avif", "image/avif"),
  ("heic", "image/heic"),
  ("heif", "image/heif"),
  ("jxl", "image/jxl"),
  ("webp", "image/webp"),
  ("apng", "image/apng"),
  ("bmp", "image/bmp"),
  ("ico", "image/x-icon"),
];

/// The media type of a picked file: the browser's, or one from the extension when it gave none.
pub fn media_type_for(file_name: &str, browser_type: &str) -> String {
  if !browser_type.trim().is_empty() && browser_type != "application/octet-stream" {
    return browser_type.to_owned();
  }
  let extension = file_name
    .rsplit_once('.')
    .map(|(_, extension)| extension.to_ascii_lowercase())
    .unwrap_or_default();
  TYPES_BY_EXTENSION
    .iter()
    .find(|(known, _)| *known == extension)
    .map_or("application/octet-stream", |(_, media_type)| media_type)
    .to_owned()
}

/// Whether the sender's app draws the thumbnail with the browser's decoder.
pub fn needs_app_thumbnail(media_type: &str) -> bool {
  let media_type = media_type.trim().to_ascii_lowercase();
  APP_THUMBNAIL_TYPES.contains(&media_type.as_str())
}

pub fn is_svg(media_type: &str) -> bool {
  media_type.trim().eq_ignore_ascii_case("image/svg+xml")
}

/// Whether picture bytes are an animation: a GIF, an animated WebP (`ANIM` flag) or an APNG
/// (`acTL` before the image data). Only the header is read.
pub fn is_animated(bytes: &[u8]) -> bool {
  if bytes.starts_with(b"GIF8") {
    return true;
  }
  if bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
    // VP8X: the second bit of its flags byte says "animation".
    return bytes.get(12..16) == Some(b"VP8X") && bytes.get(20).is_some_and(|flags| flags & 0b10 != 0);
  }
  is_apng(bytes)
}

/// Walks the PNG chunks up to the first `IDAT`; an `acTL` before it makes it animated.
fn is_apng(bytes: &[u8]) -> bool {
  const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
  if !bytes.starts_with(&SIGNATURE) {
    return false;
  }
  let mut at = SIGNATURE.len();
  while let (Some(length), Some(kind)) = (bytes.get(at..at + 4), bytes.get(at + 4..at + 8)) {
    match kind {
      b"acTL" => return true,
      b"IDAT" | b"IEND" => return false,
      _ => {},
    }
    let Ok(length) = <[u8; 4]>::try_from(length).map(u32::from_be_bytes) else {
      return false;
    };
    // Length, type, data and CRC.
    at = at.saturating_add(12).saturating_add(length as usize);
  }
  false
}

/// The short label on a still animation (reduced motion): "GIF", "WEBP", "APNG".
pub fn animation_label(media_type: &str) -> String {
  let subtype = media_type
    .trim()
    .rsplit_once('/')
    .map_or(media_type, |(_, subtype)| subtype);
  match subtype.to_ascii_lowercase().as_str() {
    "png" | "apng" => "APNG".to_owned(),
    other => other.to_ascii_uppercase(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_missing_browser_type_comes_from_the_extension() {
    assert_eq!(media_type_for("IMG_0001.HEIC", ""), "image/heic");
    assert_eq!(
      media_type_for("logo.svg", "application/octet-stream"),
      "image/svg+xml"
    );
    assert_eq!(media_type_for("photo.avif", "image/avif"), "image/avif");
    assert_eq!(media_type_for("notes.txt", "text/plain"), "text/plain");
    assert_eq!(media_type_for("archive", ""), "application/octet-stream");
    assert_eq!(media_type_for("weird.", ""), "application/octet-stream");
  }

  #[test]
  fn only_formats_the_core_cannot_decode_in_the_page_need_the_browser() {
    assert!(needs_app_thumbnail("image/svg+xml"));
    assert!(needs_app_thumbnail(" IMAGE/HEIC "));
    assert!(needs_app_thumbnail("image/avif"));
    assert!(!needs_app_thumbnail("image/jpeg"));
    assert!(!needs_app_thumbnail("image/webp"));
    assert!(!needs_app_thumbnail("application/pdf"));
  }

  #[test]
  fn animations_are_told_apart_by_their_header() {
    assert!(is_animated(b"GIF89a...."));
    let mut webp = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
    webp.push(0b10);
    assert!(is_animated(&webp), "animated WebP");
    let mut still = webp.clone();
    still[20] = 0b1000;
    assert!(!is_animated(&still), "WebP with alpha only");
    assert!(!is_animated(b"RIFF\0\0\0\0WEBPVP8 "), "simple WebP");

    let png = |chunks: &[&[u8; 4]]| {
      let mut bytes = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
      for kind in chunks {
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(*kind);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
      }
      bytes
    };
    assert!(is_animated(&png(&[b"IHDR", b"acTL", b"IDAT"])));
    assert!(
      !is_animated(&png(&[b"IHDR", b"IDAT", b"acTL"])),
      "after the image data it's not APNG"
    );
    assert!(!is_animated(&png(&[b"IHDR"])[..12]), "truncated");
    assert!(!is_animated(b""));
  }

  #[test]
  fn still_animations_are_labelled_by_format() {
    assert_eq!(animation_label("image/gif"), "GIF");
    assert_eq!(animation_label("image/webp"), "WEBP");
    assert_eq!(animation_label("image/png"), "APNG");
  }
}
