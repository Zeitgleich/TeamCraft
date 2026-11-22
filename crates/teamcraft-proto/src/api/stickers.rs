//! Sticker packs of the workspace (issue #2132, e2ee.md §4.14, admin.md §3.11): named, ordered sets
//! of pictures that admins put together and everybody sends as a message.
//!
//! They reuse the custom emoji model (§4.13): the pictures are workspace assets, stored unencrypted
//! and keyed by their SHA-256, and what makes them trustworthy is a signature. An admin's device
//! signs the whole pack as one [`StickerPackDocument`] (its name and, per sticker, the name, alt
//! text and the picture's hash), and clients use a pack only when that signature verifies against
//! the signed admin roster; a picture only when its bytes match the hash. A rename, reorder or
//! removal is a new version of the document. A sent sticker names its picture inside the
//! ciphertext ([`StickerRef`]); the message's text is a fallback that older apps and push previews
//! show.

use std::collections::HashSet;
use std::fmt;

use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
  Base64Bytes, CustomEmojiFormat, CustomEmojiHash, UnixMillis, UserId, check_custom_emoji_shortcode,
};

/// Default of `[limits] max_sticker_packs`: packs a workspace may have.
pub const DEFAULT_MAX_STICKER_PACKS: u32 = 50;
/// Default of `[limits] max_stickers_per_pack`.
pub const DEFAULT_MAX_STICKERS_PER_PACK: u32 = 120;
/// Default of `[limits] max_sticker_bytes`: the largest picture.
pub const DEFAULT_MAX_STICKER_BYTES: u64 = 256 * 1024;
/// Default of `[limits] max_sticker_side_px`: the longest side of a picture. Stickers show at about
/// 160 points, twice that on sharp screens.
pub const DEFAULT_MAX_STICKER_SIDE_PX: u32 = 512;
/// The highest values the settings accept: what any client still downloads for all packs.
pub const MAX_STICKER_PACKS_LIMIT: u32 = 500;
pub const MAX_STICKERS_PER_PACK_LIMIT: u32 = 500;
pub const MAX_STICKER_BYTES_LIMIT: u64 = 1024 * 1024;
pub const MAX_STICKER_SIDE_PX_LIMIT: u32 = 1024;
/// Longest pack name, in characters.
pub const MAX_STICKER_PACK_NAME_CHARS: usize = 64;
/// Longest alt text of a sticker, in characters.
pub const MAX_STICKER_ALT_CHARS: usize = 200;

/// A pack's identity, chosen at random by the admin's device that creates it (16 bytes), so the
/// signed document names its pack before the server has seen it. Wire format: 32 lowercase hex
/// characters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StickerPackKey([u8; 16]);

/// What an admin's device signs for a pack (JSON, the payload of [`SignedStickerPack`]): the whole
/// pack, so a server can't mix stickers of two versions. Every change is a new document with a
/// higher `version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StickerPackDocument {
  #[schema(value_type = String)]
  pub pack: StickerPackKey,
  /// 1 for a new pack, one more for each change.
  pub version: u64,
  pub name: String,
  /// In the order the picker shows them.
  pub stickers: Vec<StickerEntry>,
  /// When the admin's device signed it (its own clock), for display and ordering. Judges the
  /// signer's authority only in a document without `roster_version` (e2ee.md §4.13).
  pub signed_at_ms: i64,
  /// The version of the verified roster the signing device held: the signer must be an owner or
  /// admin in it, and it must come before any roster that demoted them. Signing devices always
  /// name it; `None` in documents from before (issue #2290).
  #[serde(rename = "roster_version", default, skip_serializing_if = "Option::is_none")]
  pub maybe_roster_version: Option<u64>,
}

/// One sticker of a pack's document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StickerEntry {
  /// Unique within the pack, like an emoji's shortcode: `thumbs_up`.
  pub name: String,
  /// What a screen reader says and the fallback text of a sent sticker shows; required.
  pub alt: String,
  /// SHA-256 of the picture exactly as uploaded and served.
  #[schema(value_type = String)]
  pub sha256: CustomEmojiHash,
  pub format: CustomEmojiFormat,
}

/// A [`StickerPackDocument`] as JSON, verbatim as signed, and the signature: a signed log entry of
/// the crypto crate's `WorkspaceLog::StickerPack` by a device of a workspace admin or owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SignedStickerPack {
  pub payload: Base64Bytes,
  pub signature: Base64Bytes,
}

/// `PUT /api/v1/sticker-packs/{pack}` (admins): creates the pack or replaces it with a newer
/// version. Every picture it names must be uploaded first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SaveStickerPackRequest {
  pub signed: SignedStickerPack,
}

/// `POST /api/v1/sticker-pictures` (admins): uploads a picture for a pack, keyed by its hash.
/// Uploading a picture the server holds already changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadStickerPictureRequest {
  /// A still PNG or WebP within the sticker limits.
  pub picture: Base64Bytes,
}

/// A picture the server holds for packs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StickerPictureResponse {
  #[schema(value_type = String)]
  pub sha256: CustomEmojiHash,
  pub format: CustomEmojiFormat,
  pub size_bytes: u64,
  pub width: u32,
  pub height: u32,
}

/// One pack as the server lists it. `name` and `version` repeat the signed document for display;
/// clients believe only the document after verifying its signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StickerPackResponse {
  #[schema(value_type = String)]
  pub pack: StickerPackKey,
  pub version: u64,
  pub name: String,
  /// Who saved its current version; absent once their account is deleted.
  #[serde(rename = "updated_by", default, skip_serializing_if = "Option::is_none")]
  pub maybe_updated_by: Option<UserId>,
  pub updated_at: UnixMillis,
  pub signed: SignedStickerPack,
}

/// `GET /api/v1/sticker-packs`: the workspace's packs and the limits for new ones (everybody signed
/// in, guests included: they read the messages that use them).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListStickerPacksResponse {
  pub packs: Vec<StickerPackResponse>,
  pub limits: StickerLimits,
}

/// `GET /api/v1/sticker-pictures/{sha256}/download`: where to fetch a picture. Clients check its
/// hash before showing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StickerPictureDownloadResponse {
  /// Presigned `GET` URL of the picture.
  pub download_url: String,
  pub size_bytes: u64,
  pub expires_at: UnixMillis,
}

/// The instance's limits for sticker packs (`[limits]`), which admin clients check before saving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StickerLimits {
  pub max_packs: u32,
  pub max_stickers_per_pack: u32,
  pub max_bytes: u64,
  pub max_side_px: u32,
}

/// A sent sticker, inside the message's ciphertext: the pack and name it had when sent, its alt
/// text and its picture's hash. Receivers show the picture whose hash this is, when they hold it
/// verified; otherwise the message's fallback text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub struct StickerRef {
  #[schema(value_type = String)]
  pub pack: StickerPackKey,
  pub name: String,
  pub alt: String,
  #[schema(value_type = String)]
  pub sha256: CustomEmojiHash,
}

/// The workspace's packs whose signatures this device verified, and the limits for new ones.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StickerSet {
  /// In the server's order (oldest pack first).
  pub packs: Vec<VerifiedStickerPack>,
  #[serde(default)]
  pub limits: StickerLimits,
}

/// One pack whose document verified.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedStickerPack {
  pub pack: StickerPackKey,
  pub version: u64,
  pub name: String,
  pub stickers: Vec<StickerEntry>,
  /// The admin whose device signed this version.
  pub signed_by: UserId,
  /// When it was signed (the signer's clock), Unix milliseconds.
  pub signed_at: i64,
}

/// A change an admin makes to one pack, applied by their device to the newest version it verified
/// and signed as the next one. Adding a sticker is its own call: it uploads the picture first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum StickerPackChange {
  Rename {
    name: String,
  },
  /// A new name and alt text for the sticker named `name`; the picture stays.
  Edit {
    name: String,
    new_name: String,
    alt: String,
  },
  Remove {
    name: String,
  },
  /// Moves the sticker named `name` to position `to` (clamped).
  Move {
    name: String,
    to: usize,
  },
}

/// Why a pack's document can't be saved or used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StickerPackError {
  #[error("a pack has a name of 1 to 64 characters")]
  InvalidPackName,
  #[error("a sticker's name has 2 to 32 lower-case letters, digits, '_' or '-'")]
  InvalidStickerName,
  #[error("two stickers of the pack have the same name")]
  DuplicateStickerName,
  #[error("a sticker has an alt text of 1 to 200 characters")]
  InvalidAlt,
  #[error("the pack has more stickers than the limit")]
  TooManyStickers,
  #[error("stickers are still PNG or WebP pictures")]
  UnsupportedFormat,
  #[error("a pack's version starts at 1")]
  InvalidVersion,
}

impl StickerPackKey {
  pub const fn from_bytes(bytes: [u8; 16]) -> Self {
    Self(bytes)
  }

  pub fn as_bytes(&self) -> &[u8; 16] {
    &self.0
  }

  pub fn to_hex(&self) -> String {
    use fmt::Write as _;
    self.0.iter().fold(String::with_capacity(32), |mut out, byte| {
      let _ = write!(out, "{byte:02x}");
      out
    })
  }

  pub fn from_hex(text: &str) -> Option<Self> {
    if text.len() != 32 || !text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
      return None;
    }
    let mut bytes = [0_u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
      *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(Self(bytes))
  }
}

impl fmt::Debug for StickerPackKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "StickerPackKey({})", self.to_hex())
  }
}

impl fmt::Display for StickerPackKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.to_hex())
  }
}

impl Serialize for StickerPackKey {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&self.to_hex())
  }
}

impl<'de> Deserialize<'de> for StickerPackKey {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let text = String::deserialize(deserializer)?;
    Self::from_hex(&text).ok_or_else(|| de::Error::custom("a pack key is 32 lower-case hex characters"))
  }
}

impl Default for StickerLimits {
  fn default() -> Self {
    Self {
      max_packs: DEFAULT_MAX_STICKER_PACKS,
      max_stickers_per_pack: DEFAULT_MAX_STICKERS_PER_PACK,
      max_bytes: DEFAULT_MAX_STICKER_BYTES,
      max_side_px: DEFAULT_MAX_STICKER_SIDE_PX,
    }
  }
}

impl StickerPackDocument {
  /// Checks the document's own rules: a name, valid and unique sticker names, alt texts, still
  /// formats and at most `max_stickers` stickers. Server and clients run the same check; clients
  /// pass [`MAX_STICKERS_PER_PACK_LIMIT`], since a pack an older limit allowed stays valid.
  pub fn check(&self, max_stickers: u32) -> Result<(), StickerPackError> {
    if self.version == 0 {
      return Err(StickerPackError::InvalidVersion);
    }
    check_sticker_pack_name(&self.name)?;
    if self.stickers.len() > max_stickers.min(MAX_STICKERS_PER_PACK_LIMIT) as usize {
      return Err(StickerPackError::TooManyStickers);
    }
    let mut names = HashSet::new();
    for sticker in &self.stickers {
      check_custom_emoji_shortcode(&sticker.name).map_err(|_| StickerPackError::InvalidStickerName)?;
      if !names.insert(sticker.name.as_str()) {
        return Err(StickerPackError::DuplicateStickerName);
      }
      check_sticker_alt(&sticker.alt)?;
      if !matches!(sticker.format, CustomEmojiFormat::Png | CustomEmojiFormat::Webp) {
        return Err(StickerPackError::UnsupportedFormat);
      }
    }
    Ok(())
  }
}

impl StickerSet {
  pub fn pack(&self, pack: StickerPackKey) -> Option<&VerifiedStickerPack> {
    self.packs.iter().find(|known| known.pack == pack)
  }

  /// Whether a picture belongs to a sticker of the set.
  pub fn has_picture(&self, sha256: CustomEmojiHash) -> bool {
    self
      .packs
      .iter()
      .any(|pack| pack.stickers.iter().any(|sticker| sticker.sha256 == sha256))
  }
}

impl VerifiedStickerPack {
  /// The document this pack's next version starts from.
  pub fn document(&self) -> StickerPackDocument {
    StickerPackDocument {
      pack: self.pack,
      version: self.version,
      name: self.name.clone(),
      stickers: self.stickers.clone(),
      signed_at_ms: self.signed_at,
      maybe_roster_version: None,
    }
  }
}

impl StickerEntry {
  /// What a message sending this sticker carries.
  pub fn reference(&self, pack: StickerPackKey) -> StickerRef {
    StickerRef {
      pack,
      name: self.name.clone(),
      alt: self.alt.clone(),
      sha256: self.sha256,
    }
  }
}

/// A pack's name: 1 to [`MAX_STICKER_PACK_NAME_CHARS`] characters, no control characters, not just
/// spaces.
pub fn check_sticker_pack_name(name: &str) -> Result<(), StickerPackError> {
  let chars = name.chars().count();
  let is_valid =
    !name.trim().is_empty() && chars <= MAX_STICKER_PACK_NAME_CHARS && !name.chars().any(char::is_control);
  is_valid.then_some(()).ok_or(StickerPackError::InvalidPackName)
}

/// A sticker's alt text: 1 to [`MAX_STICKER_ALT_CHARS`] characters, no control characters, not
/// just spaces.
pub fn check_sticker_alt(alt: &str) -> Result<(), StickerPackError> {
  let chars = alt.chars().count();
  let is_valid =
    !alt.trim().is_empty() && chars <= MAX_STICKER_ALT_CHARS && !alt.chars().any(char::is_control);
  is_valid.then_some(()).ok_or(StickerPackError::InvalidAlt)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn document(stickers: &[(&str, &str)]) -> StickerPackDocument {
    StickerPackDocument {
      pack: StickerPackKey::from_bytes([7; 16]),
      version: 1,
      name: "Team".into(),
      stickers: stickers
        .iter()
        .enumerate()
        .map(|(index, (name, alt))| StickerEntry {
          name: (*name).into(),
          alt: (*alt).into(),
          sha256: CustomEmojiHash::from_bytes([index as u8; 32]),
          format: CustomEmojiFormat::Png,
        })
        .collect(),
      signed_at_ms: 1,
      maybe_roster_version: None,
    }
  }

  #[test]
  fn a_valid_pack_passes() {
    assert_eq!(
      document(&[("wave", "Waving hand"), ("ok", "Thumbs up")]).check(10),
      Ok(())
    );
    assert_eq!(document(&[]).check(10), Ok(()));
  }

  #[test]
  fn names_alt_texts_and_counts_are_checked() {
    assert_eq!(
      document(&[("wave", "a"), ("wave", "b")]).check(10),
      Err(StickerPackError::DuplicateStickerName)
    );
    assert_eq!(
      document(&[("Wave", "a")]).check(10),
      Err(StickerPackError::InvalidStickerName)
    );
    assert_eq!(
      document(&[("wave", " ")]).check(10),
      Err(StickerPackError::InvalidAlt)
    );
    assert_eq!(
      document(&[("wave", &"x".repeat(201))]).check(10),
      Err(StickerPackError::InvalidAlt)
    );
    assert_eq!(
      document(&[("wave", "a"), ("ok", "b")]).check(1),
      Err(StickerPackError::TooManyStickers)
    );
    let mut nameless = document(&[]);
    nameless.name = "  ".into();
    assert_eq!(nameless.check(10), Err(StickerPackError::InvalidPackName));
    nameless.name = "a\nb".into();
    assert_eq!(nameless.check(10), Err(StickerPackError::InvalidPackName));
    let mut unversioned = document(&[]);
    unversioned.version = 0;
    assert_eq!(unversioned.check(10), Err(StickerPackError::InvalidVersion));
  }

  #[test]
  fn an_unknown_format_is_refused() {
    let mut pack = document(&[("wave", "a")]);
    pack.stickers[0].format = serde_json::from_str("\"image/gif\"").unwrap();
    assert_eq!(pack.check(10), Err(StickerPackError::UnsupportedFormat));
  }

  #[test]
  fn pack_key_round_trips_as_hex() {
    let key = StickerPackKey::from_bytes([0xAB; 16]);
    let json = serde_json::to_string(&key).unwrap();
    assert_eq!(json, format!("\"{}\"", "ab".repeat(16)));
    assert_eq!(serde_json::from_str::<StickerPackKey>(&json).unwrap(), key);
    assert!(serde_json::from_str::<StickerPackKey>("\"ab\"").is_err());
    assert!(StickerPackKey::from_hex(&"AB".repeat(16)).is_none());
  }

  #[test]
  fn sticker_ref_wire_format_is_stable() {
    let reference =
      document(&[("wave", "Waving hand")]).stickers[0].reference(StickerPackKey::from_bytes([1; 16]));
    let json = serde_json::to_value(&reference).unwrap();
    assert_eq!(
      json,
      serde_json::json!({
        "pack": "01".repeat(16),
        "name": "wave",
        "alt": "Waving hand",
        "sha256": "00".repeat(32),
      })
    );
  }
}
