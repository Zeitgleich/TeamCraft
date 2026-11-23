//! Delimited text as a table (#2341): CSV, TSV, semicolon-separated (spreadsheet exports in locales
//! that write a decimal comma) and pipe-separated files.
//!
//! The dialect is sniffed from the start of the file: the delimiter and quote character whose rows
//! have the most consistent number of fields. Fields follow RFC 4180 (quoted fields may hold
//! delimiters, doubled quotes and line breaks) and are read leniently where files break it (a
//! quote inside an unquoted field is text). Rows are parsed in steps so a large file never blocks
//! the page; the cells are byte ranges into the file's text, so a table costs a few bytes per cell
//! besides the text itself, and is capped at [`MAX_ROWS`] rows and [`MAX_CELLS`] cells.
//!
//! A file that is no table after all (rows of different lengths, a quote never closed) is refused
//! with a [`TableError`]; the viewer shows it as text then.

use std::collections::HashSet;

use super::Span;
use super::search::{self, Found};

/// Rows of a table at most; the rest is left to the download. The same as the text view's lines.
pub const MAX_ROWS: usize = super::MAX_LINES;
/// Cells of a table at most: some 12 bytes each, so the cells of a table stay below 30 MiB.
pub const MAX_CELLS: usize = 2_000_000;
/// Columns shown at most; a row's further fields are dropped (and the view says so).
pub const MAX_COLUMNS: usize = 256;
/// Rows the column widths, number columns and the header guess look at.
pub const PROFILE_ROWS: usize = 1_000;
/// The widest a column starts, in characters (it can be dragged wider).
pub const MAX_WIDTH_CHARS: u32 = 40;
/// The narrowest a column starts, in characters.
pub const MIN_WIDTH_CHARS: u32 = 3;
/// The longest separator a user can type, in bytes.
pub const MAX_CUSTOM_DELIMITER_BYTES: usize = 8;
/// Records the sniffer reads at most, and the bytes after which it stops at the next record.
const SAMPLE_RECORDS: usize = 100;
const SAMPLE_BYTES: usize = 64 * 1024;
/// The share of rows (in percent) that must have the usual number of fields for a table.
const MIN_CONSISTENT_PERCENT: usize = 90;
/// A header cell is a name, not a sentence: longer first-row cells make it a data row.
const MAX_HEADER_CHARS: usize = 64;
/// Characters of a cell counted for its column's width.
const WIDTH_SAMPLE_CHARS: usize = 200;

/// A parsed table: its rows of cells, as ranges into the file's text.
#[derive(Debug, Default)]
pub struct Table {
  cells: Vec<Cell>,
  /// Where each row's cells start in `cells`, with the end as the last entry.
  row_starts: Vec<u32>,
  /// Quoted cells with doubled quotes, unescaped; [`Cell::is_unescaped`] cells point in here.
  unescaped: String,
  /// The most cells any row has (at most [`MAX_COLUMNS`]).
  pub columns: usize,
  /// The file has more rows (or cells) than a table holds; only the first are shown.
  pub is_truncated: bool,
  /// Some row has more than [`MAX_COLUMNS`] fields; the rest of it is not shown.
  pub has_dropped_columns: bool,
}

/// How a file separates its fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Dialect {
  pub delimiter: Delimiter,
  pub quote: Quote,
}

/// What stands between fields: one of the usual characters (sniffed), or any short text the
/// user types ("::", "; ", a space).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Delimiter {
  Comma,
  Semicolon,
  Tab,
  Pipe,
  Colon,
  Custom(CustomDelimiter),
}

/// A separator the user typed: up to [`MAX_CUSTOM_DELIMITER_BYTES`] bytes, without line breaks or
/// quote characters.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CustomDelimiter {
  bytes: [u8; MAX_CUSTOM_DELIMITER_BYTES],
  len: u8,
}

/// The character around fields that hold delimiters, quotes or line breaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Quote {
  Double,
  Single,
}

/// How a column reads, from its first rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnProfile {
  /// Most of its cells are numbers: they are aligned at the end.
  pub is_numeric: bool,
  /// The characters of its longest cell, within [`MIN_WIDTH_CHARS`] and [`MAX_WIDTH_CHARS`].
  pub width_chars: u32,
}

/// One match of a find, within a cell's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellMatch {
  pub row: u32,
  pub column: u32,
  /// Byte range within the cell's text.
  pub span: Span,
}

/// Parses a text in steps: [`Self::step`] until it says done, then [`Self::finish`].
#[derive(Debug)]
pub struct TableParser {
  dialect: Dialect,
  at: usize,
  table: Table,
  fields: Vec<Field>,
  is_done: bool,
}

/// Why a text is not shown as a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TableError {
  /// No delimiter gives the rows a consistent number of fields.
  #[error("the text has no consistent delimiter")]
  NotDelimited,
  /// A quoted field starting on this line (1-based) is never closed.
  #[error("a quoted field from line {line} is never closed")]
  UnterminatedQuote { line: u32 },
  /// Too many rows differ from the usual number of fields.
  #[error("{differing} of {rows} rows have a different number of fields")]
  Ragged { differing: usize, rows: usize },
}

/// One cell: a range of the file's text, or of [`Table::unescaped`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
  span: Span,
  is_unescaped: bool,
}

/// A field as the record parser finds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Field {
  span: Span,
  /// Quoted, with doubled quotes inside that read as one.
  has_doubled_quotes: bool,
}

/// What the sniffer found for one dialect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sniffed {
  pub dialect: Dialect,
  /// The usual number of fields.
  pub fields: usize,
  /// Records read (blank lines left out).
  pub records: usize,
  /// The share of them with the usual number of fields, in percent.
  pub consistent_percent: usize,
}

impl Delimiter {
  /// The separators the sniffer tries, in the order of preference when two give equally
  /// consistent rows: a tab or semicolon is rarely inside a field by chance, a comma often is (as
  /// a decimal comma), a colon in times and addresses.
  pub const SNIFFED: [Delimiter; 5] = [Self::Tab, Self::Semicolon, Self::Comma, Self::Pipe, Self::Colon];

  pub fn as_bytes(&self) -> &[u8] {
    match self {
      Self::Comma => b",",
      Self::Semicolon => b";",
      Self::Tab => b"\t",
      Self::Pipe => b"|",
      Self::Colon => b":",
      Self::Custom(custom) => custom.as_str().as_bytes(),
    }
  }

  /// The name of a sniffed separator; `None` for a typed one.
  pub fn slug(self) -> Option<&'static str> {
    match self {
      Self::Comma => Some("comma"),
      Self::Semicolon => Some("semicolon"),
      Self::Tab => Some("tab"),
      Self::Pipe => Some("pipe"),
      Self::Colon => Some("colon"),
      Self::Custom(_) => None,
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::SNIFFED
      .into_iter()
      .find(|delimiter| delimiter.slug() == Some(slug))
  }
}

impl CustomDelimiter {
  /// `text` as a separator; `None` when it is empty, too long, or holds a line break or a quote.
  pub fn new(text: &str) -> Option<Self> {
    let is_allowed =
      !text.is_empty() && text.len() <= MAX_CUSTOM_DELIMITER_BYTES && !text.contains(['\n', '\r', '"', '\'']);
    if !is_allowed {
      return None;
    }
    let mut bytes = [0; MAX_CUSTOM_DELIMITER_BYTES];
    bytes[..text.len()].copy_from_slice(text.as_bytes());
    Some(Self {
      bytes,
      // Invariant: at most MAX_CUSTOM_DELIMITER_BYTES (8), checked above.
      len: text.len() as u8,
    })
  }

  pub fn as_str(&self) -> &str {
    // Invariant: the bytes were copied from a `&str` in `new`, so they are UTF-8.
    std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or_default()
  }
}

impl std::fmt::Debug for CustomDelimiter {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(formatter, "CustomDelimiter({:?})", self.as_str())
  }
}

impl Quote {
  pub const ALL: [Quote; 2] = [Self::Double, Self::Single];

  pub fn byte(self) -> u8 {
    match self {
      Self::Double => b'"',
      Self::Single => b'\'',
    }
  }
}

impl Dialect {
  pub fn new(delimiter: Delimiter) -> Self {
    Self {
      delimiter,
      quote: Quote::Double,
    }
  }
}

/// The dialect whose first records have the most consistent number of fields (at least two, in at
/// least [`MIN_CONSISTENT_PERCENT`] of them); `None` for text that is no table.
pub fn sniff(text: &str) -> Option<Sniffed> {
  let mut best: Option<Sniffed> = None;
  for delimiter in Delimiter::SNIFFED {
    for quote in Quote::ALL {
      let Some(found) = sniff_dialect(text, Dialect { delimiter, quote }) else {
        continue;
      };
      let is_better = best.is_none_or(|best| found.consistent_percent > best.consistent_percent);
      if is_better {
        best = Some(found);
      }
    }
  }
  best.filter(|best| best.consistent_percent >= MIN_CONSISTENT_PERCENT)
}

/// How consistent the first records are in `dialect`; `None` when they have one field each.
pub fn sniff_dialect(text: &str, dialect: Dialect) -> Option<Sniffed> {
  let mut counts: Vec<usize> = Vec::new();
  let mut fields = Vec::new();
  let mut at = 0;
  while at < text.len() && counts.len() < SAMPLE_RECORDS && at < SAMPLE_BYTES {
    match next_record(text, at, dialect, &mut fields) {
      Ok(next) => {
        if !is_blank(&fields) {
          counts.push(fields.len());
        }
        at = next;
      },
      // A quote never closed: the records before it decide; parsing will refuse the file.
      Err(_) => break,
    }
  }
  let mode = mode_of(&counts)?;
  if mode.0 < 2 {
    return None;
  }
  Some(Sniffed {
    dialect,
    fields: mode.0,
    records: counts.len(),
    consistent_percent: mode.1 * 100 / counts.len(),
  })
}

impl TableParser {
  pub fn new(dialect: Dialect) -> Self {
    Self {
      dialect,
      at: 0,
      table: Table {
        row_starts: vec![0],
        ..Table::default()
      },
      fields: Vec::new(),
      is_done: false,
    }
  }

  /// Parses up to `max_rows` more rows of `text` (always the same text); `true` once it is done:
  /// at the end of the text or at the table's limits.
  pub fn step(&mut self, text: &str, max_rows: usize) -> Result<bool, TableError> {
    let mut parsed = 0;
    while !self.is_done && parsed < max_rows {
      if self.at >= text.len() {
        self.is_done = true;
        break;
      }
      if self.table.rows() == MAX_ROWS || self.table.cells.len() >= MAX_CELLS {
        self.table.is_truncated = true;
        self.is_done = true;
        break;
      }
      let start = self.at;
      self.at = next_record(text, start, self.dialect, &mut self.fields).map_err(|quote_at| {
        TableError::UnterminatedQuote {
          line: line_of(text, quote_at),
        }
      })?;
      if !is_blank(&self.fields) {
        self.table.push_row(text, self.dialect.quote, &self.fields);
        parsed += 1;
      }
    }
    Ok(self.is_done)
  }

  /// The table, unless too many of its rows differ from the usual number of fields.
  pub fn finish(self) -> Result<Table, TableError> {
    let table = self.table;
    let lengths: Vec<usize> = (0..table.rows()).map(|row| table.row_len(row)).collect();
    if let Some((_, usual)) = mode_of(&lengths) {
      let differing = lengths.len() - usual;
      if differing * 100 > lengths.len() * (100 - MIN_CONSISTENT_PERCENT) {
        return Err(TableError::Ragged {
          differing,
          rows: lengths.len(),
        });
      }
    }
    Ok(table)
  }
}

impl Table {
  /// A whole text parsed at once (tests and small files).
  pub fn parse(text: &str, dialect: Dialect) -> Result<Self, TableError> {
    let mut parser = TableParser::new(dialect);
    while !parser.step(text, usize::MAX)? {}
    parser.finish()
  }

  pub fn rows(&self) -> usize {
    self.row_starts.len().saturating_sub(1)
  }

  /// The cells row `row` has (fewer than [`Self::columns`] in a short row).
  pub fn row_len(&self, row: usize) -> usize {
    self.row_range(row).len()
  }

  /// The text of a cell; `None` past the end of its row.
  pub fn cell<'a>(&'a self, text: &'a str, row: usize, column: usize) -> Option<&'a str> {
    let range = self.row_range(row);
    let cell = self
      .cells
      .get(range.start + column)
      .filter(|_| column < range.len())?;
    let source = if cell.is_unescaped {
      self.unescaped.as_str()
    } else {
      text
    };
    Some(source.get(cell.span.range()).unwrap_or_default())
  }

  /// A row's cells, separated by tabs (what a spreadsheet pastes as one row). Tabs and line breaks
  /// inside a cell become spaces.
  pub fn row_text(&self, text: &str, row: usize) -> String {
    (0..self.row_len(row))
      .map(|column| flatten(self.cell(text, row, column).unwrap_or_default()))
      .collect::<Vec<_>>()
      .join("\t")
  }

  /// Every match of `query` in the cells, ignoring case, at most `cap`, in reading order.
  pub fn find(&self, text: &str, query: &str, cap: usize) -> (Vec<CellMatch>, bool) {
    if query.is_empty() {
      return (Vec::new(), false);
    }
    let in_text = self.find_in(text, false, query, cap);
    let in_unescaped = self.find_in(&self.unescaped, true, query, cap);
    let mut merged: Vec<(u32, Span)> = in_text
      .matches
      .iter()
      .chain(&in_unescaped.matches)
      .map(|found| (found.line, found.span))
      .collect();
    merged.sort_by_key(|(index, span)| (*index, span.start));
    let is_capped = in_text.is_capped || in_unescaped.is_capped || merged.len() > cap;
    merged.truncate(cap);
    let matches = merged
      .into_iter()
      .map(|(index, span)| {
        let row = self.row_of_cell(index as usize);
        let column = index - self.row_starts[row];
        let cell_start = self.cells[index as usize].span.start;
        CellMatch {
          row: u32::try_from(row).unwrap_or(u32::MAX),
          column,
          span: Span {
            start: span.start - cell_start,
            end: span.end - cell_start,
          },
        }
      })
      .collect();
    (matches, is_capped)
  }

  /// Whether the first row reads like column names: every cell filled, no numbers, no repeats,
  /// none longer than a name, and more than one row.
  pub fn looks_like_header(&self, text: &str) -> bool {
    if self.rows() < 2 || self.row_len(0) == 0 {
      return false;
    }
    let mut seen = HashSet::new();
    (0..self.row_len(0)).all(|column| {
      let cell = self.cell(text, 0, column).unwrap_or_default().trim();
      !cell.is_empty()
        && cell.chars().count() <= MAX_HEADER_CHARS
        && !is_numeric(cell)
        && seen.insert(cell.to_lowercase())
    })
  }

  /// Each column's alignment and starting width, from the first [`PROFILE_ROWS`] rows (the
  /// header, when there is one, counts for the width but not the alignment).
  pub fn profile(&self, text: &str, has_header: bool) -> Vec<ColumnProfile> {
    let rows = self.rows().min(PROFILE_ROWS);
    let body_from = usize::from(has_header);
    (0..self.columns)
      .map(|column| {
        let mut widest = 0;
        let (mut numbers, mut filled) = (0usize, 0usize);
        for row in 0..rows {
          let cell = self.cell(text, row, column).unwrap_or_default();
          widest = widest.max(width_of(cell));
          let cell = cell.trim();
          if row >= body_from && !cell.is_empty() {
            filled += 1;
            numbers += usize::from(is_numeric(cell));
          }
        }
        ColumnProfile {
          is_numeric: filled > 0 && numbers * 10 >= filled * 8,
          width_chars: widest.clamp(MIN_WIDTH_CHARS, MAX_WIDTH_CHARS),
        }
      })
      .collect()
  }

  /// The lines row `row` takes when each cell wraps at its column's `column_chars` characters, at
  /// most `max_lines` (the view clips a taller cell).
  pub fn wrapped_lines(&self, text: &str, row: usize, column_chars: &[u32], max_lines: u32) -> u32 {
    (0..self.row_len(row))
      .map(|column| {
        let wrap = column_chars.get(column).copied().unwrap_or(1).max(1);
        let cell = self.cell(text, row, column).unwrap_or_default();
        let mut lines = 0u32;
        for line in cell.lines() {
          let chars = line.chars().take((wrap * max_lines) as usize).count();
          lines = lines.saturating_add(u32::try_from(chars).unwrap_or(u32::MAX).div_ceil(wrap).max(1));
          if lines >= max_lines {
            break;
          }
        }
        lines.max(1)
      })
      .max()
      .unwrap_or(1)
      .min(max_lines)
  }

  /// Rows of up to `columns` cells in the order given; for tests.
  #[cfg(test)]
  fn texts(&self, text: &str) -> Vec<Vec<String>> {
    (0..self.rows())
      .map(|row| {
        (0..self.row_len(row))
          .map(|column| self.cell(text, row, column).unwrap_or_default().to_owned())
          .collect()
      })
      .collect()
  }

  fn push_row(&mut self, text: &str, quote: Quote, fields: &[Field]) {
    if fields.len() > MAX_COLUMNS {
      self.has_dropped_columns = true;
    }
    let room = MAX_CELLS.saturating_sub(self.cells.len());
    for field in fields.iter().take(MAX_COLUMNS.min(room)) {
      let cell = if field.has_doubled_quotes {
        let raw = text.get(field.span.range()).unwrap_or_default();
        let quote = char::from(quote.byte());
        let start = self.unescaped.len();
        for (index, part) in raw.split(quote).enumerate() {
          // Pieces between quotes: every second quote of a doubled pair is dropped.
          if index > 0 && index % 2 == 0 {
            self.unescaped.push(quote);
          }
          self.unescaped.push_str(part);
        }
        Cell {
          span: Span::new(start, self.unescaped.len()),
          is_unescaped: true,
        }
      } else {
        Cell {
          span: field.span,
          is_unescaped: false,
        }
      };
      self.cells.push(cell);
    }
    let pushed = fields.len().min(MAX_COLUMNS).min(room);
    self.columns = self.columns.max(pushed);
    self
      .row_starts
      .push(u32::try_from(self.cells.len()).unwrap_or(u32::MAX));
  }

  fn row_range(&self, row: usize) -> std::ops::Range<usize> {
    match (self.row_starts.get(row), self.row_starts.get(row + 1)) {
      (Some(&start), Some(&end)) => start as usize..end as usize,
      _ => 0..0,
    }
  }

  fn row_of_cell(&self, index: usize) -> usize {
    let index = u32::try_from(index).unwrap_or(u32::MAX);
    self
      .row_starts
      .partition_point(|start| *start <= index)
      .saturating_sub(1)
  }

  /// Matches in the cells that live in `source` (the text, or the unescaped cells); a match's
  /// "line" is the cell's index.
  fn find_in(&self, source: &str, is_unescaped: bool, query: &str, cap: usize) -> Found {
    let cells = self
      .cells
      .iter()
      .enumerate()
      .filter(|(_, cell)| cell.is_unescaped == is_unescaped)
      .map(|(index, cell)| (u32::try_from(index).unwrap_or(u32::MAX), cell.span));
    search::find_all(source, cells, query, cap)
  }
}

/// Reads the record at byte `at` into `fields`; returns where the next record starts, or, for a
/// quote that is never closed, `Err` with where it opened.
fn next_record(text: &str, at: usize, dialect: Dialect, fields: &mut Vec<Field>) -> Result<usize, usize> {
  let bytes = text.as_bytes();
  let delimiter = dialect.delimiter.as_bytes();
  let quote = dialect.quote.byte();
  let first = delimiter.first().copied().unwrap_or(b',');
  let is_delimiter_at = |i: usize| bytes.get(i..).is_some_and(|rest| rest.starts_with(delimiter));
  let is_end_at = |i: usize| match bytes.get(i) {
    None | Some(b'\n' | b'\r') => true,
    Some(&byte) => byte == first && is_delimiter_at(i),
  };
  let scan = |from: usize| {
    let mut i = from;
    while let Some(found) = bytes[i..]
      .iter()
      .position(|&byte| byte == first || byte == b'\n' || byte == b'\r')
    {
      if is_end_at(i + found) {
        return i + found;
      }
      i += found + 1;
    }
    bytes.len()
  };
  fields.clear();
  let mut i = at;
  loop {
    let field_start = i;
    if bytes.get(i) == Some(&quote) {
      let content_start = i + 1;
      let mut j = content_start;
      let mut has_doubled_quotes = false;
      let content_end = loop {
        let Some(found) = bytes[j..].iter().position(|&byte| byte == quote) else {
          return Err(field_start);
        };
        let close = j + found;
        if bytes.get(close + 1) == Some(&quote) {
          has_doubled_quotes = true;
          j = close + 2;
        } else {
          break close;
        }
      };
      i = content_end + 1;
      if is_end_at(i) {
        fields.push(Field {
          span: Span::new(content_start, content_end),
          has_doubled_quotes,
        });
      } else {
        // Text after the closing quote: not RFC 4180; the field is shown as it is written.
        i = scan(i);
        fields.push(Field {
          span: Span::new(field_start, i),
          has_doubled_quotes: false,
        });
      }
    } else {
      i = scan(i);
      fields.push(Field {
        span: Span::new(field_start, i),
        has_doubled_quotes: false,
      });
    }
    match bytes.get(i) {
      None => return Ok(bytes.len()),
      Some(b'\r') if bytes.get(i + 1) == Some(&b'\n') => return Ok(i + 2),
      Some(b'\n' | b'\r') => return Ok(i + 1),
      Some(_) => i += delimiter.len(),
    }
  }
}

/// A blank line: one empty, unquoted field.
fn is_blank(fields: &[Field]) -> bool {
  matches!(fields, [only] if only.span.is_empty() && !only.has_doubled_quotes)
}

/// The most common value and how often it occurs; ties go to the larger value.
fn mode_of(values: &[usize]) -> Option<(usize, usize)> {
  let mut sorted = values.to_vec();
  sorted.sort_unstable();
  sorted
    .chunk_by(|a, b| a == b)
    .map(|run| (run[0], run.len()))
    .max_by_key(|(value, count)| (*count, *value))
}

/// The 1-based line of byte `at`, counting `\n`, `\r\n` and a lone `\r` as one break each.
fn line_of(text: &str, at: usize) -> u32 {
  let before = &text.as_bytes()[..at.min(text.len())];
  let breaks = before
    .iter()
    .enumerate()
    .filter(|(index, byte)| **byte == b'\n' || (**byte == b'\r' && before.get(index + 1) != Some(&b'\n')))
    .count();
  u32::try_from(breaks + 1).unwrap_or(u32::MAX)
}

/// The width a cell asks for, in characters: its longest line, counting at most
/// [`WIDTH_SAMPLE_CHARS`] characters.
pub fn width_of(cell: &str) -> u32 {
  let mut widest = 0u32;
  let mut current = 0u32;
  for character in cell.chars().take(WIDTH_SAMPLE_CHARS) {
    if character == '\n' || character == '\r' {
      current = 0;
    } else {
      current += 1;
      widest = widest.max(current);
    }
  }
  widest
}

/// Whether a cell is a number: digits with an optional sign, decimal point or comma, digit group
/// separators, exponent and percent sign ("-1.234,5", "12 %", "3e-4").
pub fn is_numeric(cell: &str) -> bool {
  let cell = cell.trim();
  let cell = cell.strip_suffix('%').map_or(cell, str::trim_end);
  let cell = cell.strip_prefix(['-', '+', '\u{2212}']).unwrap_or(cell);
  let (mantissa, maybe_exponent) = match cell.split_once(['e', 'E']) {
    Some((mantissa, exponent)) => (mantissa, Some(exponent)),
    None => (cell, None),
  };
  let is_exponent_ok = maybe_exponent.is_none_or(|exponent| {
    let digits = exponent.strip_prefix(['-', '+']).unwrap_or(exponent);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
  });
  let is_separator = |c: char| matches!(c, '.' | ',' | ' ' | '\'' | '\u{a0}' | '\u{202f}');
  let starts_ok = mantissa.starts_with(|c: char| c.is_ascii_digit())
    || (mantissa.starts_with(['.', ',']) && mantissa[1..].starts_with(|c: char| c.is_ascii_digit()));
  is_exponent_ok
    && starts_ok
    && mantissa.ends_with(|c: char| c.is_ascii_digit())
    && mantissa.chars().all(|c| c.is_ascii_digit() || is_separator(c))
}

/// A cell on one line: tabs and line breaks become spaces.
fn flatten(cell: &str) -> String {
  cell.replace("\r\n", " ").replace(['\n', '\r', '\t'], " ")
}

#[cfg(test)]
mod tests {
  use super::*;

  const SEMICOLONS: &str = "serial;point;location;status\n\
    SN0001;P-01;Location A;ok\n\
    SN0002;P-02;\"Location B; north\";\"says \"\"hi\"\"\"\n\
    SN0003;P-03;Location C;\n";

  mod sniffing {
    use super::*;

    #[test]
    fn semicolons_win_over_commas_inside_fields() {
      let text = "name;price;note\nBolt;1,50;small, steel\nNut;0,20;tiny, brass\nWasher;0,05;flat\n";
      assert_eq!(delimiter_of(text), Some(Delimiter::Semicolon));
    }

    #[test]
    fn every_delimiter_is_found() {
      assert_eq!(delimiter_of("a,b,c\n1,2,3\n4,5,6\n"), Some(Delimiter::Comma));
      assert_eq!(delimiter_of("a\tb\n1\t2\n"), Some(Delimiter::Tab));
      assert_eq!(delimiter_of("a|b|c\n1|2|3\n"), Some(Delimiter::Pipe));
      assert_eq!(
        delimiter_of("key:value:unit\nx:1:m\ny:2:s\n"),
        Some(Delimiter::Colon)
      );
      assert_eq!(delimiter_of(SEMICOLONS), Some(Delimiter::Semicolon));
    }

    #[test]
    fn quoted_delimiters_and_line_breaks_dont_break_the_count() {
      let text = "id,comment\n1,\"one, two\nthree\"\n2,plain\n3,\"x\"\n";
      let found = sniff(text).expect("a table");
      assert_eq!(found.dialect, Dialect::new(Delimiter::Comma));
      assert_eq!(found.consistent_percent, 100);
      assert_eq!(found.records, 4);
    }

    #[test]
    fn single_quotes_are_found_when_they_quote() {
      let text = "a,b\n'x, y',1\n'z, w',2\n";
      assert_eq!(sniff(text).map(|found| found.dialect.quote), Some(Quote::Single));
    }

    #[test]
    fn prose_is_no_table() {
      assert_eq!(
        sniff("Hello, world.\nThis line has none\nBut this, and this, too.\n"),
        None
      );
      assert_eq!(sniff("just one line"), None);
      assert_eq!(sniff(""), None);
    }
  }

  mod parsing {
    use super::*;

    #[test]
    fn quoted_fields_hold_delimiters_quotes_and_empty_cells() {
      let table = Table::parse(SEMICOLONS, Dialect::new(Delimiter::Semicolon)).expect("parses");
      assert_eq!(table.columns, 4);
      assert_eq!(
        table.texts(SEMICOLONS)[2],
        ["SN0002", "P-02", "Location B; north", "says \"hi\""]
      );
      assert_eq!(table.cell(SEMICOLONS, 3, 3), Some(""));
      assert_eq!(table.cell(SEMICOLONS, 3, 4), None);
    }

    #[test]
    fn quoted_line_breaks_stay_in_their_cell() {
      let text = "a,b\r\n\"line one\r\nline two\",2\r\n";
      let table = Table::parse(text, Dialect::new(Delimiter::Comma)).expect("parses");
      assert_eq!(table.rows(), 2);
      assert_eq!(table.cell(text, 1, 0), Some("line one\r\nline two"));
    }

    #[test]
    fn mixed_line_endings_and_blank_lines() {
      let text = "a,b\r\n1,2\n3,4\r\n\n5,6\r7,8";
      let table = Table::parse(text, Dialect::new(Delimiter::Comma)).expect("parses");
      assert_eq!(
        table.texts(text),
        [["a", "b"], ["1", "2"], ["3", "4"], ["5", "6"], ["7", "8"]]
      );
    }

    #[test]
    fn a_trailing_delimiter_makes_an_empty_last_cell() {
      let text = "a,b,\n1,2,";
      let table = Table::parse(text, Dialect::new(Delimiter::Comma)).expect("parses");
      assert_eq!(table.texts(text)[1], ["1", "2", ""]);
    }

    #[test]
    fn stray_quotes_are_text() {
      let text = "a,b\n5\" pipe,\"x\"y\n";
      let table = Table::parse(text, Dialect::new(Delimiter::Comma)).expect("parses");
      assert_eq!(table.texts(text)[1], ["5\" pipe", "\"x\"y"]);
    }

    #[test]
    fn a_typed_separator_of_several_characters_splits_fields() {
      let text = "a::b::c\n1::\"x::y\"::3\n4:5::6::\n";
      let custom = CustomDelimiter::new("::").expect("valid");
      let table = Table::parse(text, Dialect::new(Delimiter::Custom(custom))).expect("parses");
      assert_eq!(
        table.texts(text),
        [["a", "b", "c"], ["1", "x::y", "3"], ["4:5", "6", ""]]
      );
      let spaced = "Location A; 1\nLocation B; 2\n";
      let custom = CustomDelimiter::new("; ").expect("valid");
      let table = Table::parse(spaced, Dialect::new(Delimiter::Custom(custom))).expect("parses");
      assert_eq!(table.texts(spaced)[1], ["Location B", "2"]);
    }

    #[test]
    fn typed_separators_are_checked() {
      assert_eq!(
        CustomDelimiter::new(" ").map(|custom| custom.as_str().to_owned()),
        Some(" ".to_owned())
      );
      assert_eq!(
        CustomDelimiter::new("§").map(|custom| custom.as_str().to_owned()),
        Some("§".to_owned())
      );
      for refused in ["", "\n", "a\rb", "\"", "'", "123456789"] {
        assert_eq!(CustomDelimiter::new(refused), None, "{refused:?}");
      }
    }

    #[test]
    fn an_unterminated_quote_is_refused_with_its_line() {
      let text = "a,b\n1,2\n3,\"never closed\n4,5\n";
      assert_eq!(
        Table::parse(text, Dialect::new(Delimiter::Comma)).map(|_| ()),
        Err(TableError::UnterminatedQuote { line: 3 })
      );
    }

    #[test]
    fn ragged_rows_are_refused_but_a_few_are_shown() {
      let ragged = "a,b,c\n1,2\n3\n4,5,6,7\n8,9,10\n";
      assert!(matches!(
        Table::parse(ragged, Dialect::new(Delimiter::Comma)),
        Err(TableError::Ragged {
          differing: 3,
          rows: 5
        })
      ));
      let mostly: String = std::iter::once("a,b,c\n".to_owned())
        .chain((0..20).map(|n| format!("{n},x,y\n")))
        .chain(std::iter::once("short,row\n".to_owned()))
        .collect();
      let table = Table::parse(&mostly, Dialect::new(Delimiter::Comma)).expect("parses");
      assert_eq!(table.columns, 3);
      assert_eq!(table.row_len(21), 2);
    }

    #[test]
    fn an_empty_text_is_an_empty_table() {
      let table = Table::parse("", Dialect::new(Delimiter::Comma)).expect("parses");
      assert_eq!(table.rows(), 0);
      assert_eq!(table.columns, 0);
    }

    #[test]
    fn a_truncated_file_keeps_its_complete_rows() {
      let text = "a,b\n1,2\n3,";
      let table = Table::parse(text, Dialect::new(Delimiter::Comma)).expect("parses");
      assert_eq!(table.texts(text)[2], ["3", ""]);
    }

    #[test]
    fn parsing_in_steps_gives_the_same_table() {
      let mut parser = TableParser::new(Dialect::new(Delimiter::Semicolon));
      while !parser.step(SEMICOLONS, 1).expect("parses") {}
      let stepped = parser.finish().expect("consistent");
      let whole = Table::parse(SEMICOLONS, Dialect::new(Delimiter::Semicolon)).expect("parses");
      assert_eq!(stepped.texts(SEMICOLONS), whole.texts(SEMICOLONS));
    }

    #[test]
    fn oversized_rows_drop_their_extra_columns() {
      let row = vec!["x"; MAX_COLUMNS + 10].join(",");
      let text = format!("{row}\n{row}\n");
      let table = Table::parse(&text, Dialect::new(Delimiter::Comma)).expect("parses");
      assert_eq!(table.columns, MAX_COLUMNS);
      assert!(table.has_dropped_columns);
    }

    #[test]
    fn a_huge_file_stops_at_the_cell_limit() {
      let text = "a,b,c,d\n".repeat(MAX_CELLS / 4 + 10);
      let table = Table::parse(&text, Dialect::new(Delimiter::Comma)).expect("parses");
      assert!(table.is_truncated);
      assert_eq!(table.rows(), MAX_CELLS / 4);
    }

    #[test]
    fn a_row_copies_as_tab_separated_text() {
      let text = "a,\"b\tc\nd\",e\n1,2,3\n";
      let table = Table::parse(text, Dialect::new(Delimiter::Comma)).expect("parses");
      assert_eq!(table.row_text(text, 0), "a\tb c d\te");
    }
  }

  mod reading {
    use super::*;

    #[test]
    fn a_first_row_of_names_is_a_header() {
      let table = Table::parse(SEMICOLONS, Dialect::new(Delimiter::Semicolon)).expect("parses");
      assert!(table.looks_like_header(SEMICOLONS));
    }

    #[test]
    fn a_first_row_with_numbers_empty_or_repeated_cells_is_data() {
      for text in ["a,1\nb,2\n", "a,\nb,c\n", "x,X\n1,2\n", "only,row\n"] {
        let table = Table::parse(text, Dialect::new(Delimiter::Comma)).expect("parses");
        assert!(!table.looks_like_header(text), "{text:?}");
      }
    }

    #[test]
    fn number_columns_and_widths() {
      let text = "name;amount;code\nLocation A;1.234,50;SN0001\nLocation B with a much longer name than fits a column;-3;SN0002\n";
      let table = Table::parse(text, Dialect::new(Delimiter::Semicolon)).expect("parses");
      let profile = table.profile(text, true);
      assert_eq!(
        profile,
        [
          ColumnProfile {
            is_numeric: false,
            width_chars: MAX_WIDTH_CHARS
          },
          ColumnProfile {
            is_numeric: true,
            width_chars: 8
          },
          ColumnProfile {
            is_numeric: false,
            width_chars: 6
          },
        ]
      );
    }

    #[test]
    fn numbers_are_recognised() {
      for number in [
        "0", "-12", "+3.5", "1.234,56", "1 000", ".5", "12 %", "3e-4", "1'000",
      ] {
        assert!(is_numeric(number), "{number}");
      }
      for text in ["", "-", "SN0001", "2026-11-23", "12.", "1e", "abc", "1,2x", "%"] {
        assert!(!is_numeric(text), "{text}");
      }
    }

    #[test]
    fn find_reports_cells_and_places_within_them() {
      let table = Table::parse(SEMICOLONS, Dialect::new(Delimiter::Semicolon)).expect("parses");
      let (matches, is_capped) = table.find(SEMICOLONS, "location", 10);
      assert!(!is_capped);
      let places: Vec<_> = matches
        .iter()
        .map(|found| (found.row, found.column, found.span.start))
        .collect();
      assert_eq!(places, [(0, 2, 0), (1, 2, 0), (2, 2, 0), (3, 2, 0)]);
      // In an unescaped cell, the place is within the shown text.
      let (matches, _) = table.find(SEMICOLONS, "hi", 10);
      assert_eq!(
        matches,
        [CellMatch {
          row: 2,
          column: 3,
          span: Span::new(6, 8)
        }]
      );
    }

    #[test]
    fn wrapped_rows_take_the_lines_their_tallest_cell_needs() {
      let text = "a,b\n\"one\ntwo three\",12345678901\n";
      let table = Table::parse(text, Dialect::new(Delimiter::Comma)).expect("parses");
      assert_eq!(table.wrapped_lines(text, 0, &[5, 5], 8), 1);
      // "one" 1 line, "two three" 2 lines; the number 3 lines.
      assert_eq!(table.wrapped_lines(text, 1, &[5, 5], 8), 3);
      assert_eq!(table.wrapped_lines(text, 1, &[5, 5], 2), 2);
    }

    #[test]
    fn find_is_capped() {
      let table = Table::parse(SEMICOLONS, Dialect::new(Delimiter::Semicolon)).expect("parses");
      let (matches, is_capped) = table.find(SEMICOLONS, "o", 2);
      assert_eq!(matches.len(), 2);
      assert!(is_capped);
    }
  }

  fn delimiter_of(text: &str) -> Option<Delimiter> {
    sniff(text).map(|found| found.dialect.delimiter)
  }
}
