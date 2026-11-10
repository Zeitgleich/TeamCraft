//! Custom emoji of the workspace as the apps use them (issue #207): the set a device verified, the
//! `:shortcode:` words of a text that name one, and the checks of a new name that need no catalog
//! of built-in emoji (the apps add that one, `teamcraft-client-api`).
//!
//! A message keeps the `:shortcode:` in its text, which is what older apps and apps without the
//! picture show; the emoji it names travel inside the ciphertext as [`CustomEmojiRef`]s, so a
//! later rename or deletion doesn't change what an earlier message showed.

use serde::{Deserialize, Serialize};

use crate::{
  CustomEmojiFormat, CustomEmojiHash, CustomEmojiId, CustomEmojiLimits, CustomEmojiRef,
  MAX_CUSTOM_EMOJI_PER_MESSAGE, UserId, check_custom_emoji_shortcode,
};

/// The workspace's emoji whose signatures this device verified, and the limits for new ones.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomEmojiSet {
  /// By shortcode.
  pub emoji: Vec<CustomEmoji>,
  #[serde(default)]
  pub limits: CustomEmojiLimits,
}

/// One verified emoji.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomEmoji {
  pub id: CustomEmojiId,
  pub shortcode: String,
  pub sha256: CustomEmojiHash,
  pub format: CustomEmojiFormat,
  /// The admin whose device signed its current name.
  pub signed_by: UserId,
  /// When it was signed (the signer's clock), Unix milliseconds.
  pub signed_at: i64,
}

/// A piece of a text: as written, or a custom emoji to show as its picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmojiSegment<'a> {
  Text(&'a str),
  Emoji(&'a CustomEmojiRef),
}

/// Why a name can't be given to a new emoji, as the admin console says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewShortcodeError {
  /// Not 2 to 32 of `a`–`z`, `0`–`9`, `_`, `-`.
  Invalid,
  /// A built-in emoji has it (`:smile:`): the composer turns it into that emoji. Only the apps,
  /// which know the built-in emoji, check this.
  BuiltIn,
  /// Another custom emoji has it.
  Taken,
}

impl CustomEmojiSet {
  pub fn by_shortcode(&self, shortcode: &str) -> Option<&CustomEmoji> {
    self.emoji.iter().find(|emoji| emoji.shortcode == shortcode)
  }

  pub fn by_hash(&self, sha256: CustomEmojiHash) -> Option<&CustomEmoji> {
    self.emoji.iter().find(|emoji| emoji.sha256 == sha256)
  }

  /// Emoji whose shortcode contains `query` (any case), those starting with it first, then by name.
  pub fn search(&self, query: &str) -> Vec<&CustomEmoji> {
    let query = query.trim().trim_matches(':').to_lowercase();
    let mut found: Vec<&CustomEmoji> = self
      .emoji
      .iter()
      .filter(|emoji| emoji.shortcode.contains(&query))
      .collect();
    found.sort_by_key(|emoji| (!emoji.shortcode.starts_with(&query), emoji.shortcode.clone()));
    found
  }

  /// The emoji a text names by `:shortcode:`, each once, in order, outside code; at most
  /// [`MAX_CUSTOM_EMOJI_PER_MESSAGE`]. What a sent message carries inside its ciphertext.
  pub fn refs_in(&self, text: &str) -> Vec<CustomEmojiRef> {
    let mut refs: Vec<CustomEmojiRef> = Vec::new();
    if self.emoji.is_empty() || !text.contains(':') {
      return refs;
    }
    for shortcode in shortcodes_in(text) {
      if refs.len() >= MAX_CUSTOM_EMOJI_PER_MESSAGE {
        break;
      }
      if let Some(emoji) = self.by_shortcode(shortcode)
        && !refs.iter().any(|known| known.shortcode == shortcode)
      {
        refs.push(emoji.reference());
      }
    }
    refs
  }

  /// Whether `shortcode` can name a new emoji (or be the new name of `maybe_renamed`): valid and
  /// not another emoji's. Built-in emoji are the apps' to check.
  pub fn check_new_shortcode(
    &self,
    shortcode: &str,
    maybe_renamed: Option<CustomEmojiId>,
  ) -> Result<(), NewShortcodeError> {
    check_custom_emoji_shortcode(shortcode).map_err(|_| NewShortcodeError::Invalid)?;
    match self.by_shortcode(shortcode) {
      Some(other) if Some(other.id) != maybe_renamed => Err(NewShortcodeError::Taken),
      _ => Ok(()),
    }
  }
}

impl CustomEmoji {
  pub fn reference(&self) -> CustomEmojiRef {
    CustomEmojiRef {
      shortcode: self.shortcode.clone(),
      sha256: self.sha256,
    }
  }

  /// `:ship_it:`
  pub fn text(&self) -> String {
    format!(":{}:", self.shortcode)
  }
}

/// The shortcode of a whole `:shortcode:` token (a picked emoji, a reaction), if it is one.
pub fn token_shortcode(token: &str) -> Option<&str> {
  let shortcode = token.strip_prefix(':')?.strip_suffix(':')?;
  check_custom_emoji_shortcode(shortcode).ok()?;
  Some(shortcode)
}

/// Splits `text` at the `:shortcode:` words of `refs`, for showing those as pictures. Text without
/// any stays one piece.
pub fn segments<'a>(text: &'a str, refs: &'a [CustomEmojiRef]) -> Vec<EmojiSegment<'a>> {
  if refs.is_empty() || !text.contains(':') {
    return vec![EmojiSegment::Text(text)];
  }
  let mut out = Vec::new();
  let (mut text_start, mut from) = (0, 0);
  while let Some(token) = token_at(text, from) {
    match refs
      .iter()
      .find(|reference| reference.shortcode == token.shortcode)
    {
      Some(reference) => {
        if token.open > text_start {
          out.push(EmojiSegment::Text(&text[text_start..token.open]));
        }
        out.push(EmojiSegment::Emoji(reference));
        text_start = token.end;
        from = token.end;
      },
      // Not one of ours: its second colon may open the next token.
      None => from = token.end - 1,
    }
  }
  if text_start < text.len() {
    out.push(EmojiSegment::Text(&text[text_start..]));
  }
  out
}

/// How many custom emoji a text of nothing else names (1 to 3, by whitespace-separated words):
/// such a message shows them larger, like the apps do for 1 to 3 emoji.
pub fn jumbo_custom_count(text: &str, refs: &[CustomEmojiRef]) -> Option<usize> {
  if refs.is_empty() {
    return None;
  }
  let words: Vec<&str> = text.split_whitespace().collect();
  let is_all_emoji = words.iter().all(|word| {
    token_shortcode(word)
      .is_some_and(|shortcode| refs.iter().any(|reference| reference.shortcode == shortcode))
  });
  (is_all_emoji && (1..=3).contains(&words.len())).then_some(words.len())
}

/// The `:shortcode:` words of a text outside fenced and inline code.
fn shortcodes_in(text: &str) -> Vec<&str> {
  let mut found = Vec::new();
  let mut is_fenced = false;
  for line in text.split('\n') {
    if line.trim_start().starts_with("```") {
      is_fenced = !is_fenced;
      continue;
    }
    if is_fenced {
      continue;
    }
    for (index, part) in line.split('`').enumerate() {
      // Odd parts are inside `inline code`.
      if index % 2 == 1 {
        continue;
      }
      let mut from = 0;
      while let Some(token) = token_at(part, from) {
        found.push(token.shortcode);
        from = token.end;
      }
    }
  }
  found
}

/// A `:shortcode:` token found in a text: where its colons are and its shortcode.
struct Token<'a> {
  /// The opening colon.
  open: usize,
  /// Just past the closing colon.
  end: usize,
  shortcode: &'a str,
}

/// The first `:shortcode:` in `text` at or after byte `from`.
fn token_at(text: &str, from: usize) -> Option<Token<'_>> {
  let mut at = from;
  loop {
    let open = at + text.get(at..)?.find(':')?;
    let close = open + 1 + text.get(open + 1..)?.find(':')?;
    let shortcode = &text[open + 1..close];
    if check_custom_emoji_shortcode(shortcode).is_ok() {
      return Some(Token {
        open,
        end: close + 1,
        shortcode,
      });
    }
    // The second colon may open a token itself (`a:b :ok:`).
    at = close;
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Id, Snowflake};

  fn id<T: Id>(raw: u64) -> T {
    T::from_snowflake(Snowflake::new(raw as i64).unwrap())
  }

  fn set(names: &[&str]) -> CustomEmojiSet {
    CustomEmojiSet {
      emoji: names
        .iter()
        .enumerate()
        .map(|(index, name)| CustomEmoji {
          id: id(index as u64 + 1),
          shortcode: (*name).to_owned(),
          sha256: CustomEmojiHash::from_bytes([index as u8 + 1; 32]),
          format: CustomEmojiFormat::Png,
          signed_by: id(1),
          signed_at: 0,
        })
        .collect(),
      limits: CustomEmojiLimits::default(),
    }
  }

  #[test]
  fn refs_name_each_known_emoji_once_outside_code() {
    let set = set(&["ship_it", "party"]);
    let refs = set.refs_in(":party: we :ship_it: it :party: `:ship_it:` :unknown: 10:30:00");
    let names: Vec<&str> = refs
      .iter()
      .map(|reference| reference.shortcode.as_str())
      .collect();
    assert_eq!(names, ["party", "ship_it"]);
    assert!(set.refs_in("```\n:party:\n```").is_empty());
    assert!(set.refs_in("no colons here").is_empty());
  }

  #[test]
  fn segments_split_at_the_named_emoji_only() {
    let set = set(&["ship_it"]);
    let refs = set.refs_in("go :ship_it:!");
    let pieces = segments("go :ship_it:! at 10:30:ship_it:", &refs);
    assert_eq!(
      pieces,
      [
        EmojiSegment::Text("go "),
        EmojiSegment::Emoji(&refs[0]),
        EmojiSegment::Text("! at 10:30"),
        EmojiSegment::Emoji(&refs[0]),
      ]
    );
    assert_eq!(
      segments("plain :ship_it:", &[]),
      [EmojiSegment::Text("plain :ship_it:")]
    );
  }

  #[test]
  fn new_names_are_checked_against_taken_ones() {
    let set = set(&["ship_it"]);
    assert_eq!(set.check_new_shortcode("deploy", None), Ok(()));
    assert_eq!(
      set.check_new_shortcode("Deploy", None),
      Err(NewShortcodeError::Invalid)
    );
    // Built-in emoji are the apps' to check.
    assert_eq!(set.check_new_shortcode("smile", None), Ok(()));
    assert_eq!(
      set.check_new_shortcode("ship_it", None),
      Err(NewShortcodeError::Taken)
    );
    // Renaming an emoji to its own name is no conflict.
    assert_eq!(set.check_new_shortcode("ship_it", Some(id(1))), Ok(()));
  }

  #[test]
  fn only_a_few_custom_emoji_alone_are_jumbo() {
    let set = set(&["ship_it", "party"]);
    let refs = set.refs_in(":ship_it: :party:");
    assert_eq!(jumbo_custom_count(" :ship_it:  :party: ", &refs), Some(2));
    assert_eq!(jumbo_custom_count(":ship_it: now", &refs), None);
    assert_eq!(jumbo_custom_count(":unknown:", &refs), None);
    assert_eq!(jumbo_custom_count(&":ship_it: ".repeat(4), &refs), None);
  }

  #[test]
  fn search_puts_prefix_matches_first() {
    let set = set(&["ship_it", "relationship", "party"]);
    let found: Vec<&str> = set
      .search(":ship")
      .iter()
      .map(|emoji| emoji.shortcode.as_str())
      .collect();
    assert_eq!(found, ["ship_it", "relationship"]);
    assert_eq!(token_shortcode(":party:"), Some("party"));
    assert_eq!(token_shortcode("🎉"), None);
  }
}
