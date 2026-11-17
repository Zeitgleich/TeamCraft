//! Custom emoji for the phone apps (issue #207): the verified set, the pictures the core holds and
//! the text rules of `teamcraft-client-api`, so Android and iOS split messages, find names for the
//! `:` suggestions and recognise reactions as web and desktop do. The core checks signatures and
//! pictures; these functions only shape what it kept.

use teamcraft_client_api::custom_emoji::{
  CustomEmoji, CustomEmojiSet, EmojiSegment, segments, token_shortcode,
};
use teamcraft_proto::{CustomEmojiHash, CustomEmojiRef};

/// One emoji of the workspace set: its name and the hash that names its picture.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CustomEmojiItem {
  /// Without colons: `ship_it`.
  pub shortcode: String,
  /// 64 lower-case hex characters; the key of [`crate::TeamCraft::custom_emoji_picture`].
  pub sha256: String,
}

/// An emoji a message or reaction names, as its JSON carries it (`custom_emoji`, `custom`).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CustomEmojiRefItem {
  pub shortcode: String,
  pub sha256: String,
}

/// One piece of a message text: plain text, or a custom emoji to draw as its picture.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum EmojiSegmentItem {
  Text { text: String },
  Emoji { shortcode: String, sha256: String },
}

impl From<&CustomEmoji> for CustomEmojiItem {
  fn from(emoji: &CustomEmoji) -> Self {
    Self {
      shortcode: emoji.shortcode.clone(),
      sha256: emoji.sha256.to_hex(),
    }
  }
}

/// The emoji of `set` whose names match what was typed after `:`, best first; all of them (by
/// name) for an empty query. At most `limit`.
pub(crate) fn matching(set: &CustomEmojiSet, query: &str, limit: usize) -> Vec<CustomEmojiItem> {
  let mut found: Vec<&CustomEmoji> = if query.is_empty() {
    let mut all: Vec<&CustomEmoji> = set.emoji.iter().collect();
    all.sort_by(|left, right| left.shortcode.cmp(&right.shortcode));
    all
  } else {
    set.search(query)
  };
  found.truncate(limit);
  found.into_iter().map(CustomEmojiItem::from).collect()
}

/// Splits a message text at the `:shortcode:` words of `refs`, for drawing those as pictures. A
/// reference with a malformed hash is left out, so its words stay text.
#[uniffi::export]
pub fn custom_emoji_segments(text: String, refs: Vec<CustomEmojiRefItem>) -> Vec<EmojiSegmentItem> {
  let refs = refs_from(refs);
  segments(&text, &refs)
    .into_iter()
    .map(|segment| match segment {
      EmojiSegment::Text(text) => EmojiSegmentItem::Text {
        text: text.to_owned(),
      },
      EmojiSegment::Emoji(reference) => EmojiSegmentItem::Emoji {
        shortcode: reference.shortcode.clone(),
        sha256: reference.sha256.to_hex(),
      },
    })
    .collect()
}

/// The shortcode (without colons) if `token` is a whole `:shortcode:`: a reaction of a workspace
/// emoji, or one picked to react with.
#[uniffi::export]
pub fn custom_emoji_token_shortcode(token: String) -> Option<String> {
  token_shortcode(&token).map(str::to_owned)
}

fn refs_from(items: Vec<CustomEmojiRefItem>) -> Vec<CustomEmojiRef> {
  items
    .into_iter()
    .filter_map(|item| {
      Some(CustomEmojiRef {
        shortcode: item.shortcode,
        sha256: CustomEmojiHash::from_hex(&item.sha256)?,
      })
    })
    .collect()
}

/// A picture's hash from its hex form, if it is one.
pub(crate) fn parse_hash(hex: &str) -> Option<CustomEmojiHash> {
  CustomEmojiHash::from_hex(hex)
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::custom_emoji::CustomEmojiSet;
  use teamcraft_proto::{CustomEmojiFormat, CustomEmojiLimits};

  use super::*;

  const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

  #[test]
  fn a_known_name_becomes_a_picture_between_its_text() {
    let pieces = custom_emoji_segments("go :ship_it: now".into(), vec![reference("ship_it", HASH_A)]);
    assert_eq!(
      pieces,
      vec![
        EmojiSegmentItem::Text { text: "go ".into() },
        EmojiSegmentItem::Emoji {
          shortcode: "ship_it".into(),
          sha256: HASH_A.into()
        },
        EmojiSegmentItem::Text { text: " now".into() },
      ]
    );
  }

  #[test]
  fn text_without_a_reference_stays_one_piece() {
    let pieces = custom_emoji_segments("a :ship_it: b".into(), Vec::new());
    assert_eq!(
      pieces,
      vec![EmojiSegmentItem::Text {
        text: "a :ship_it: b".into()
      }]
    );
  }

  #[test]
  fn a_malformed_hash_leaves_the_words_as_text() {
    let pieces = custom_emoji_segments(":ship_it:".into(), vec![reference("ship_it", "xyz")]);
    assert_eq!(
      pieces,
      vec![EmojiSegmentItem::Text {
        text: ":ship_it:".into()
      }]
    );
  }

  #[test]
  fn only_a_whole_token_is_a_reaction_name() {
    assert_eq!(
      custom_emoji_token_shortcode(":ship_it:".into()),
      Some("ship_it".into())
    );
    assert_eq!(custom_emoji_token_shortcode("x :ship_it:".into()), None);
    assert_eq!(custom_emoji_token_shortcode("👍".into()), None);
  }

  #[test]
  fn matching_puts_names_that_start_with_the_query_first() {
    let set = set_of(&["big_ship", "ship_it", "ship"]);
    let names: Vec<String> = matching(&set, "ship", 10)
      .into_iter()
      .map(|item| item.shortcode)
      .collect();
    assert_eq!(names, vec!["ship", "ship_it", "big_ship"]);
  }

  #[test]
  fn an_empty_query_lists_by_name_up_to_the_limit() {
    let set = set_of(&["b_one", "a_one", "c_one"]);
    let names: Vec<String> = matching(&set, "", 2)
      .into_iter()
      .map(|item| item.shortcode)
      .collect();
    assert_eq!(names, vec!["a_one", "b_one"]);
  }

  fn reference(shortcode: &str, sha256: &str) -> CustomEmojiRefItem {
    CustomEmojiRefItem {
      shortcode: shortcode.into(),
      sha256: sha256.into(),
    }
  }

  fn set_of(names: &[&str]) -> CustomEmojiSet {
    use teamcraft_client_api::model::id_from_raw;
    CustomEmojiSet {
      emoji: names
        .iter()
        .enumerate()
        .map(|(index, name)| CustomEmoji {
          id: id_from_raw(index as u64 + 1),
          shortcode: (*name).into(),
          sha256: CustomEmojiHash::from_bytes([1; 32]),
          format: CustomEmojiFormat::Png,
          signed_by: id_from_raw(9),
          signed_at: 0,
        })
        .collect(),
      limits: CustomEmojiLimits::default(),
    }
  }
}
