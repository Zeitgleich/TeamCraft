//! Custom emoji of the workspace in the apps (issue #207): the set and text helpers come from
//! `teamcraft-proto` (the core verifies with them too); this adds what needs the catalog of
//! built-in emoji and the admin console's texts.

pub use teamcraft_proto::custom_emoji_set::{
  CustomEmoji, CustomEmojiSet, EmojiSegment, NewShortcodeError, jumbo_custom_count, segments, token_shortcode,
};
use teamcraft_proto::{CustomEmojiError, CustomEmojiId};

use crate::emoji_catalog;

/// Whether `shortcode` can name a new emoji (or be the new name of `maybe_renamed`): valid, no
/// built-in emoji's (the composer would turn it into that one) and not another custom emoji's.
pub fn check_new_shortcode(
  set: &CustomEmojiSet,
  shortcode: &str,
  maybe_renamed: Option<CustomEmojiId>,
) -> Result<(), NewShortcodeError> {
  set.check_new_shortcode(shortcode, maybe_renamed)?;
  if emoji_catalog::by_shortcode(shortcode).is_some() {
    return Err(NewShortcodeError::BuiltIn);
  }
  Ok(())
}

/// Why a picture can't be an emoji, as the admin console says it.
pub fn picture_error_key(error: CustomEmojiError) -> &'static str {
  match error {
    CustomEmojiError::TooLarge => "admin-emoji-error-too-large",
    CustomEmojiError::TooManyPixels => "admin-emoji-error-too-wide",
    CustomEmojiError::Animated => "admin-emoji-error-animated",
    CustomEmojiError::InvalidShortcode => "admin-emoji-error-name",
    CustomEmojiError::UnsupportedFormat | CustomEmojiError::Malformed | CustomEmojiError::Mismatch => {
      "admin-emoji-error-format"
    },
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{CustomEmojiFormat, CustomEmojiHash, CustomEmojiLimits};

  use super::*;
  use crate::model::id_from_raw as id;

  #[test]
  fn built_in_shortcodes_are_refused_too() {
    let set = CustomEmojiSet {
      emoji: vec![CustomEmoji {
        id: id(1),
        shortcode: "ship_it".into(),
        sha256: CustomEmojiHash::from_bytes([1; 32]),
        format: CustomEmojiFormat::Png,
        signed_by: id(2),
        signed_at: 0,
      }],
      limits: CustomEmojiLimits::default(),
    };
    assert_eq!(check_new_shortcode(&set, "deploy", None), Ok(()));
    assert_eq!(
      check_new_shortcode(&set, "smile", None),
      Err(NewShortcodeError::BuiltIn)
    );
    assert_eq!(
      check_new_shortcode(&set, "ship_it", None),
      Err(NewShortcodeError::Taken)
    );
    assert_eq!(check_new_shortcode(&set, "ship_it", Some(id(1))), Ok(()));
  }
}
