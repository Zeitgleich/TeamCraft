//! Find in a file: every match of a query, ignoring case, within the lines shown. Matches don't
//! span lines and don't overlap; their count is capped so a one-letter query in a large log stays
//! cheap.

use super::Span;

/// Matches found at most; the count then reads "10000+".
pub const MAX_MATCHES: usize = 10_000;

/// The matches of a query, in the order of the lines searched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
  pub matches: Vec<Match>,
  /// More matched than [`MAX_MATCHES`].
  pub is_capped: bool,
}

/// One match: its line (index into the document's lines) and where it is in the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
  pub line: u32,
  pub span: Span,
}

/// The query, lower case, in the form its comparison needs.
enum Needle {
  /// An ASCII query compares bytes: non-ASCII bytes never match it, so matches end on character
  /// boundaries.
  Ascii(Vec<u8>),
  Unicode(Vec<char>),
}

/// Every match of `query` in `lines` (line index and span pairs), at most `cap`.
pub fn find_all(text: &str, lines: impl IntoIterator<Item = (u32, Span)>, query: &str, cap: usize) -> Found {
  let mut found = Found::default();
  if query.is_empty() {
    return found;
  }
  let needle = if query.is_ascii() {
    Needle::Ascii(query.bytes().map(|byte| byte.to_ascii_lowercase()).collect())
  } else {
    Needle::Unicode(query.chars().flat_map(char::to_lowercase).collect())
  };
  for (line, span) in lines {
    let Some(haystack) = text.get(span.range()) else {
      continue;
    };
    let mut push = |start: usize, end: usize| {
      if found.matches.len() == cap {
        found.is_capped = true;
        return false;
      }
      found.matches.push(Match {
        line,
        span: Span::new(start, end).shifted(span.start),
      });
      true
    };
    let is_complete = match &needle {
      Needle::Ascii(needle) => ascii_matches(haystack.as_bytes(), needle, &mut push),
      Needle::Unicode(needle) => unicode_matches(haystack, needle, &mut push),
    };
    if !is_complete {
      break;
    }
  }
  found
}

/// Hands each match to `push` until it says stop; `false` when it did.
fn ascii_matches(haystack: &[u8], needle: &[u8], push: &mut impl FnMut(usize, usize) -> bool) -> bool {
  let Some(&first) = needle.first() else {
    return true;
  };
  let mut at = 0;
  while at + needle.len() <= haystack.len() {
    let end = at + needle.len();
    if haystack[at].to_ascii_lowercase() == first && haystack[at..end].eq_ignore_ascii_case(needle) {
      if !push(at, end) {
        return false;
      }
      at = end;
    } else {
      at += 1;
    }
  }
  true
}

fn unicode_matches(haystack: &str, needle: &[char], push: &mut impl FnMut(usize, usize) -> bool) -> bool {
  let mut next_free = 0;
  for (at, _) in haystack.char_indices() {
    if at < next_free {
      continue;
    }
    if let Some(end) = match_at(haystack, at, needle) {
      if !push(at, end) {
        return false;
      }
      next_free = end;
    }
  }
  true
}

/// The end of a match starting at `at`, comparing lower case forms.
fn match_at(haystack: &str, at: usize, needle: &[char]) -> Option<usize> {
  let mut wanted = needle.iter();
  let mut maybe_next = wanted.next();
  for (offset, character) in haystack[at..].char_indices() {
    for lower in character.to_lowercase() {
      match maybe_next {
        Some(&next) if next == lower => maybe_next = wanted.next(),
        _ => return None,
      }
    }
    if maybe_next.is_none() {
      return Some(at + offset + character.len_utf8());
    }
  }
  None
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::text_preview::index_lines;

  #[test]
  fn ascii_queries_ignore_case_and_dont_overlap() {
    let text = "Error error ERROR\naaaa";
    assert_eq!(spans(text, "error"), ["Error", "error", "ERROR"]);
    assert_eq!(spans(text, "aa"), ["aa", "aa"]);
  }

  #[test]
  fn matches_know_their_line_and_place_in_the_text() {
    let found = find(text_of_three_lines(), "two");
    assert_eq!(
      found.matches,
      [Match {
        line: 1,
        span: Span::new(4, 7),
      }]
    );
  }

  #[test]
  fn unicode_queries_ignore_case_too() {
    let text = "Größe GRÖSSE größe\nÄrger ärger";
    assert_eq!(spans(text, "größe"), ["Größe", "größe"]);
    assert_eq!(spans(text, "Ärger"), ["Ärger", "ärger"]);
    assert_eq!(spans("İstanbul", "i̇stanbul"), ["İstanbul"]);
  }

  #[test]
  fn an_ascii_query_never_matches_inside_a_character() {
    assert!(spans("é€", "a").is_empty());
    assert_eq!(spans("éxé", "x"), ["x"]);
  }

  #[test]
  fn matches_stay_within_a_line() {
    assert!(spans("ab\ncd", "bc").is_empty());
    assert!(spans("ab\ncd", "b\nc").is_empty());
  }

  #[test]
  fn the_count_is_capped() {
    let text = "x".repeat(50);
    let mut lines = Vec::new();
    index_lines(&text, 0, usize::MAX, &mut lines);
    let found = find_all(&text, numbered(&lines), "x", 10);
    assert_eq!(found.matches.len(), 10);
    assert!(found.is_capped);
    assert!(!find_all(&text, numbered(&lines), "x", 50).is_capped);
  }

  #[test]
  fn an_empty_query_finds_nothing() {
    assert_eq!(find("abc", ""), Found::default());
  }

  fn text_of_three_lines() -> &'static str {
    "one\ntwo\nthree"
  }

  fn find(text: &str, query: &str) -> Found {
    let mut lines = Vec::new();
    index_lines(text, 0, usize::MAX, &mut lines);
    find_all(text, numbered(&lines), query, MAX_MATCHES)
  }

  fn spans<'a>(text: &'a str, query: &str) -> Vec<&'a str> {
    find(text, query)
      .matches
      .iter()
      .map(|found| &text[found.span.range()])
      .collect()
  }

  fn numbered(lines: &[Span]) -> impl Iterator<Item = (u32, Span)> + '_ {
    lines
      .iter()
      .enumerate()
      .map(|(index, span)| (index as u32, *span))
  }
}
