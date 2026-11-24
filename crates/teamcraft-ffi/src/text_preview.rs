//! In-app preview of text and delimited files on Android and iOS (#2342): the same text decoding,
//! separator sniffing, table parser and find as the desktop and web viewer
//! (`teamcraft_core::text_preview`), behind one object per open file.
//!
//! Everything that reads the file or walks its rows is `async` (#2184): the work runs off the UI
//! thread, and the apps ask for the few rows on screen ([`TextPreview::lines`],
//! [`TextPreview::table_rows`]), so a large file costs the apps no memory of their own. The file is
//! untrusted plain text: nothing in it is read as markup. Match ranges are UTF-16 offsets within
//! the text of a line or cell as the apps get it, what Kotlin strings and `NSString` index by.

use std::fs::File;
use std::io::Read;
use std::sync::{Arc, Mutex, PoisonError};

use teamcraft_core::text_preview::decode::{self, TextDecodeError, TextEncoding};
use teamcraft_core::text_preview::search::{self, MAX_MATCHES};
use teamcraft_core::text_preview::table::{
  self, CustomDelimiter, Delimiter, Dialect, Quote, Table, TableError, TableParser,
};
use teamcraft_core::text_preview::{MAX_LINES, Span, index_lines};

use crate::blocking::off_thread;

/// Rows parsed per step when a table is built; a step is short, so a huge file never holds a lock.
const PARSE_STEP_ROWS: usize = 50_000;
/// Characters of a cell [`TextPreview::table_rows`] returns; [`TextPreview::table_cell`] gives all.
const CELL_PREVIEW_CHARS: usize = 400;
/// Characters of a line [`TextPreview::lines`] returns, so a minified one-line file stays cheap.
const LINE_PREVIEW_CHARS: usize = 4_000;
/// Matches of a find at most (the apps show "10000+").
const FIND_CAP: usize = MAX_MATCHES;

/// One open file. Created by [`open_text_preview`], dropped when the preview closes.
#[derive(uniffi::Object)]
pub struct TextPreview {
  text: Arc<str>,
  lines: Vec<Span>,
  info: TextPreviewInfo,
  maybe_sniffed: Option<table::Sniffed>,
  table: Mutex<Option<Arc<Table>>>,
}

/// What the preview knows about the file.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TextPreviewInfo {
  pub line_count: u32,
  /// More lines than the preview holds, or the file is larger than the limit: the rest is left to
  /// the download.
  pub is_truncated: bool,
  pub encoding: PreviewEncoding,
  /// The separator the sniffer found; `None` when the text is no table.
  pub maybe_detected: Option<DelimiterChoice>,
}

/// How the file's bytes were read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum PreviewEncoding {
  Utf8,
  Utf8Repaired,
  Utf16,
  Latin1,
}

/// What stands between fields.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum DelimiterChoice {
  Comma,
  Semicolon,
  Tab,
  Pipe,
  Colon,
  /// A separator the user typed (up to eight bytes, no line breaks or quotes).
  Custom {
    text: String,
  },
}

/// A parsed table's shape.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TableInfo {
  pub dialect: DelimiterChoice,
  pub rows: u32,
  pub columns: u32,
  /// The file has more rows than a table holds.
  pub is_truncated: bool,
  /// Some row has more fields than are shown.
  pub has_dropped_columns: bool,
  /// The first row reads like column names (what [`HeaderChoice::Auto`] decides by).
  pub looks_like_header: bool,
  pub columns_profile: Vec<ColumnInfo>,
}

/// How a column reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ColumnInfo {
  /// Numbers: aligned at the end.
  pub is_numeric: bool,
  /// A starting width in characters.
  pub width_chars: u32,
}

/// One match of a find in the text view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LineMatch {
  pub line: u32,
  pub start: u32,
  pub end: u32,
}

/// One match of a find in the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CellHit {
  pub row: u32,
  pub column: u32,
  pub start: u32,
  pub end: u32,
}

/// The matches of a find.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LineFind {
  pub matches: Vec<LineMatch>,
  pub is_capped: bool,
}

/// The matches of a find in the table.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CellFind {
  pub matches: Vec<CellHit>,
  pub is_capped: bool,
}

/// Why a file has no preview, or no table.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum TextPreviewError {
  #[error("the file could not be read")]
  Unreadable,
  /// Not text (binary): offer the other app.
  #[error("the file is not text")]
  NotText,
  /// The separator is empty, too long or holds a line break or a quote.
  #[error("the separator is not usable")]
  BadSeparator,
  /// The text is no table in this dialect; show it as text and say so.
  #[error("the text is not a consistent table")]
  NotATable,
  /// A quoted field is never closed.
  #[error("a quoted field from line {line} is never closed")]
  UnterminatedQuote { line: u32 },
}

/// Opens the file at `path`: its first `max_bytes` bytes as text (cut at a line break when the file
/// is longer) and, when the text reads as a table, the sniffed separator. Reads off the UI thread.
#[uniffi::export]
pub async fn open_text_preview(path: String, max_bytes: u64) -> Result<Arc<TextPreview>, TextPreviewError> {
  off_thread("text-preview-open", move || open(&path, max_bytes)).await
}

#[uniffi::export]
impl TextPreview {
  pub fn info(&self) -> TextPreviewInfo {
    self.info.clone()
  }

  /// Lines `start..start + count`, each cut at a length the screen can't show anyway.
  pub fn lines(&self, start: u32, count: u32) -> Vec<String> {
    let from = start as usize;
    (from..from.saturating_add(count as usize).min(self.lines.len()))
      .map(|index| clip(self.line(index), LINE_PREVIEW_CHARS))
      .collect()
  }

  /// Finds `query` in the lines, ignoring case.
  pub async fn find_lines(self: Arc<Self>, query: String) -> LineFind {
    off_thread("text-preview-find", move || self.find_in_lines(&query)).await
  }

  /// Parses the text as a table in `delimiter` (`None`: the sniffed one). Replaces the table the
  /// rows come from. Fails with [`TextPreviewError::NotATable`] when the rows don't fit it.
  pub async fn parse_table(
    self: Arc<Self>,
    maybe_delimiter: Option<DelimiterChoice>,
  ) -> Result<TableInfo, TextPreviewError> {
    off_thread("text-preview-table", move || self.build_table(maybe_delimiter)).await
  }

  /// Rows `start..start + count` of the table, each cell cut to a few hundred characters. Empty
  /// before [`Self::parse_table`] succeeded.
  pub fn table_rows(&self, start: u32, count: u32) -> Vec<Vec<String>> {
    let Some(state) = self.current_table() else {
      return Vec::new();
    };
    let from = start as usize;
    (from..from.saturating_add(count as usize).min(state.rows()))
      .map(|row| {
        (0..state.row_len(row))
          .map(|column| {
            clip(
              state.cell(&self.text, row, column).unwrap_or_default(),
              CELL_PREVIEW_CHARS,
            )
          })
          .collect()
      })
      .collect()
  }

  /// A cell's whole text (to copy it).
  pub fn table_cell(&self, row: u32, column: u32) -> String {
    self
      .current_table()
      .and_then(|state| {
        state
          .cell(&self.text, row as usize, column as usize)
          .map(str::to_owned)
      })
      .unwrap_or_default()
  }

  /// A row's cells separated by tabs (to copy it).
  pub fn table_row_text(&self, row: u32) -> String {
    self
      .current_table()
      .map(|state| state.row_text(&self.text, row as usize))
      .unwrap_or_default()
  }

  /// Column alignment and widths, for a first row that is (or is not) the header.
  pub fn table_profile(&self, has_header: bool) -> Vec<ColumnInfo> {
    self
      .current_table()
      .map(|state| profile_of(&state, &self.text, has_header))
      .unwrap_or_default()
  }

  /// Finds `query` in the cells, ignoring case.
  pub async fn find_cells(self: Arc<Self>, query: String) -> CellFind {
    off_thread("text-preview-find", move || {
      let Some(state) = self.current_table() else {
        return CellFind {
          matches: Vec::new(),
          is_capped: false,
        };
      };
      let (found, is_capped) = state.find(&self.text, &query, FIND_CAP);
      let matches = found
        .iter()
        .filter_map(|hit| {
          let cell = state.cell(&self.text, hit.row as usize, hit.column as usize)?;
          Some(CellHit {
            row: hit.row,
            column: hit.column,
            start: utf16_at(cell, hit.span.start as usize),
            end: utf16_at(cell, hit.span.end as usize),
          })
        })
        .collect();
      CellFind { matches, is_capped }
    })
    .await
  }
}

impl TextPreview {
  fn line(&self, index: usize) -> &str {
    self
      .lines
      .get(index)
      .and_then(|span| self.text.get(span.range()))
      .unwrap_or_default()
  }

  fn find_in_lines(&self, query: &str) -> LineFind {
    let spans = self
      .lines
      .iter()
      .enumerate()
      .map(|(index, span)| (to_u32(index), *span));
    let found = search::find_all(&self.text, spans, query, FIND_CAP);
    let matches = found
      .matches
      .iter()
      .filter_map(|hit| {
        let line = self.lines.get(hit.line as usize)?;
        let text = self.text.get(line.range())?;
        let start = hit.span.start.checked_sub(line.start)? as usize;
        let end = hit.span.end.checked_sub(line.start)? as usize;
        Some(LineMatch {
          line: hit.line,
          start: utf16_at(text, start),
          end: utf16_at(text, end),
        })
      })
      .collect();
    LineFind {
      matches,
      is_capped: found.is_capped,
    }
  }

  fn current_table(&self) -> Option<Arc<Table>> {
    self.table.lock().unwrap_or_else(PoisonError::into_inner).clone()
  }

  fn build_table(&self, maybe_delimiter: Option<DelimiterChoice>) -> Result<TableInfo, TextPreviewError> {
    let dialect = match maybe_delimiter {
      Some(choice) => self.dialect_for(choice)?,
      None => self
        .maybe_sniffed
        .map(|found| found.dialect)
        .ok_or(TextPreviewError::NotATable)?,
    };
    let mut parser = TableParser::new(dialect);
    while !parser
      .step(&self.text, PARSE_STEP_ROWS)
      .map_err(map_table_error)?
    {}
    let table = parser.finish().map_err(map_table_error)?;
    let looks_like_header = table.looks_like_header(&self.text);
    let info = TableInfo {
      dialect: choice_of(dialect.delimiter),
      rows: to_u32(table.rows()),
      columns: to_u32(table.columns),
      is_truncated: table.is_truncated,
      has_dropped_columns: table.has_dropped_columns,
      looks_like_header,
      columns_profile: profile_of(&table, &self.text, looks_like_header),
    };
    *self.table.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(table));
    Ok(info)
  }

  /// A typed or picked separator with the quote character that suits the text best.
  fn dialect_for(&self, choice: DelimiterChoice) -> Result<Dialect, TextPreviewError> {
    let delimiter = delimiter_of(choice)?;
    let mut dialect = Dialect::new(delimiter);
    // A single-quoted file is rare; keep the quote the sniffer found when it found this separator.
    if let Some(found) = self.maybe_sniffed
      && found.dialect.delimiter == delimiter
    {
      dialect.quote = found.dialect.quote;
    } else if table::sniff_dialect(
      &self.text,
      Dialect {
        delimiter,
        quote: Quote::Single,
      },
    )
    .zip(table::sniff_dialect(&self.text, dialect))
    .is_some_and(|(single, double)| single.consistent_percent > double.consistent_percent)
    {
      dialect.quote = Quote::Single;
    }
    Ok(dialect)
  }
}

fn open(path: &str, max_bytes: u64) -> Result<Arc<TextPreview>, TextPreviewError> {
  let file = File::open(path).map_err(unreadable)?;
  open_reader(file, max_bytes)
}

fn open_reader(reader: impl Read, max_bytes: u64) -> Result<Arc<TextPreview>, TextPreviewError> {
  let mut bytes = Vec::new();
  reader
    .take(max_bytes.saturating_add(1))
    .read_to_end(&mut bytes)
    .map_err(unreadable)?;
  let is_cut = bytes.len() as u64 > max_bytes;
  if is_cut {
    bytes.truncate(usize::try_from(max_bytes).unwrap_or(usize::MAX));
    // Cut at a line break: a half record would read as a ragged row.
    if let Some(last_break) = bytes.iter().rposition(|byte| *byte == b'\n') {
      bytes.truncate(last_break + 1);
    }
  }
  let decoded = decode::decode(bytes).map_err(|TextDecodeError::Binary| TextPreviewError::NotText)?;
  let mut lines = Vec::new();
  let is_over_lines = index_lines(&decoded.text, 0, MAX_LINES, &mut lines).is_some();
  let maybe_sniffed = table::sniff(&decoded.text);
  let info = TextPreviewInfo {
    line_count: to_u32(lines.len()),
    is_truncated: is_cut || is_over_lines,
    encoding: match decoded.encoding {
      TextEncoding::Utf8 => PreviewEncoding::Utf8,
      TextEncoding::Utf8Repaired => PreviewEncoding::Utf8Repaired,
      TextEncoding::Utf16 => PreviewEncoding::Utf16,
      TextEncoding::Latin1 => PreviewEncoding::Latin1,
    },
    maybe_detected: maybe_sniffed.map(|found| choice_of(found.dialect.delimiter)),
  };
  Ok(Arc::new(TextPreview {
    text: Arc::from(decoded.text),
    lines,
    info,
    maybe_sniffed,
    table: Mutex::new(None),
  }))
}

fn unreadable(error: std::io::Error) -> TextPreviewError {
  tracing::warn!(kind = ?error.kind(), "text preview: file not readable");
  TextPreviewError::Unreadable
}

fn profile_of(table: &Table, text: &str, has_header: bool) -> Vec<ColumnInfo> {
  table
    .profile(text, has_header)
    .into_iter()
    .map(|column| ColumnInfo {
      is_numeric: column.is_numeric,
      width_chars: column.width_chars,
    })
    .collect()
}

fn choice_of(delimiter: Delimiter) -> DelimiterChoice {
  match delimiter {
    Delimiter::Comma => DelimiterChoice::Comma,
    Delimiter::Semicolon => DelimiterChoice::Semicolon,
    Delimiter::Tab => DelimiterChoice::Tab,
    Delimiter::Pipe => DelimiterChoice::Pipe,
    Delimiter::Colon => DelimiterChoice::Colon,
    Delimiter::Custom(custom) => DelimiterChoice::Custom {
      text: custom.as_str().to_owned(),
    },
  }
}

fn delimiter_of(choice: DelimiterChoice) -> Result<Delimiter, TextPreviewError> {
  Ok(match choice {
    DelimiterChoice::Comma => Delimiter::Comma,
    DelimiterChoice::Semicolon => Delimiter::Semicolon,
    DelimiterChoice::Tab => Delimiter::Tab,
    DelimiterChoice::Pipe => Delimiter::Pipe,
    DelimiterChoice::Colon => Delimiter::Colon,
    DelimiterChoice::Custom { text } => {
      Delimiter::Custom(CustomDelimiter::new(&text).ok_or(TextPreviewError::BadSeparator)?)
    },
  })
}

fn map_table_error(error: TableError) -> TextPreviewError {
  match error {
    TableError::UnterminatedQuote { line } => TextPreviewError::UnterminatedQuote { line },
    TableError::NotDelimited | TableError::Ragged { .. } => TextPreviewError::NotATable,
  }
}

/// `text` cut to `max_chars` characters, with an ellipsis when it was cut.
fn clip(text: &str, max_chars: usize) -> String {
  match text.char_indices().nth(max_chars) {
    Some((end, _)) => format!("{}…", &text[..end]),
    None => text.to_owned(),
  }
}

/// The UTF-16 offset of byte `at` in `text` (the end when `at` is past it).
fn utf16_at(text: &str, at: usize) -> u32 {
  let end = at.min(text.len());
  let units: usize = text
    .char_indices()
    .take_while(|(index, _)| *index < end)
    .map(|(_, character)| character.len_utf16())
    .sum();
  to_u32(units)
}

fn to_u32(value: usize) -> u32 {
  u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;

  mod opening {
    use super::*;

    #[test]
    fn a_csv_is_sniffed_and_read_as_a_table() {
      let preview = open_bytes(b"id;name;qty\n1;Anna;2,5\n2;Ben;10\n", 1 << 20).unwrap();
      assert_eq!(preview.info().maybe_detected, Some(DelimiterChoice::Semicolon));
      let table = preview.build_table(None).unwrap();
      assert_eq!((table.rows, table.columns), (3, 3));
      assert!(table.looks_like_header);
      assert_eq!(
        preview.table_rows(1, 5),
        [["1", "Anna", "2,5"], ["2", "Ben", "10"]]
      );
      assert!(table.columns_profile[2].is_numeric);
    }

    #[test]
    fn a_picked_or_typed_separator_replaces_the_sniffed_one() {
      let preview = open_bytes(b"a::b::c\n1::2::3\n", 1 << 20).unwrap();
      let custom = DelimiterChoice::Custom { text: "::".into() };
      let table = preview.build_table(Some(custom)).unwrap();
      assert_eq!(table.columns, 3);
      assert_eq!(preview.table_cell(1, 2), "3");
      let bad = DelimiterChoice::Custom { text: "\"".into() };
      assert_eq!(
        preview.build_table(Some(bad)).unwrap_err(),
        TextPreviewError::BadSeparator
      );
    }

    #[test]
    fn prose_is_no_table_and_binary_is_no_text() {
      let preview = open_bytes(b"Hello there.\nThis is a note.\n", 1 << 20).unwrap();
      assert_eq!(preview.info().maybe_detected, None);
      assert_eq!(
        preview.build_table(None).unwrap_err(),
        TextPreviewError::NotATable
      );
      assert_eq!(
        open_bytes(&[0, 1, 2, 0, 0, 9, 0, 0xFF, 0, 0, 3], 1 << 20).err(),
        Some(TextPreviewError::NotText)
      );
    }

    #[test]
    fn an_unclosed_quote_is_named_by_its_line() {
      let preview = open_bytes(b"a,b\n1,\"two\n3,4\n", 1 << 20).unwrap();
      let error = preview.build_table(Some(DelimiterChoice::Comma)).unwrap_err();
      assert_eq!(error, TextPreviewError::UnterminatedQuote { line: 2 });
    }

    #[test]
    fn a_file_over_the_limit_is_cut_at_a_line_break() {
      let preview = open_bytes(b"a,b\n1,2\n3,4\n5,6\n", 11).unwrap();
      let info = preview.info();
      assert!(info.is_truncated);
      assert_eq!(info.line_count, 2);
      assert!(!open_bytes(b"a,b\n1,2\n", 8).unwrap().info().is_truncated);
    }

    #[test]
    fn a_missing_file_is_unreadable() {
      assert_eq!(
        open("/nonexistent/teamcraft-preview", 10).err(),
        Some(TextPreviewError::Unreadable)
      );
    }
  }

  mod finding {
    use super::*;

    #[test]
    fn matches_are_utf16_offsets_within_the_line() {
      let preview = open_bytes("ab\n😀 Käse\n".as_bytes(), 1 << 20).unwrap();
      let found = preview.find_in_lines("käse");
      assert_eq!(
        found.matches,
        [LineMatch {
          line: 1,
          start: 3,
          end: 7
        }]
      );
      assert_eq!(preview.lines(0, 9), ["ab", "😀 Käse"]);
    }

    #[test]
    fn cells_are_found_and_copied_whole() {
      let preview = open_bytes(b"k,v\nx,\"he said \"\"Hi\"\"\"\ny,hi\n", 1 << 20).unwrap();
      preview.build_table(None).unwrap();
      let (found, _) = preview.current_table().unwrap().find(&preview.text, "hi", 10);
      assert_eq!(found.len(), 2);
      assert_eq!(preview.table_cell(1, 1), "he said \"Hi\"");
      assert_eq!(preview.table_row_text(2), "y\thi");
    }
  }

  #[test]
  fn long_cells_and_lines_are_clipped_for_the_screen() {
    assert_eq!(clip("abcdef", 3), "abc…");
    assert_eq!(clip("abc", 3), "abc");
  }

  fn open_bytes(bytes: &[u8], max_bytes: u64) -> Result<Arc<TextPreview>, TextPreviewError> {
    open_reader(bytes, max_bytes)
  }
}
