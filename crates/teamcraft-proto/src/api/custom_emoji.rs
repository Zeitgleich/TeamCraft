//! Custom emoji of the workspace (issue #207, e2ee.md §4.13, admin.md §3.11): small pictures named
//! `:shortcode:` that admins add, and everybody uses in messages and reactions.
//!
//! The pictures are workspace assets like the organization's call backgrounds: stored unencrypted
//! and served only to signed-in people of the instance. What makes them trustworthy is the
//! signature: an admin's device signs each emoji's [`CustomEmojiDocument`] (name and SHA-256 of
//! the picture), and clients show a picture only when that signature verifies against the signed
//! admin roster and the bytes match the hash. A message names the emoji it uses inside its
//! ciphertext ([`CustomEmojiRef`]); the text keeps the `:shortcode:`, which is what older clients
//! and clients without the picture show.

use std::fmt;

use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{Base64Bytes, CustomEmojiId, UnixMillis, UserId};

/// Default of `[limits] max_custom_emoji`: emoji a workspace may have.
pub const DEFAULT_MAX_CUSTOM_EMOJI: u32 = 500;
/// Default of `[limits] max_custom_emoji_bytes`: the largest picture. A 128×128 PNG of a logo is
/// usually below 20 KiB.
pub const DEFAULT_MAX_CUSTOM_EMOJI_BYTES: u64 = 64 * 1024;
/// Default of `[limits] max_custom_emoji_side_px`: the longest side of a picture. Emoji show at
/// text size, twice that on sharp screens, and four times in a message of emoji only.
pub const DEFAULT_MAX_CUSTOM_EMOJI_SIDE_PX: u32 = 128;
/// The highest values the settings accept: what any client still downloads for a whole set.
pub const MAX_CUSTOM_EMOJI_LIMIT: u32 = 5_000;
pub const MAX_CUSTOM_EMOJI_BYTES_LIMIT: u64 = 512 * 1024;
pub const MAX_CUSTOM_EMOJI_SIDE_PX_LIMIT: u32 = 512;
/// Shortest and longest shortcode, without the colons.
pub const MIN_CUSTOM_EMOJI_SHORTCODE_CHARS: usize = 2;
pub const MAX_CUSTOM_EMOJI_SHORTCODE_CHARS: usize = 32;
/// Custom emoji a message names in its ciphertext at most; more stay text.
pub const MAX_CUSTOM_EMOJI_PER_MESSAGE: usize = 50;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];
const PNG_IHDR_LENGTH: [u8; 4] = [0, 0, 0, 13];
/// The animation flag of a WebP file's extended header (`VP8X`).
const WEBP_ANIMATION_FLAG: u8 = 0x02;

wire_enum! {
  /// The picture formats of custom emoji: still PNG and WebP. Animated pictures wait for the
  /// reduced-motion rules of #201; GIF, SVG (scripts) and JPEG (no transparency) are refused.
  pub enum CustomEmojiFormat {
    Png => "image/png",
    Webp => "image/webp",
  }
}

/// SHA-256 of a custom emoji's picture, which names it inside messages and in the device's cache.
/// Wire format: 64 lowercase hex characters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CustomEmojiHash([u8; 32]);

/// What an admin's device signs for one emoji (JSON, the payload of [`SignedCustomEmoji`]). A new
/// name is a new document over the same picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CustomEmojiDocument {
  /// Without colons: `ship_it` ([`check_custom_emoji_shortcode`]).
  pub shortcode: String,
  /// SHA-256 of the picture exactly as uploaded and served.
  #[schema(value_type = String)]
  pub sha256: CustomEmojiHash,
  pub format: CustomEmojiFormat,
  /// When the admin's device signed it (its own clock): judged against when the signer was an
  /// admin of the verified roster.
  pub signed_at_ms: i64,
}

/// A [`CustomEmojiDocument`] as JSON, verbatim as signed, and the signature: a signed log entry of
/// the crypto crate's `WorkspaceLog::CustomEmoji` by a device of a workspace admin or owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SignedCustomEmoji {
  pub payload: Base64Bytes,
  pub signature: Base64Bytes,
}

/// `POST /api/v1/custom-emoji` (admins): adds an emoji. The server checks the picture (format,
/// size, sides, hash) and the document's name, and keeps both as sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateCustomEmojiRequest {
  /// The picture: a still PNG or WebP ([`inspect_custom_emoji_picture`]).
  pub picture: Base64Bytes,
  pub signed: SignedCustomEmoji,
}

/// `PUT /api/v1/custom-emoji/{id}` (admins): a new name, signed again over the same picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RenameCustomEmojiRequest {
  pub signed: SignedCustomEmoji,
}

/// One emoji as the server lists it. `shortcode`, `sha256` and `format` repeat the signed
/// document for display; clients believe only the document after verifying its signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CustomEmojiResponse {
  pub id: CustomEmojiId,
  pub shortcode: String,
  #[schema(value_type = String)]
  pub sha256: CustomEmojiHash,
  pub format: CustomEmojiFormat,
  pub size_bytes: u64,
  pub width: u32,
  pub height: u32,
  /// Who added it; absent once their account is deleted.
  #[serde(rename = "created_by", default, skip_serializing_if = "Option::is_none")]
  pub maybe_created_by: Option<UserId>,
  pub created_at: UnixMillis,
  pub signed: SignedCustomEmoji,
}

/// `GET /api/v1/custom-emoji/{id}/download`: where to fetch the picture. Clients check its hash
/// against the signed document before showing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CustomEmojiDownloadResponse {
  /// Presigned `GET` URL of the picture.
  pub download_url: String,
  pub size_bytes: u64,
  pub expires_at: UnixMillis,
}

/// `GET /api/v1/custom-emoji`: the workspace's emoji and the limits for new ones (everybody signed
/// in, guests included: they read the messages that use them).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListCustomEmojiResponse {
  pub emoji: Vec<CustomEmojiResponse>,
  pub limits: CustomEmojiLimits,
}

/// The instance's limits for custom emoji (`[limits]`), which admin clients check before upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CustomEmojiLimits {
  pub max_count: u32,
  pub max_bytes: u64,
  pub max_side_px: u32,
}

/// An emoji a message or reaction uses, inside its ciphertext: the name it had when sent and its
/// picture's hash. Receivers show the picture whose hash this is, when they hold it verified;
/// otherwise the `:shortcode:` text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
pub struct CustomEmojiRef {
  pub shortcode: String,
  #[schema(value_type = String)]
  pub sha256: CustomEmojiHash,
}

/// What a picture's header says, after [`inspect_custom_emoji_picture`] accepted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomEmojiPicture {
  pub format: CustomEmojiFormat,
  pub width: u32,
  pub height: u32,
}

/// Why a name or picture can't be a custom emoji.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CustomEmojiError {
  #[error("a shortcode has 2 to 32 lower-case letters, digits, '_' or '-'")]
  InvalidShortcode,
  #[error("the picture is empty or larger than the limit")]
  TooLarge,
  #[error("custom emoji are still PNG or WebP pictures")]
  UnsupportedFormat,
  #[error("animated pictures are not accepted yet")]
  Animated,
  #[error("the picture is wider or taller than the limit")]
  TooManyPixels,
  #[error("the picture's header can't be read")]
  Malformed,
  #[error("the picture doesn't match the signed hash or format")]
  Mismatch,
}

impl CustomEmojiHash {
  pub fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
  }

  pub fn as_bytes(&self) -> &[u8; 32] {
    &self.0
  }

  pub fn to_hex(&self) -> String {
    use fmt::Write as _;
    self.0.iter().fold(String::with_capacity(64), |mut out, byte| {
      let _ = write!(out, "{byte:02x}");
      out
    })
  }

  pub fn from_hex(text: &str) -> Option<Self> {
    if text.len() != 64 || !text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
      return None;
    }
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
      *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(Self(bytes))
  }
}

impl fmt::Debug for CustomEmojiHash {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "CustomEmojiHash({}…)", &self.to_hex()[..8])
  }
}

impl fmt::Display for CustomEmojiHash {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.to_hex())
  }
}

impl Serialize for CustomEmojiHash {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&self.to_hex())
  }
}

impl<'de> Deserialize<'de> for CustomEmojiHash {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let text = String::deserialize(deserializer)?;
    Self::from_hex(&text).ok_or_else(|| de::Error::custom("a hash is 64 lower-case hex characters"))
  }
}

impl Default for CustomEmojiLimits {
  fn default() -> Self {
    Self {
      max_count: DEFAULT_MAX_CUSTOM_EMOJI,
      max_bytes: DEFAULT_MAX_CUSTOM_EMOJI_BYTES,
      max_side_px: DEFAULT_MAX_CUSTOM_EMOJI_SIDE_PX,
    }
  }
}

impl CustomEmojiRef {
  /// The text that stands for it, and that older clients show: `:ship_it:`.
  pub fn text(&self) -> String {
    format!(":{}:", self.shortcode)
  }
}

/// Checks a shortcode (without colons): 2 to 32 of `a`–`z`, `0`–`9`, `_` and `-`. Lower case
/// only, so `:Ship_It:` and `:ship_it:` can't be two emoji that look alike.
pub fn check_custom_emoji_shortcode(shortcode: &str) -> Result<(), CustomEmojiError> {
  let is_valid = (MIN_CUSTOM_EMOJI_SHORTCODE_CHARS..=MAX_CUSTOM_EMOJI_SHORTCODE_CHARS)
    .contains(&shortcode.len())
    && shortcode
      .bytes()
      .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'));
  if is_valid {
    Ok(())
  } else {
    Err(CustomEmojiError::InvalidShortcode)
  }
}

/// Checks a picture before anything decodes it: at most `max_bytes`, a still PNG or WebP by its
/// header, neither side longer than `max_side_px`. Server and clients run the same check.
pub fn inspect_custom_emoji_picture(
  bytes: &[u8],
  max_bytes: u64,
  max_side_px: u32,
) -> Result<CustomEmojiPicture, CustomEmojiError> {
  if bytes.is_empty() || bytes.len() as u64 > max_bytes {
    return Err(CustomEmojiError::TooLarge);
  }
  let picture = if bytes.starts_with(&PNG_SIGNATURE) {
    inspect_png(bytes)?
  } else if bytes.len() >= 12 && bytes[..4] == *b"RIFF" && bytes[8..12] == *b"WEBP" {
    inspect_webp(bytes)?
  } else {
    return Err(CustomEmojiError::UnsupportedFormat);
  };
  if picture.width == 0 || picture.height == 0 {
    return Err(CustomEmojiError::Malformed);
  }
  if picture.width > max_side_px || picture.height > max_side_px {
    return Err(CustomEmojiError::TooManyPixels);
  }
  Ok(picture)
}

fn inspect_png(bytes: &[u8]) -> Result<CustomEmojiPicture, CustomEmojiError> {
  let header = bytes.get(..24).ok_or(CustomEmojiError::Malformed)?;
  // The image header's length is fixed; a false one could jump over what comes next.
  if header[8..12] != PNG_IHDR_LENGTH || header[12..16] != *b"IHDR" {
    return Err(CustomEmojiError::Malformed);
  }
  let side = |at: usize| u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]);
  let (width, height) = (side(16), side(20));
  // An animated PNG says so (`acTL`) before its first image data.
  let mut at = PNG_SIGNATURE.len();
  loop {
    let end = at.checked_add(8).ok_or(CustomEmojiError::Malformed)?;
    let chunk = bytes.get(at..end).ok_or(CustomEmojiError::Malformed)?;
    match &chunk[4..8] {
      b"acTL" => return Err(CustomEmojiError::Animated),
      b"IDAT" => break,
      _ => {},
    }
    let length = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    at = usize::try_from(length)
      .ok()
      .and_then(|length| at.checked_add(length)?.checked_add(12))
      .ok_or(CustomEmojiError::Malformed)?;
  }
  Ok(CustomEmojiPicture {
    format: CustomEmojiFormat::Png,
    width,
    height,
  })
}

/// A WebP file is one RIFF chunk: a lossy (`VP8 `), lossless (`VP8L`) or extended (`VP8X`)
/// picture. Only the extended header can say "animated".
fn inspect_webp(bytes: &[u8]) -> Result<CustomEmojiPicture, CustomEmojiError> {
  let chunk = bytes.get(12..16).ok_or(CustomEmojiError::Malformed)?;
  let byte = |at: usize| bytes.get(at).copied().ok_or(CustomEmojiError::Malformed);
  let (width, height) = match chunk {
    b"VP8 " => {
      // Frame tag (3 bytes) and start code 9d 01 2a, then 14-bit width and height.
      if [byte(23)?, byte(24)?, byte(25)?] != [0x9D, 0x01, 0x2A] {
        return Err(CustomEmojiError::Malformed);
      }
      let width = u32::from(u16::from_le_bytes([byte(26)?, byte(27)?]) & 0x3FFF);
      let height = u32::from(u16::from_le_bytes([byte(28)?, byte(29)?]) & 0x3FFF);
      (width, height)
    },
    b"VP8L" => {
      if byte(20)? != 0x2F {
        return Err(CustomEmojiError::Malformed);
      }
      let bits = u32::from_le_bytes([byte(21)?, byte(22)?, byte(23)?, byte(24)?]);
      ((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1)
    },
    b"VP8X" => {
      if byte(20)? & WEBP_ANIMATION_FLAG != 0 {
        return Err(CustomEmojiError::Animated);
      }
      let side = |at: usize| -> Result<u32, CustomEmojiError> {
        Ok(u32::from_le_bytes([byte(at)?, byte(at + 1)?, byte(at + 2)?, 0]) + 1)
      };
      (side(24)?, side(27)?)
    },
    _ => return Err(CustomEmojiError::Malformed),
  };
  Ok(CustomEmojiPicture {
    format: CustomEmojiFormat::Webp,
    width,
    height,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A PNG header of `width`×`height` followed by `extra` chunks and an image data chunk.
  fn png(width: u32, height: u32, extra: &[&[u8; 4]]) -> Vec<u8> {
    let mut bytes = PNG_SIGNATURE.to_vec();
    bytes.extend_from_slice(&PNG_IHDR_LENGTH);
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    for kind in extra {
      bytes.extend_from_slice(&[0, 0, 0, 0]);
      bytes.extend_from_slice(*kind);
      bytes.extend_from_slice(&[0, 0, 0, 0]);
    }
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes.extend_from_slice(b"IDAT");
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes
  }

  fn webp_extended(flags: u8, width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
    bytes.extend_from_slice(&[10, 0, 0, 0, flags, 0, 0, 0]);
    bytes.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
    bytes.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
    bytes
  }

  fn webp_lossless(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0".to_vec();
    bytes.push(0x2F);
    let bits = (width - 1) | ((height - 1) << 14);
    bytes.extend_from_slice(&bits.to_le_bytes());
    bytes
  }

  #[test]
  fn shortcodes_are_short_lower_case_words() {
    for valid in ["ok", "ship_it", "team-a", "a1", &"x".repeat(32)] {
      assert_eq!(check_custom_emoji_shortcode(valid), Ok(()), "{valid}");
    }
    for invalid in [
      "",
      "a",
      "Ship_it",
      "ship it",
      "ship:it",
      "schön",
      ":ok:",
      &"x".repeat(33),
    ] {
      assert_eq!(
        check_custom_emoji_shortcode(invalid),
        Err(CustomEmojiError::InvalidShortcode),
        "{invalid}"
      );
    }
  }

  #[test]
  fn still_png_within_the_limits_is_accepted() {
    let picture = inspect_custom_emoji_picture(&png(64, 48, &[b"tEXt"]), 1024, 128).unwrap();
    assert_eq!(picture.format, CustomEmojiFormat::Png);
    assert_eq!((picture.width, picture.height), (64, 48));
  }

  #[test]
  fn limits_and_formats_are_enforced() {
    let ok = png(64, 64, &[]);
    assert_eq!(
      inspect_custom_emoji_picture(&ok, ok.len() as u64 - 1, 128),
      Err(CustomEmojiError::TooLarge)
    );
    assert_eq!(
      inspect_custom_emoji_picture(&[], 1024, 128),
      Err(CustomEmojiError::TooLarge)
    );
    assert_eq!(
      inspect_custom_emoji_picture(&png(129, 64, &[]), 1024, 128),
      Err(CustomEmojiError::TooManyPixels)
    );
    assert_eq!(
      inspect_custom_emoji_picture(&png(64, 64, &[b"acTL"]), 1024, 128),
      Err(CustomEmojiError::Animated)
    );
    assert_eq!(
      inspect_custom_emoji_picture(b"GIF89a\x10\x00\x10\x00", 1024, 128),
      Err(CustomEmojiError::UnsupportedFormat)
    );
    assert_eq!(
      inspect_custom_emoji_picture(b"<svg xmlns='http://www.w3.org/2000/svg'/>", 1024, 128),
      Err(CustomEmojiError::UnsupportedFormat)
    );
    assert_eq!(
      inspect_custom_emoji_picture(&ok[..20], 1024, 128),
      Err(CustomEmojiError::Malformed)
    );
  }

  #[test]
  fn webp_sides_and_animation_are_read() {
    let still = inspect_custom_emoji_picture(&webp_extended(0x10, 100, 90), 1024, 128).unwrap();
    assert_eq!(
      (still.format, still.width, still.height),
      (CustomEmojiFormat::Webp, 100, 90)
    );
    assert_eq!(
      inspect_custom_emoji_picture(&webp_extended(WEBP_ANIMATION_FLAG, 64, 64), 1024, 128),
      Err(CustomEmojiError::Animated)
    );
    let lossless = inspect_custom_emoji_picture(&webp_lossless(128, 7), 1024, 128).unwrap();
    assert_eq!((lossless.width, lossless.height), (128, 7));
    assert_eq!(
      inspect_custom_emoji_picture(&webp_lossless(300, 7), 1024, 128),
      Err(CustomEmojiError::TooManyPixels)
    );
  }

  #[test]
  fn hash_round_trips_as_hex() {
    let hash = CustomEmojiHash::from_bytes([0xAB; 32]);
    let json = serde_json::to_string(&hash).unwrap();
    assert_eq!(json, format!("\"{}\"", "ab".repeat(32)));
    assert_eq!(serde_json::from_str::<CustomEmojiHash>(&json).unwrap(), hash);
    assert!(serde_json::from_str::<CustomEmojiHash>("\"AB\"").is_err());
    assert!(CustomEmojiHash::from_hex(&"AB".repeat(32)).is_none());
  }
}
