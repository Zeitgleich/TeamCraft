//! Sticker packs for the phone apps (issue #2132): the packs the core verified, shaped for the
//! sticker sheet. The core checks signatures and pictures; these records only carry what it kept.

use teamcraft_proto::{StickerPackKey, StickerRef, StickerSet, VerifiedStickerPack};

/// One verified pack, in the order of the sticker sheet.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct StickerPackItem {
  /// 32 lower-case hex characters; what [`crate::TeamCraft::send_sticker`] takes.
  pub pack: String,
  pub name: String,
  pub stickers: Vec<StickerItem>,
}

/// One sticker of a pack.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct StickerItem {
  pub name: String,
  /// The accessible name, and what the fallback text of a sent sticker shows.
  pub alt: String,
  /// 64 lower-case hex characters; the key of [`crate::TeamCraft::sticker_picture`].
  pub sha256: String,
}

impl From<&VerifiedStickerPack> for StickerPackItem {
  fn from(pack: &VerifiedStickerPack) -> Self {
    Self {
      pack: pack.pack.to_hex(),
      name: pack.name.clone(),
      stickers: pack
        .stickers
        .iter()
        .map(|sticker| StickerItem {
          name: sticker.name.clone(),
          alt: sticker.alt.clone(),
          sha256: sticker.sha256.to_hex(),
        })
        .collect(),
    }
  }
}

pub(crate) fn items(set: &StickerSet) -> Vec<StickerPackItem> {
  set.packs.iter().map(StickerPackItem::from).collect()
}

/// The sticker `name` of `pack` in the verified set, as a message names it.
pub(crate) fn reference(set: &StickerSet, pack: &str, name: &str) -> Option<StickerRef> {
  let pack = StickerPackKey::from_hex(pack)?;
  set
    .pack(pack)?
    .stickers
    .iter()
    .find(|sticker| sticker.name == name)
    .map(|sticker| sticker.reference(pack))
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::model::id_from_raw;
  use teamcraft_proto::{CustomEmojiFormat, CustomEmojiHash, StickerEntry};

  use super::*;

  #[test]
  fn a_known_sticker_is_found_and_others_are_not() {
    let key = StickerPackKey::from_bytes([5; 16]);
    let set = StickerSet {
      packs: vec![VerifiedStickerPack {
        pack: key,
        version: 1,
        name: "Team".into(),
        stickers: vec![StickerEntry {
          name: "wave".into(),
          alt: "Waving hand".into(),
          sha256: CustomEmojiHash::from_bytes([1; 32]),
          format: CustomEmojiFormat::Png,
        }],
        signed_by: id_from_raw(1),
        signed_at: 0,
      }],
      limits: Default::default(),
    };
    let items = items(&set);
    assert_eq!(items[0].pack, key.to_hex());
    assert_eq!(items[0].stickers[0].sha256, "01".repeat(32));
    let found = reference(&set, &key.to_hex(), "wave").unwrap();
    assert_eq!(found.alt, "Waving hand");
    assert!(reference(&set, &key.to_hex(), "other").is_none());
    assert!(reference(&set, "nothex", "wave").is_none());
  }
}
