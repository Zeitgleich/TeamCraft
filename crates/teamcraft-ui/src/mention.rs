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

/// Replaces the selection `start..end` (UTF-16 offsets, as the browser reports them) with `insert`
/// and returns the new text and the cursor after the insertion.
pub fn replace_selection(text: &str, start: usize, end: usize, insert: &str) -> (String, usize) {
  let (from, to) = (byte_index(text, start), byte_index(text, end.max(start)));
  let new_cursor = start + insert.encode_utf16().count();
  (format!("{}{insert}{}", &text[..from], &text[to..]), new_cursor)
}

/// Pasting a URL over selected text makes a Markdown link of it: `[selected](url)`. `None` when the
/// paste is not a single http(s) URL, nothing is selected or the selection is a URL itself.
pub fn link_over_selection(text: &str, start: usize, end: usize, pasted: &str) -> Option<(String, usize)> {
  let url = pasted.trim();
  let is_url = |value: &str| {
    (value.starts_with("https://") || value.starts_with("http://"))
      && value.len() > "https://".len()
      && !value.contains(char::is_whitespace)
  };
  if end <= start || !is_url(url) {
    return None;
  }
  let (from, to) = (byte_index(text, start), byte_index(text, end));
  let selected = &text[from..to];
  if is_url(selected.trim()) || selected.contains('\n') || selected.contains(['[', ']']) {
    return None;
  }
  Some(replace_selection(
    text,
    start,
    end,
    &format!("[{selected}]({url})"),
  ))
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

  #[test]
  fn replacing_the_selection_counts_utf16() {
    assert_eq!(replace_selection("ab", 1, 1, "🙂"), ("a🙂b".to_owned(), 3));
    assert_eq!(
      replace_selection("🙂 hi there", 3, 5, "yo"),
      ("🙂 yo there".to_owned(), 5)
    );
  }

  #[test]
  fn pasting_a_url_over_a_selection_makes_a_link() {
    let (text, cursor) =
      link_over_selection("see the docs now", 8, 12, " https://example.com/a ").unwrap_or_default();
    assert_eq!(text, "see the [docs](https://example.com/a) now");
    assert_eq!(cursor, 37);
  }

  #[test]
  fn link_paste_leaves_other_pastes_alone() {
    assert_eq!(link_over_selection("docs", 0, 0, "https://example.com"), None);
    assert_eq!(link_over_selection("docs", 0, 4, "not a url"), None);
    assert_eq!(link_over_selection("docs", 0, 4, "https://a b"), None);
    assert_eq!(
      link_over_selection("https://old.example", 0, 19, "https://example.com"),
      None
    );
    assert_eq!(link_over_selection("a\nb", 0, 3, "https://example.com"), None);
    assert_eq!(link_over_selection("docs", 0, 4, "ftp://example.com"), None);
  }
}
