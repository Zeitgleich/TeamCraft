//! Width and height of a picture read from its header, turned upright by its EXIF orientation,
//! without a decoder (#135). The browser's engine has no image codecs (the browser decodes the
//! pictures it sends, `thumbnail::PictureCodec`), but it still sends every picture's size with the
//! message, so receivers lay it out before it arrives.
//!
//! It reads what `image`'s decoders read for the same formats (PNG, JPEG, GIF, WebP, BMP, ICO):
//! `thumbnail::image_dimensions` natively, which the tests compare it with. A header this doesn't
//! read is `None`, never a guess: the size is only a layout hint, a wrong one would distort it.

/// The upright width and height of a PNG, JPEG, GIF, WebP, BMP or ICO picture from its header;
/// `None` for anything else, a truncated header or an empty picture.
pub fn upright_size(bytes: &[u8]) -> Option<(u32, u32)> {
  let header = match media_type(bytes)? {
    "image/png" => png(bytes)?,
    "image/jpeg" => jpeg(bytes)?,
    "image/gif" => gif(bytes)?,
    "image/webp" => webp(bytes)?,
    "image/bmp" => bmp(bytes)?,
    _ => ico(bytes)?,
  };
  let Header {
    width,
    height,
    is_sideways,
  } = header;
  if width == 0 || height == 0 {
    return None;
  }
  Some(match is_sideways {
    true => (height, width),
    false => (width, height),
  })
}

/// The media type of a picture this reads, told by its bytes (as `image` guesses the format):
/// `image/png`, `image/jpeg`, `image/gif`, `image/webp`, `image/bmp` or `image/x-icon`.
pub fn media_type(bytes: &[u8]) -> Option<&'static str> {
  if bytes.starts_with(PNG_SIGNATURE) {
    Some("image/png")
  } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
    Some("image/jpeg")
  } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
    Some("image/gif")
  } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
    Some("image/webp")
  } else if bytes.starts_with(b"BM") {
    Some("image/bmp")
  } else if bytes.starts_with(&[0, 0, 1, 0]) {
    Some("image/x-icon")
  } else {
    None
  }
}

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// A picture's stored size, and whether its orientation turns it by a quarter.
struct Header {
  width: u32,
  height: u32,
  is_sideways: bool,
}

impl Header {
  fn upright(width: u32, height: u32) -> Self {
    Self {
      width,
      height,
      is_sideways: false,
    }
  }
}

fn u16_be(bytes: &[u8], at: usize) -> Option<u16> {
  Some(u16::from_be_bytes(
    bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
  ))
}

fn u16_le(bytes: &[u8], at: usize) -> Option<u16> {
  Some(u16::from_le_bytes(
    bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
  ))
}

fn u32_be(bytes: &[u8], at: usize) -> Option<u32> {
  Some(u32::from_be_bytes(
    bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
  ))
}

fn u32_le(bytes: &[u8], at: usize) -> Option<u32> {
  Some(u32::from_le_bytes(
    bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
  ))
}

fn u24_le(bytes: &[u8], at: usize) -> Option<u32> {
  let low = u32::from(u16_le(bytes, at)?);
  Some(low | u32::from(*bytes.get(at.checked_add(2)?)?) << 16)
}

/// Whether an EXIF block (a TIFF header and its first directory, as `image` reads it) turns the
/// picture by a quarter: orientations 5 to 8. A block without a readable orientation doesn't.
fn exif_is_sideways(exif: &[u8]) -> bool {
  let read_u16 = |at: usize| match exif.get(..4) {
    Some([0x49, 0x49, 42, 0]) => u16_le(exif, at),
    Some([0x4D, 0x4D, 0, 42]) => u16_be(exif, at),
    _ => None,
  };
  let read_u32 = |at: usize| match exif.get(..4) {
    Some([0x49, 0x49, 42, 0]) => u32_le(exif, at),
    Some([0x4D, 0x4D, 0, 42]) => u32_be(exif, at),
    _ => None,
  };
  let orientation = || -> Option<u16> {
    let directory = usize::try_from(read_u32(4)?).ok()?;
    let entries = read_u16(directory)?;
    (0..usize::from(entries)).find_map(|index| {
      let entry = directory + 2 + index * 12;
      let is_orientation =
        read_u16(entry)? == 0x0112 && read_u16(entry + 2)? == 3 && read_u32(entry + 4)? == 1;
      // An entry that can't be read ends the search, as in `image`.
      is_orientation.then(|| read_u16(entry + 8)).flatten()
    })
  };
  matches!(orientation(), Some(5..=8))
}

/// IHDR (checked like the `png` crate does), then the chunks before the image data for an `eXIf`.
fn png(bytes: &[u8]) -> Option<Header> {
  let mut at = PNG_SIGNATURE.len();
  let ihdr = chunk(bytes, at)?;
  if ihdr.kind != *b"IHDR" || ihdr.data.len() != 13 {
    return None;
  }
  let width = u32_be(ihdr.data, 0)?;
  let height = u32_be(ihdr.data, 4)?;
  let (bit_depth, color_type) = (ihdr.data[8], ihdr.data[9]);
  let is_valid_depth = match color_type {
    0 => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
    3 => matches!(bit_depth, 1 | 2 | 4 | 8),
    2 | 4 | 6 => matches!(bit_depth, 8 | 16),
    _ => false,
  };
  if !is_valid_depth || width > i32::MAX as u32 || height > i32::MAX as u32 {
    return None;
  }
  at = ihdr.end;
  let mut is_sideways = false;
  loop {
    // The image data's chunk header is where the `png` crate's header reading stops.
    let kind = bytes.get(at + 4..at + 8)?;
    if kind == b"IDAT" {
      return Some(Header {
        width,
        height,
        is_sideways,
      });
    }
    let next = chunk(bytes, at)?;
    if next.kind == *b"eXIf" {
      is_sideways = exif_is_sideways(next.data);
    }
    at = next.end;
  }
}

struct Chunk<'a> {
  kind: [u8; 4],
  data: &'a [u8],
  end: usize,
}

/// The PNG chunk at `at`, complete and with a matching checksum.
fn chunk(bytes: &[u8], at: usize) -> Option<Chunk<'_>> {
  let length = usize::try_from(u32_be(bytes, at)?).ok()?;
  let data_start = at.checked_add(8)?;
  let data_end = data_start.checked_add(length)?;
  let end = data_end.checked_add(4)?;
  let crc = u32_be(bytes, data_end)?;
  if crc32(bytes.get(at + 4..data_end)?) != crc {
    return None;
  }
  Some(Chunk {
    kind: bytes.get(at + 4..data_start)?.try_into().ok()?,
    data: bytes.get(data_start..data_end)?,
    end,
  })
}

/// CRC-32 (ISO-HDLC), as PNG chunks use it.
fn crc32(bytes: &[u8]) -> u32 {
  let mut crc = u32::MAX;
  for byte in bytes {
    crc ^= u32::from(*byte);
    for _ in 0..8 {
      crc = if crc & 1 == 1 {
        (crc >> 1) ^ 0xEDB8_8320
      } else {
        crc >> 1
      };
    }
  }
  !crc
}

/// The markers up to the start of the scan, as `zune-jpeg` reads them: the frame's size and the last
/// EXIF block's orientation. Without a start of scan it is no picture `image` would read.
fn jpeg(bytes: &[u8]) -> Option<Header> {
  let mut at = 2;
  let mut maybe_size = None;
  let mut is_sideways = false;
  loop {
    // Fill bytes and anything else before the next marker are skipped.
    while *bytes.get(at)? != 0xFF {
      at += 1;
    }
    while *bytes.get(at)? == 0xFF {
      at += 1;
    }
    let marker = *bytes.get(at)?;
    at += 1;
    match marker {
      // Byte stuffing, restart markers and markers without a segment.
      0x00 | 0x01 | 0xD0..=0xD8 => continue,
      // The end of the picture before its scan.
      0xD9 => return None,
      _ => {},
    }
    let length = usize::from(u16_be(bytes, at)?);
    if length < 2 {
      return None;
    }
    let segment = bytes.get(at + 2..at + length)?;
    match marker {
      0xDA => {
        let (width, height) = maybe_size?;
        return Some(Header {
          width,
          height,
          is_sideways,
        });
      },
      // Start of frame, any coding (0xC4, 0xC8 and 0xCC are tables, not frames).
      0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
        let height = u32::from(u16_be(segment, 1)?);
        let width = u32::from(u16_be(segment, 3)?);
        maybe_size = Some((width, height));
      },
      0xE1 if segment.len() > 6 && segment.starts_with(b"Exif\0\0") => {
        is_sideways = exif_is_sideways(&segment[6..]);
      },
      _ => {},
    }
    at += length;
  }
}

/// The logical screen's size.
fn gif(bytes: &[u8]) -> Option<Header> {
  let width = u32::from(u16_le(bytes, 6)?);
  let height = u32::from(u16_le(bytes, 8)?);
  // The rest of the screen descriptor.
  bytes.get(12)?;
  Some(Header::upright(width, height))
}

/// The first chunk's size (a lossy or lossless picture, or the extended format's canvas), and for
/// the extended format the orientation in its `EXIF` chunk.
fn webp(bytes: &[u8]) -> Option<Header> {
  let kind = bytes.get(12..16)?;
  let data = 20;
  match kind {
    b"VP8 " => {
      let tag = u24_le(bytes, data)?;
      // Only a key frame, with its start code.
      if tag & 1 != 0 || bytes.get(data + 3..data + 6)? != [0x9D, 0x01, 0x2A] {
        return None;
      }
      let width = u32::from(u16_le(bytes, data + 6)? & 0x3FFF);
      let height = u32::from(u16_le(bytes, data + 8)? & 0x3FFF);
      Some(Header::upright(width, height))
    },
    b"VP8L" => {
      if *bytes.get(data)? != 0x2F {
        return None;
      }
      let header = u32_le(bytes, data + 1)?;
      if header >> 29 != 0 {
        return None;
      }
      let width = (1 + header) & 0x3FFF;
      let height = (1 + (header >> 14)) & 0x3FFF;
      Some(Header::upright(width, height))
    },
    b"VP8X" => {
      let flags = *bytes.get(data)?;
      let width = u24_le(bytes, data + 4)? + 1;
      let height = u24_le(bytes, data + 7)? + 1;
      let chunk_size = u32_le(bytes, 16)?;
      let riff_end = usize::try_from(u32_le(bytes, 4)?).ok()?.saturating_add(8);
      let mut at = data.checked_add(usize::try_from(chunk_size + (chunk_size & 1)).ok()?)?;
      let mut maybe_exif = None;
      let mut found = Vec::new();
      while at.saturating_add(8) <= riff_end.min(bytes.len()) {
        let kind: [u8; 4] = bytes[at..at + 4].try_into().ok()?;
        let size = usize::try_from(u32_le(bytes, at + 4)?).ok()?;
        let start = at + 8;
        if &kind == b"EXIF" && maybe_exif.is_none() {
          maybe_exif = Some(bytes.get(start..start.checked_add(size)?)?);
        }
        found.push(kind);
        at = start.checked_add(size)?.checked_add(size & 1)?;
      }
      // The chunks `image-webp` requires: those the flags name, and the image data.
      let has = |kind: &[u8; 4]| found.contains(kind);
      let is_animated = flags & 0b10 != 0;
      let is_complete = match is_animated {
        true => has(b"ANIM") && has(b"ANMF"),
        false => has(b"VP8 ") != has(b"VP8L"),
      };
      if !is_complete || (flags & 0b1000 != 0 && !has(b"EXIF")) || (flags & 0b100 != 0 && !has(b"XMP ")) {
        return None;
      }
      let is_sideways = maybe_exif.is_some_and(exif_is_sideways);
      Some(Header {
        width,
        height,
        is_sideways,
      })
    },
    _ => None,
  }
}

/// The file header and the bitmap header after it.
fn bmp(bytes: &[u8]) -> Option<Header> {
  bitmap_info(bytes, 14, false)
}

/// The size of the bitmap whose info header starts at `at`, checked as `image` checks it; in an
/// icon the height counts the mask too, so it is halved.
fn bitmap_info(bytes: &[u8], at: usize, is_icon: bool) -> Option<Header> {
  /// Larger bitmaps `image` refuses.
  const MAX_SIDE: i64 = 0xFFFF;
  let header_size = u32_le(bytes, at)?;
  let (width, height, planes, bits, compression) = match header_size {
    // The OS/2 core header: 16-bit sides, no compression.
    12 => {
      let bits = u16_le(bytes, at + 10)?;
      if !matches!(bits, 1 | 4 | 8 | 24) {
        return None;
      }
      (
        i64::from(u16_le(bytes, at + 4)?),
        i64::from(u16_le(bytes, at + 6)?),
        u16_le(bytes, at + 8)?,
        bits,
        0,
      )
    },
    // The info header and its later versions (V2 to V5).
    40 | 52 | 56 | 108 | 124 => (
      i64::from(u32_le(bytes, at + 4)? as i32),
      i64::from(u32_le(bytes, at + 8)? as i32),
      u16_le(bytes, at + 12)?,
      u16_le(bytes, at + 14)?,
      u32_le(bytes, at + 16)?,
    ),
    _ => return None,
  };
  let is_top_down = height < 0;
  let is_valid = match compression {
    0 => matches!(bits, 1 | 2 | 4 | 8 | 16 | 24 | 32),
    1 => bits == 8 && !is_top_down,
    2 => bits == 4 && !is_top_down,
    3 => matches!(bits, 16 | 32),
    _ => false,
  };
  let is_in_range = (0..=MAX_SIDE).contains(&width) && height <= MAX_SIDE && height != i64::from(i32::MIN);
  if !is_valid || !is_in_range || planes != 1 {
    return None;
  }
  let height = match is_icon {
    true => height.abs() / 2,
    false => height.abs(),
  };
  Some(Header::upright(
    u32::try_from(width).ok()?,
    u32::try_from(height).ok()?,
  ))
}

/// The entry `image` picks (the most bits per pixel, then the most pixels), and that entry's own
/// size: a PNG's or a bitmap's.
fn ico(bytes: &[u8]) -> Option<Header> {
  let count = usize::from(u16_le(bytes, 4)?);
  let side = |at: usize| -> Option<u32> {
    Some(match *bytes.get(at)? {
      0 => 256,
      side => u32::from(side),
    })
  };
  let mut entries = Vec::with_capacity(count);
  for index in 0..count {
    let entry = 6 + index * 16;
    let (planes, bits) = (u16_le(bytes, entry + 4)?, u16_le(bytes, entry + 6)?);
    if planes > 256 || bits > 256 {
      return None;
    }
    let score = (bits, side(entry)? * side(entry + 1)?);
    entries.push((score, usize::try_from(u32_le(bytes, entry + 12)?).ok()?));
  }
  // As `image` picks: the last entry, unless an earlier one is strictly better (the first of those).
  let (mut best_score, mut offset) = *entries.last()?;
  for &(score, entry_offset) in &entries[..entries.len() - 1] {
    if score > best_score {
      (best_score, offset) = (score, entry_offset);
    }
  }
  let image = bytes.get(offset..)?;
  match image.starts_with(PNG_SIGNATURE) {
    true => png(image).map(|header| Header::upright(header.width, header.height)),
    false => bitmap_info(image, 0, true),
  }
}
