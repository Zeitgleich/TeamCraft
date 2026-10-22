//! Text editing helpers of the composer, on UTF-16 offsets (what the browser's `selectionStart`
//! reports): finding the `@mention` or `/command` being typed, completing it, and wrapping a
//! selection in Markdown markers.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerKind {
  Mention,
  Command,
}

/// The word being typed that opens a picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
  pub kind: TriggerKind,
  /// What was typed after the `@` or `/`.
  pub query: String,
  /// UTF-16 offset of the `@` or `/`.
  pub start: usize,
}

fn is_mention_char(character: char) -> bool {
  character.is_alphanumeric() || matches!(character, '_' | '.' | '-')
}

/// The trigger right before the cursor: `@na` after a space or at the start (a mention), or `/he` at
/// the very start of the text (a command).
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
    _ => None,
  }
}

/// Replaces the typed trigger word (`@na`) with `insert` (without the marker) and a space; returns the
/// new text and cursor.
pub fn complete(text: &str, trigger: &Trigger, cursor: usize, insert: &str) -> (String, usize) {
  let marker = match trigger.kind {
    TriggerKind::Mention => '@',
    TriggerKind::Command => '/',
  };
  let (from, to) = (byte_index(text, trigger.start), byte_index(text, cursor));
  let (head, tail) = (&text[..from], &text[to..]);
  let replacement = format!("{marker}{insert} ");
  let new_cursor = trigger.start + replacement.encode_utf16().count();
  (format!("{head}{replacement}{tail}"), new_cursor)
}

/// Wraps the selection `start..end` in `left` and `right` (an empty selection gets the markers with
/// the cursor between them). Returns the new text and the new selection.
pub fn wrap_selection(
  text: &str,
  start: usize,
  end: usize,
  left: &str,
  right: &str,
) -> (String, usize, usize) {
  let (from, to) = (byte_index(text, start), byte_index(text, end.max(start)));
  let (head, selected, tail) = (&text[..from], &text[from..to], &text[to..]);
  let new_start = start + left.encode_utf16().count();
  let new_end = new_start + selected.encode_utf16().count();
  (format!("{head}{left}{selected}{right}{tail}"), new_start, new_end)
}

fn byte_index(text: &str, utf16_offset: usize) -> usize {
  let mut units = 0;
  for (index, character) in text.char_indices() {
    if units >= utf16_offset {
      return index;
    }
    units += character.len_utf16();
  }
  text.len()
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
  fn completion_replaces_the_typed_word_and_handles_emoji_offsets() {
    let text = "😀 @sa and more";
    let cursor = "😀 @sa".encode_utf16().count();
    let found = trigger(text, cursor).expect("a mention");
    let (done, new_cursor) = complete(text, &found, cursor, "sam");
    assert_eq!(done, "😀 @sam  and more");
    assert_eq!(new_cursor, "😀 @sam ".encode_utf16().count());
  }

  #[test]
  fn selections_are_wrapped_in_markers() {
    assert_eq!(
      wrap_selection("say hi now", 4, 6, "**", "**"),
      ("say **hi** now".to_owned(), 6, 8)
    );
    assert_eq!(wrap_selection("x", 1, 1, "`", "`"), ("x``".to_owned(), 2, 2));
  }
}
