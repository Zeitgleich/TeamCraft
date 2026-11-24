//! The text and log viewer of file previews (#821), apart from the DOM so it can be tested: how a
//! file's bytes become text (or are refused as binary), its escape sequences, lines and log levels,
//! finding, formatting JSON and which rows a virtualized view draws.
//!
//! The file is untrusted and shown as plain text only: nothing in it is read as markup, links are
//! not made clickable, and escape sequences become one of sixteen colours or are dropped
//! ([`ansi`]). Like the other previews it works from the file the client core has already
//! decrypted, in memory; nothing leaves the client. docs/e2ee.md, "Previews of files".

pub mod ansi;
pub mod json;
pub mod layout;
pub mod log;

pub use teamcraft_core::text_preview::{MAX_LINES, Span, decode, index_lines, search, table};

use crate::model::LogLevel;

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

#[cfg(test)]
mod tests {
  use super::*;

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
