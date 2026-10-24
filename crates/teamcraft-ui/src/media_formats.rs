//! What a picture file is, from its bytes: whether it moves (GIF, animated WebP, APNG), which decides
//! whether the conversation may show a still rendition of it.

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

#[cfg(test)]
mod tests {
  use super::*;

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
}
