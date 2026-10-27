//! A file's bytes as text: UTF-8 (with or without a byte order mark, a few broken bytes repaired),
//! UTF-16 (with a byte order mark, or clearly UTF-16 without one) and, when the bytes are clearly
//! text in no Unicode encoding, Latin-1 as Windows writes it (Windows-1252). Anything else is
//! binary and not shown: a preview of an executable or an archive helps nobody.

const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
/// The first bytes looked at to recognise UTF-16 without a byte order mark.
const UTF16_SAMPLE_BYTES: usize = 4096;
/// Control characters (other than tab, line breaks, form feed and escape) in more than one byte in
/// this many make a file binary.
const CONTROL_DIVISOR: usize = 100;
/// Without valid UTF-8, a text has at most this share of bytes above ASCII (in tenths): Latin-1
/// text is mostly ASCII, compressed or encrypted data is not.
const MAX_HIGH_TENTHS: usize = 3;
/// What bytes 0x80 to 0x9F are in Windows-1252; `None` where it has no character.
const WINDOWS_1252_HIGH: [Option<char>; 32] = [
  Some('€'),
  None,
  Some('‚'),
  Some('ƒ'),
  Some('„'),
  Some('…'),
  Some('†'),
  Some('‡'),
  Some('ˆ'),
  Some('‰'),
  Some('Š'),
  Some('‹'),
  Some('Œ'),
  None,
  Some('Ž'),
  None,
  None,
  Some('‘'),
  Some('’'),
  Some('“'),
  Some('”'),
  Some('•'),
  Some('–'),
  Some('—'),
  Some('˜'),
  Some('™'),
  Some('š'),
  Some('›'),
  Some('œ'),
  None,
  Some('ž'),
  Some('Ÿ'),
];

/// A file's text and how it was read.
#[derive(Debug, PartialEq, Eq)]
pub struct Decoded {
  pub text: String,
  pub encoding: TextEncoding,
}

/// How a file's bytes were read; the viewer says so when it isn't plain UTF-8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextEncoding {
  Utf8,
  /// UTF-8 with a few invalid bytes, shown as U+FFFD.
  Utf8Repaired,
  Utf16,
  /// Not UTF-8 but clearly text: read as Windows-1252, the Latin-1 that Windows writes.
  Latin1,
}

/// Why a file is not shown as text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TextDecodeError {
  #[error("the file is not text")]
  Binary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Endian {
  Little,
  Big,
}

/// Reads `bytes` as text, or refuses them as binary.
pub fn decode(mut bytes: Vec<u8>) -> Result<Decoded, TextDecodeError> {
  if bytes.starts_with(&UTF8_BOM) {
    bytes.drain(..UTF8_BOM.len());
  }
  let maybe_utf16 = match bytes.get(..2) {
    Some([0xFF, 0xFE]) => Some((Endian::Little, 2)),
    Some([0xFE, 0xFF]) => Some((Endian::Big, 2)),
    _ => utf16_without_bom(&bytes).map(|endian| (endian, 0)),
  };
  if let Some((endian, skip)) = maybe_utf16 {
    return utf16(&bytes[skip..], endian);
  }
  if has_binary_bytes(&bytes) {
    return Err(TextDecodeError::Binary);
  }
  utf8_or_latin1(bytes)
}

/// UTF-16 without a byte order mark: in the start of the file, nearly every other byte is zero
/// (ASCII characters) and the bytes in between are not.
fn utf16_without_bom(bytes: &[u8]) -> Option<Endian> {
  let sample = &bytes[..bytes.len().min(UTF16_SAMPLE_BYTES) & !1];
  let pairs = sample.len() / 2;
  if pairs < 2 {
    return None;
  }
  let zeros_at = |parity: usize| {
    sample
      .iter()
      .skip(parity)
      .step_by(2)
      .filter(|&&byte| byte == 0)
      .count()
  };
  let (even, odd) = (zeros_at(0), zeros_at(1));
  let is_mostly = |zeros: usize| zeros * 10 >= pairs * 4;
  let is_rare = |zeros: usize| zeros * 20 <= pairs;
  if is_mostly(odd) && is_rare(even) {
    Some(Endian::Little)
  } else if is_mostly(even) && is_rare(odd) {
    Some(Endian::Big)
  } else {
    None
  }
}

fn utf16(bytes: &[u8], endian: Endian) -> Result<Decoded, TextDecodeError> {
  let (pairs, rest) = bytes.as_chunks::<2>();
  let units = pairs.iter().map(|pair| match endian {
    Endian::Little => u16::from_le_bytes(*pair),
    Endian::Big => u16::from_be_bytes(*pair),
  });
  let mut text: String = char::decode_utf16(units)
    .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
    .collect();
  if !rest.is_empty() {
    text.push(char::REPLACEMENT_CHARACTER);
  }
  let controls = text
    .chars()
    .filter(|&c| u32::from(c) < 0x80 && is_control(c as u8))
    .count();
  if text.contains('\0') || controls * CONTROL_DIVISOR > text.len() {
    return Err(TextDecodeError::Binary);
  }
  Ok(Decoded {
    text,
    encoding: TextEncoding::Utf16,
  })
}

/// A NUL byte, or more control characters than a text has.
fn has_binary_bytes(bytes: &[u8]) -> bool {
  let mut controls = 0;
  for &byte in bytes {
    if byte == 0 {
      return true;
    }
    if is_control(byte) {
      controls += 1;
    }
  }
  controls * CONTROL_DIVISOR > bytes.len()
}

/// Control characters a text doesn't have: everything below space but tab, the line breaks, form
/// feed and escape (colour codes in logs), and DEL.
fn is_control(byte: u8) -> bool {
  matches!(byte, 0x00..=0x08 | 0x0E..=0x1A | 0x1C..=0x1F | 0x7F)
}

fn utf8_or_latin1(bytes: Vec<u8>) -> Result<Decoded, TextDecodeError> {
  let bytes = match String::from_utf8(bytes) {
    Ok(text) => {
      return Ok(Decoded {
        text,
        encoding: TextEncoding::Utf8,
      });
    },
    Err(error) => error.into_bytes(),
  };
  let (multibyte, invalid) = utf8_health(&bytes);
  if multibyte >= invalid {
    // Mostly UTF-8: a truncated or slightly damaged file.
    return Ok(Decoded {
      text: String::from_utf8_lossy(&bytes).into_owned(),
      encoding: TextEncoding::Utf8Repaired,
    });
  }
  let high = bytes.iter().filter(|byte| !byte.is_ascii()).count();
  if high * 10 > bytes.len() * MAX_HIGH_TENTHS {
    return Err(TextDecodeError::Binary);
  }
  Ok(Decoded {
    text: windows_1252(&bytes),
    encoding: TextEncoding::Latin1,
  })
}

/// How many valid non-ASCII characters and how many invalid sequences the bytes hold as UTF-8.
fn utf8_health(bytes: &[u8]) -> (usize, usize) {
  bytes.utf8_chunks().fold((0, 0), |(multibyte, invalid), chunk| {
    let valid = chunk.valid().chars().filter(|c| !c.is_ascii()).count();
    (
      multibyte + valid,
      invalid + usize::from(!chunk.invalid().is_empty()),
    )
  })
}

fn windows_1252(bytes: &[u8]) -> String {
  bytes
    .iter()
    .map(|&byte| match byte {
      0x80..=0x9F => WINDOWS_1252_HIGH[usize::from(byte - 0x80)].unwrap_or(char::REPLACEMENT_CHARACTER),
      _ => char::from(byte),
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  mod unicode {
    use super::*;

    #[test]
    fn utf8_with_and_without_a_byte_order_mark() {
      assert_eq!(read(b"caf\xC3\xA9"), ("café".to_owned(), TextEncoding::Utf8));
      assert_eq!(read(b"\xEF\xBB\xBFhi\n"), ("hi\n".to_owned(), TextEncoding::Utf8));
    }

    #[test]
    fn a_few_broken_bytes_in_utf8_are_repaired() {
      let (text, encoding) = read(
        "Grüße, schöne Größe \u{2014} ok"
          .bytes()
          .chain([0xFF])
          .collect::<Vec<_>>()
          .as_slice(),
      );
      assert_eq!(encoding, TextEncoding::Utf8Repaired);
      assert!(text.ends_with("ok\u{FFFD}"));
    }

    #[test]
    fn utf16_with_a_byte_order_mark() {
      let little: Vec<u8> = [0xFF, 0xFE].into_iter().chain(utf16_le("Log: ü\r\n")).collect();
      assert_eq!(read(&little), ("Log: ü\r\n".to_owned(), TextEncoding::Utf16));
      let big: Vec<u8> = [0xFE, 0xFF]
        .into_iter()
        .chain("ok".encode_utf16().flat_map(u16::to_be_bytes))
        .collect();
      assert_eq!(read(&big), ("ok".to_owned(), TextEncoding::Utf16));
    }

    #[test]
    fn utf16_without_a_byte_order_mark_is_recognised() {
      let bytes: Vec<u8> = utf16_le("2026-10-27 INFO started\r\n").collect();
      assert_eq!(read(&bytes).0, "2026-10-27 INFO started\r\n");
    }

    #[test]
    fn an_odd_trailing_byte_of_utf16_is_marked() {
      let bytes: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain(utf16_le("ab"))
        .chain([0x41])
        .collect();
      assert_eq!(read(&bytes).0, "ab\u{FFFD}");
    }

    #[test]
    fn colour_codes_and_form_feeds_are_text() {
      assert!(decode(b"\x1b[31mred\x1b[0m\x0cnext page\ttab".to_vec()).is_ok());
    }
  }

  mod latin1 {
    use super::*;

    #[test]
    fn text_in_no_unicode_encoding_is_read_as_windows_1252() {
      assert_eq!(
        read(b"Gr\xFC\xDFe aus der Stadt: Preis \x80 5, \x93ok\x94 gesagt"),
        (
          "Grüße aus der Stadt: Preis € 5, “ok” gesagt".to_owned(),
          TextEncoding::Latin1
        )
      );
    }

    #[test]
    fn bytes_windows_1252_has_no_character_for_are_marked() {
      assert_eq!(read(b"a\x81b and caf\xE9 text").0, "a\u{FFFD}b and café text");
    }
  }

  mod binary {
    use super::*;

    #[test]
    fn a_nul_byte_makes_a_file_binary() {
      assert_eq!(
        decode(b"PK\x03\x04\0\0abc".to_vec()),
        Err(TextDecodeError::Binary)
      );
      assert_eq!(decode(b"text\0".to_vec()), Err(TextDecodeError::Binary));
    }

    #[test]
    fn many_control_characters_make_a_file_binary() {
      let bytes: Vec<u8> = (0..200).map(|n| if n % 10 == 0 { 0x02 } else { b'a' }).collect();
      assert_eq!(decode(bytes), Err(TextDecodeError::Binary));
    }

    #[test]
    fn mostly_high_bytes_without_utf8_are_binary() {
      let bytes: Vec<u8> = (0..400u32).map(|n| 0x80 | (n % 0x40) as u8).collect();
      assert_eq!(decode(bytes), Err(TextDecodeError::Binary));
    }

    #[test]
    fn utf16_of_binary_content_is_refused() {
      let bytes: Vec<u8> = [0xFF, 0xFE, 0x00, 0x00, 0x41, 0x00].to_vec();
      assert_eq!(decode(bytes), Err(TextDecodeError::Binary));
    }

    #[test]
    fn an_empty_file_is_text() {
      assert_eq!(read(b""), (String::new(), TextEncoding::Utf8));
    }
  }

  fn read(bytes: &[u8]) -> (String, TextEncoding) {
    let decoded = decode(bytes.to_vec()).expect("is text");
    (decoded.text, decoded.encoding)
  }

  fn utf16_le(text: &str) -> impl Iterator<Item = u8> + '_ {
    text.encode_utf16().flat_map(u16::to_le_bytes)
  }
}
