//! Finding the `@mention`, `/command` or `:emoji` being typed in the composer, on UTF-16 offsets
//! (what the browser's selection reports).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerKind {
  Mention,
  Command,
  /// `:po`: emoji whose shortcode or name starts with it.
  Emoji,
}

/// Letters typed after `:` before emoji are suggested, so `:)` and `12:30` stay quiet.
const MIN_EMOJI_QUERY_CHARS: usize = 2;

/// The word being typed that opens a picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
  pub kind: TriggerKind,
  /// What was typed after the `@`, `/` or `:`.
  pub query: String,
  /// UTF-16 offset of the `@`, `/` or `:`.
  pub start: usize,
}

fn is_mention_char(character: char) -> bool {
  character.is_alphanumeric() || matches!(character, '_' | '.' | '-')
}

/// The trigger right before the cursor: `@na` after a space or at the start (a mention), `/he` at
/// the very start of the text (a command), or `:po` after a space or at the start (an emoji).
pub fn trigger(text: &str, cursor: usize) -> Option<Trigger> {
  let mut offset = 0;
  let mut before: Vec<(usize, char)> = Vec::new();
  for character in text.chars() {
    if offset >= cursor {
      break;
    }
    before.push((offset, character));
    offset += character.len_utf16();
  }
  let word_start = before
    .iter()
    .rposition(|(_, character)| !is_mention_char(*character))
    .unwrap_or(0);
  let (marker_at, marker) = *before.get(word_start)?;
  let query: String = before[word_start + 1..]
    .iter()
    .map(|(_, character)| *character)
    .collect();
  let preceded_by_boundary = word_start == 0 || before[word_start - 1].1.is_whitespace();
  match marker {
    '@' if preceded_by_boundary => Some(Trigger {
      kind: TriggerKind::Mention,
      query,
      start: marker_at,
    }),
    '/' if marker_at == 0 && !query.contains('.') => Some(Trigger {
      kind: TriggerKind::Command,
      query,
      start: 0,
    }),
    ':' if preceded_by_boundary && is_emoji_query(&query) => Some(Trigger {
      kind: TriggerKind::Emoji,
      query,
      start: marker_at,
    }),
    _ => None,
  }
}

/// Long enough, starting with a letter or digit and without dots (`:-)`, `:..` stay text).
fn is_emoji_query(query: &str) -> bool {
  query.chars().count() >= MIN_EMOJI_QUERY_CHARS
    && query.chars().next().is_some_and(char::is_alphanumeric)
    && !query.contains('.')
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn mentions_open_after_a_boundary_only() {
    let found = trigger("hi @sa", 6).expect("a mention");
    assert_eq!(
      (found.kind, found.query.as_str(), found.start),
      (TriggerKind::Mention, "sa", 3)
    );
    assert!(trigger("mail me@sa", 10).is_none());
    assert_eq!(trigger("@", 1).map(|found| found.query), Some(String::new()));
    assert!(trigger("hi @sam there", 13).is_none());
  }

  #[test]
  fn commands_open_only_at_the_start() {
    let found = trigger("/he", 3).expect("a command");
    assert_eq!((found.kind, found.query.as_str()), (TriggerKind::Command, "he"));
    assert!(trigger("a /he", 5).is_none());
    assert!(trigger("/usr/bin", 8).is_none());
  }

  #[test]
  fn emoji_open_after_two_characters_at_a_boundary() {
    let found = trigger("nice :po", 8).expect("an emoji");
    assert_eq!(
      (found.kind, found.query.as_str(), found.start),
      (TriggerKind::Emoji, "po", 5)
    );
    assert_eq!(
      trigger(":thumbs", 7).map(|found| found.kind),
      Some(TriggerKind::Emoji)
    );
    assert!(trigger("hi :p", 5).is_none());
    assert!(trigger("at 12:30", 8).is_none());
    assert!(trigger("see http://example", 18).is_none());
    assert!(trigger("hi :-)", 6).is_none());
    assert!(trigger(":tada: ", 7).is_none());
  }
}
