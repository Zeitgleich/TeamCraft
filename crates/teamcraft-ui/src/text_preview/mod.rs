//! The text and log viewer of file previews (#821), apart from the DOM so it can be tested: how a
//! file's bytes become text (or are refused as binary), its escape sequences, lines and log levels,
//! finding, formatting JSON and which rows a virtualized view draws.
//!
//! The file is untrusted and shown as plain text only: nothing in it is read as markup, links are
//! not made clickable, and escape sequences become one of sixteen colours or are dropped
//! ([`ansi`]). Like the other previews it works from the file the client core has already
//! decrypted, in memory; nothing leaves the client. docs/e2ee.md, "Previews of files".

pub mod ansi;
pub mod decode;
pub mod json;
pub mod layout;
pub mod log;
pub mod search;

use std::ops::Range;

use crate::model::LogLevel;

/// Lines shown at most. Each line costs some 30 bytes of bookkeeping besides its text, so a file
/// of nothing but line breaks would otherwise take hundreds of megabytes; the rest of such a file
/// is left to the download.
pub const MAX_LINES: usize = 1_000_000;

/// A byte range of a text, `start..end`. Offsets are `u32`: a preview is far below 4 GiB (the
/// largest limit is 50 MiB, `settings::PreviewLimit`), and a large file has many lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
  pub start: u32,
  pub end: u32,
}

/// A file's text, ready to show: the text without escape sequences, its lines, the colours the
/// escape sequences set and, for a log, each line's level.
#[derive(Debug, Default)]
pub struct TextDoc {
  pub text: String,
  pub lines: Vec<Span>,
  pub runs: Vec<ansi::StyledRun>,
  /// One level per line; `None` when the file is no log.
  pub maybe_levels: Option<Vec<LogLevel>>,
  /// The text has more than [`MAX_LINES`] lines; only those are indexed and shown.
  pub is_truncated: bool,
}

impl Span {
  pub fn new(start: usize, end: usize) -> Self {
    Self {
      start: offset(start),
      end: offset(end),
    }
  }

  pub fn range(self) -> Range<usize> {
    self.start as usize..self.end as usize
  }

  pub fn len(self) -> usize {
    self.end.saturating_sub(self.start) as usize
  }

  pub fn is_empty(self) -> bool {
    self.end <= self.start
  }

  /// Whether the two ranges share at least one byte.
  pub fn overlaps(self, other: Span) -> bool {
    self.start < other.end && other.start < self.end
  }

  /// This range moved by `by` bytes (a range within a line, made one within the text).
  pub fn shifted(self, by: u32) -> Self {
    Self {
      start: self.start.saturating_add(by),
      end: self.end.saturating_add(by),
    }
  }
}

impl TextDoc {
  /// A document of plain text without colours or levels (formatted JSON).
  pub fn plain(text: String) -> Self {
    let mut lines = Vec::new();
    let is_truncated = index_lines(&text, 0, MAX_LINES, &mut lines).is_some();
    Self {
      text,
      lines,
      runs: Vec::new(),
      maybe_levels: None,
      is_truncated,
    }
  }

  /// The text of line `index` (without its line break); empty for a line that doesn't exist.
  pub fn line(&self, index: usize) -> &str {
    self
      .lines
      .get(index)
      .and_then(|span| self.text.get(span.range()))
      .unwrap_or_default()
  }

  pub fn is_log(&self) -> bool {
    self.maybe_levels.is_some()
  }
}

/// Splits `text` into lines from byte `from`, at most `max_lines` of them, onto `lines`. Lines end
/// at `\n`, `\r\n` or a lone `\r` (as in editors); the break is not part of the line, and a break at
/// the very end starts no further, empty line. Returns where to go on, `None` once the text is done,
/// so a large file can be split in steps.
pub fn index_lines(text: &str, from: usize, max_lines: usize, lines: &mut Vec<Span>) -> Option<usize> {
  let bytes = text.as_bytes();
  let mut start = from;
  let mut count = 0;
  while start < bytes.len() {
    if count == max_lines {
      return Some(start);
    }
    let end = bytes[start..]
      .iter()
      .position(|&byte| byte == b'\n' || byte == b'\r')
      .map_or(bytes.len(), |found| start + found);
    lines.push(Span::new(start, end));
    count += 1;
    start = match (bytes.get(end), bytes.get(end + 1)) {
      (Some(b'\r'), Some(b'\n')) => end + 2,
      _ => end + 1,
    };
  }
  None
}

/// `value` as a text offset. Texts of a preview are far smaller than 4 GiB; a larger one would
/// show its end at the limit rather than panic.
fn offset(value: usize) -> u32 {
  u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;

  mod lines {
    use super::*;

    #[test]
    fn every_kind_of_line_break_ends_a_line() {
      assert_eq!(texts("a\nb\r\nc\rd"), ["a", "b", "c", "d"]);
    }

    #[test]
    fn a_break_at_the_end_adds_no_empty_line_but_empty_lines_inside_stay() {
      assert_eq!(texts("a\n"), ["a"]);
      assert_eq!(texts("a\n\n\nb"), ["a", "", "", "b"]);
      assert_eq!(texts("\n"), [""]);
      assert!(texts("").is_empty());
    }

    #[test]
    fn indexing_in_steps_gives_the_same_lines() {
      let text = "one\r\ntwo\nthree\rfour\n\nsix";
      let mut stepped = Vec::new();
      let mut from = Some(0);
      while let Some(at) = from {
        from = index_lines(text, at, 2, &mut stepped);
      }
      let mut whole = Vec::new();
      assert_eq!(index_lines(text, 0, usize::MAX, &mut whole), None);
      assert_eq!(stepped, whole);
    }

    #[test]
    fn a_document_reads_its_lines() {
      let doc = TextDoc::plain("x\ny".to_owned());
      assert_eq!(doc.line(1), "y");
      assert_eq!(doc.line(7), "");
      assert!(!doc.is_log());
      assert!(!doc.is_truncated);
    }

    #[test]
    fn a_document_shows_at_most_the_line_limit() {
      assert!(!TextDoc::plain("\n".repeat(MAX_LINES)).is_truncated);
      let doc = TextDoc::plain("\n".repeat(MAX_LINES + 1));
      assert!(doc.is_truncated);
      assert_eq!(doc.lines.len(), MAX_LINES);
    }
  }

  mod spans {
    use super::*;

    #[test]
    fn overlap_needs_a_shared_byte() {
      let a = Span::new(2, 5);
      assert!(a.overlaps(Span::new(4, 9)));
      assert!(!a.overlaps(Span::new(5, 9)));
      assert!(!a.overlaps(Span::new(0, 2)));
      assert_eq!(a.shifted(10), Span::new(12, 15));
      assert_eq!(a.len(), 3);
    }
  }

  fn texts(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    index_lines(text, 0, usize::MAX, &mut lines);
    lines.iter().map(|span| &text[span.range()]).collect()
  }
}
