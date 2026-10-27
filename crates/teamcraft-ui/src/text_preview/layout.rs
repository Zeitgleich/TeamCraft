//! What the virtualized view draws: the rows of the lines shown (a filter by level hides lines, a
//! very long line is split into several rows), how many screen rows each takes when lines wrap,
//! which rows a scroll position shows, and how a row is cut into pieces of one look each.
//!
//! Only the rows in view (and a few around them) are in the page, so a file of a million lines
//! costs as much to show as one of a hundred. Rows have the height of one text line; wrapped rows
//! take as many as their characters need at the measured width (the font is monospace and lines
//! break anywhere, so the count is exact but for wide characters and tabs).

use std::ops::Range;

use super::Span;
use super::ansi::{AnsiStyle, StyledRun};
use super::search::Match;
use crate::model::LogLevel;

/// A line longer than this is split into rows of at most this many bytes, so one huge line
/// (minified JSON, a memory dump) never becomes one huge piece of the page.
pub const MAX_ROW_BYTES: usize = 8 * 1024;
/// A tab counts as this many columns (the viewer's `tab-size`).
pub const TAB_COLUMNS: u32 = 4;
/// The scrollable height at most. Browsers stop growing an element at some millions of pixels
/// (Firefox about 17.9); a taller text maps the scroll position onto it.
pub const MAX_CANVAS_PX: f64 = 8_000_000.0;

/// One row of the view: a line, or a part of a long one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
  /// Index into the document's lines.
  pub line: u32,
  pub span: Span,
  /// The first row of its line (the one with the line number).
  pub is_line_start: bool,
}

/// How tall each row is, in screen rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heights {
  /// Lines don't wrap: every row is one screen row.
  Single(usize),
  /// Lines wrap: where each row starts, in screen rows, and the total as the last entry.
  Wrapped(Vec<u32>),
}

/// How the scroll position maps onto the content when the content is taller than
/// [`MAX_CANVAS_PX`] (otherwise one to one).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollMap {
  pub content_px: f64,
  pub canvas_px: f64,
  pub viewport_px: f64,
}

/// The rows a scroll position draws, and where the first of them goes on the canvas.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
  pub rows: Range<usize>,
  pub top_px: f64,
}

/// What a piece of a row looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
  Ansi(AnsiStyle),
  /// A log line's level word.
  Level,
  /// A log line's time.
  Time,
  Match,
  CurrentMatch,
}

/// A decoration over a range of the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deco {
  pub span: Span,
  pub mark: Mark,
}

/// A stretch of a row with one look: the marks of every decoration over it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
  pub span: Span,
  pub marks: Vec<Mark>,
}

/// The rows of the lines whose level is at least `min_level` (all lines without levels), long
/// lines split at character boundaries.
pub fn rows(text: &str, lines: &[Span], maybe_levels: Option<&[LogLevel]>, min_level: LogLevel) -> Vec<Row> {
  let mut rows = Vec::with_capacity(lines.len());
  for (index, span) in lines.iter().enumerate() {
    let is_shown = maybe_levels
      .and_then(|levels| levels.get(index))
      .is_none_or(|level| *level >= min_level);
    if !is_shown {
      continue;
    }
    let line = u32::try_from(index).unwrap_or(u32::MAX);
    let mut start = span.start as usize;
    let end = span.end as usize;
    loop {
      let mut cut = end.min(start + MAX_ROW_BYTES);
      while cut > start && !text.is_char_boundary(cut) {
        cut -= 1;
      }
      rows.push(Row {
        line,
        span: Span::new(start, cut),
        is_line_start: start == span.start as usize,
      });
      if cut >= end || cut == start {
        break;
      }
      start = cut;
    }
  }
  rows
}

/// The columns each row takes.
pub fn columns(text: &str, rows: &[Row]) -> Vec<u32> {
  rows
    .iter()
    .map(|row| text.get(row.span.range()).map_or(0, columns_of))
    .collect()
}

/// The columns `text` takes: its characters, a tab as [`TAB_COLUMNS`].
pub fn columns_of(text: &str) -> u32 {
  text
    .chars()
    .map(|character| if character == '\t' { TAB_COLUMNS } else { 1 })
    .sum()
}

/// The first row (index into `rows`) of `line` or of the first shown line after it.
pub fn row_of_line(rows: &[Row], line: u32) -> Option<usize> {
  let index = rows.partition_point(|row| row.line < line);
  (index < rows.len()).then_some(index)
}

/// The row a match starts in.
pub fn row_of_match(rows: &[Row], found: &Match) -> Option<usize> {
  let index = rows.partition_point(|row| (row.line, row.span.end) <= (found.line, found.span.start));
  rows
    .get(index)
    .filter(|row| row.line == found.line)
    .map(|_| index)
}

impl Heights {
  /// Screen rows per row: one each, or with `wrap_columns` as many as the columns need.
  pub fn new(columns: &[u32], maybe_wrap_columns: Option<u32>) -> Self {
    let Some(wrap) = maybe_wrap_columns.filter(|wrap| *wrap > 0) else {
      return Self::Single(columns.len());
    };
    let mut starts = Vec::with_capacity(columns.len() + 1);
    let mut total = 0u32;
    for &width in columns {
      starts.push(total);
      total = total.saturating_add(width.div_ceil(wrap).max(1));
    }
    starts.push(total);
    Self::Wrapped(starts)
  }

  /// The number of rows.
  pub fn len(&self) -> usize {
    match self {
      Self::Single(count) => *count,
      Self::Wrapped(starts) => starts.len().saturating_sub(1),
    }
  }

  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// Screen rows in all.
  pub fn total(&self) -> u32 {
    match self {
      Self::Single(count) => u32::try_from(*count).unwrap_or(u32::MAX),
      Self::Wrapped(starts) => starts.last().copied().unwrap_or(0),
    }
  }

  /// The screen row `row` starts at.
  pub fn start_of(&self, row: usize) -> u32 {
    match self {
      Self::Single(_) => u32::try_from(row).unwrap_or(u32::MAX),
      Self::Wrapped(starts) => starts.get(row).copied().unwrap_or_else(|| self.total()),
    }
  }

  /// The row that covers screen row `screen_row` (the last row past the end).
  pub fn row_at(&self, screen_row: u32) -> usize {
    let last = self.len().saturating_sub(1);
    match self {
      Self::Single(_) => (screen_row as usize).min(last),
      Self::Wrapped(starts) => starts
        .partition_point(|start| *start <= screen_row)
        .saturating_sub(1)
        .min(last),
    }
  }
}

impl ScrollMap {
  pub fn new(heights: &Heights, row_px: f64, viewport_px: f64) -> Self {
    let content_px = f64::from(heights.total()) * row_px;
    Self {
      content_px,
      canvas_px: content_px.min(MAX_CANVAS_PX),
      viewport_px,
    }
  }

  /// Where in the content a scroll position is.
  pub fn content_top(&self, scroll_top: f64) -> f64 {
    let (scrollable, content) = self.ranges();
    if scrollable <= 0.0 {
      return 0.0;
    }
    scroll_top.clamp(0.0, scrollable) / scrollable * content
  }

  /// The scroll position that shows the content from `content_top`.
  pub fn scroll_top_for(&self, content_top: f64) -> f64 {
    let (scrollable, content) = self.ranges();
    if content <= 0.0 {
      return 0.0;
    }
    content_top.clamp(0.0, content) / content * scrollable
  }

  /// How far the canvas scrolls, and how far the content does.
  fn ranges(&self) -> (f64, f64) {
    (
      (self.canvas_px - self.viewport_px).max(0.0),
      (self.content_px - self.viewport_px).max(0.0),
    )
  }
}

/// The rows to draw at `scroll_top`, with `overscan` screen rows more above and below.
pub fn window(heights: &Heights, map: &ScrollMap, row_px: f64, scroll_top: f64, overscan: u32) -> Window {
  if heights.is_empty() || row_px <= 0.0 {
    return Window {
      rows: 0..0,
      top_px: 0.0,
    };
  }
  let content_top = map.content_top(scroll_top);
  let first_screen_row = screen_row(content_top / row_px);
  let last_screen_row = screen_row(((content_top + map.viewport_px) / row_px).ceil());
  let first = heights.row_at(first_screen_row.saturating_sub(overscan));
  let last = heights.row_at(last_screen_row.saturating_add(overscan));
  let first_px = f64::from(heights.start_of(first)) * row_px;
  Window {
    rows: first..last + 1,
    top_px: (scroll_top.max(0.0) - (content_top - first_px)).max(0.0),
  }
}

/// The content position that puts screen row `row` in the middle of the viewport.
pub fn centered(heights: &Heights, row: usize, row_px: f64, viewport_px: f64) -> f64 {
  (f64::from(heights.start_of(row)) * row_px - (viewport_px - row_px) / 2.0).max(0.0)
}

/// Cuts `row` into pieces where the decorations over it begin and end.
pub fn pieces(row: Span, decos: &[Deco]) -> Vec<Piece> {
  let mut cuts: Vec<u32> = vec![row.start, row.end];
  for deco in decos.iter().filter(|deco| deco.span.overlaps(row)) {
    cuts.push(deco.span.start.clamp(row.start, row.end));
    cuts.push(deco.span.end.clamp(row.start, row.end));
  }
  cuts.sort_unstable();
  cuts.dedup();
  cuts
    .windows(2)
    .map(|pair| {
      let span = Span {
        start: pair[0],
        end: pair[1],
      };
      let marks = decos
        .iter()
        .filter(|deco| deco.span.start <= span.start && deco.span.end >= span.end)
        .map(|deco| deco.mark)
        .collect();
      Piece { span, marks }
    })
    .collect()
}

/// The colour runs over `span`.
pub fn runs_over(runs: &[StyledRun], span: Span) -> impl Iterator<Item = &StyledRun> {
  let first = runs.partition_point(|run| run.span.end <= span.start);
  runs[first..]
    .iter()
    .take_while(move |run| run.span.start < span.end)
}

/// The matches in `line` that overlap `span`, with their index in `matches`.
pub fn matches_over(matches: &[Match], line: u32, span: Span) -> impl Iterator<Item = (usize, &Match)> {
  let first = matches.partition_point(|found| (found.line, found.span.end) <= (line, span.start));
  matches[first..]
    .iter()
    .enumerate()
    .map(move |(offset, found)| (first + offset, found))
    .take_while(move |(_, found)| found.line == line && found.span.start < span.end)
}

fn screen_row(value: f64) -> u32 {
  if value.is_finite() && value > 0.0 {
    // Saturates: a value past `u32::MAX` is past the end anyway.
    value as u32
  } else {
    0
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::text_preview::index_lines;

  mod rows {
    use super::*;

    #[test]
    fn a_filter_by_level_hides_lines_below_it() {
      let (text, lines) = indexed("a\nb\nc");
      let levels = [LogLevel::Info, LogLevel::Error, LogLevel::Debug];
      let shown: Vec<u32> = rows(&text, &lines, Some(&levels), LogLevel::Warn)
        .iter()
        .map(|row| row.line)
        .collect();
      assert_eq!(shown, [1]);
      assert_eq!(rows(&text, &lines, None, LogLevel::Error).len(), 3);
    }

    #[test]
    fn long_lines_are_split_at_character_boundaries() {
      let text = format!("{}\nshort", "é".repeat(MAX_ROW_BYTES));
      let mut lines = Vec::new();
      index_lines(&text, 0, usize::MAX, &mut lines);
      let all = rows(&text, &lines, None, LogLevel::Debug);
      assert_eq!(all.len(), 3);
      assert!(all[0].is_line_start && !all[1].is_line_start && all[2].is_line_start);
      assert_eq!(all[0].span.len(), MAX_ROW_BYTES);
      assert_eq!(all[1].span.len(), MAX_ROW_BYTES);
      assert!(all.iter().all(|row| text.get(row.span.range()).is_some()));
      assert_eq!((all[1].line, all[2].line), (0, 1));
    }

    #[test]
    fn an_empty_line_is_one_row() {
      let (text, lines) = indexed("\n\nx");
      assert_eq!(rows(&text, &lines, None, LogLevel::Debug).len(), 3);
    }

    #[test]
    fn rows_of_lines_and_matches() {
      let (text, lines) = indexed("a\nb\nc\nd");
      let levels = [LogLevel::Error, LogLevel::Debug, LogLevel::Debug, LogLevel::Error];
      let shown = rows(&text, &lines, Some(&levels), LogLevel::Error);
      assert_eq!(row_of_line(&shown, 1), Some(1));
      assert_eq!(row_of_line(&shown, 3), Some(1));
      assert_eq!(row_of_line(&shown, 4), None);
      let found = Match {
        line: 3,
        span: Span::new(6, 7),
      };
      assert_eq!(row_of_match(&shown, &found), Some(1));
      let hidden = Match {
        line: 1,
        span: Span::new(2, 3),
      };
      assert_eq!(row_of_match(&shown, &hidden), None);
    }

    #[test]
    fn columns_count_characters_and_tabs() {
      let (text, lines) = indexed("ab\tü\n");
      assert_eq!(
        columns(&text, &rows(&text, &lines, None, LogLevel::Debug)),
        [3 + TAB_COLUMNS]
      );
    }
  }

  mod heights {
    use super::*;

    #[test]
    fn unwrapped_rows_are_one_screen_row_each() {
      let heights = Heights::new(&[0, 300, 5], None);
      assert_eq!(
        (
          heights.len(),
          heights.total(),
          heights.start_of(2),
          heights.row_at(1)
        ),
        (3, 3, 2, 1)
      );
      assert_eq!(heights.row_at(99), 2);
    }

    #[test]
    fn wrapped_rows_take_what_their_columns_need() {
      let heights = Heights::new(&[0, 10, 11, 25], Some(10));
      assert_eq!(heights, Heights::Wrapped(vec![0, 1, 2, 4, 7]));
      assert_eq!(heights.total(), 7);
      assert_eq!(heights.row_at(0), 0);
      assert_eq!(heights.row_at(3), 2);
      assert_eq!(heights.row_at(6), 3);
      assert_eq!(heights.row_at(70), 3);
    }

    #[test]
    fn no_rows() {
      let heights = Heights::new(&[], Some(80));
      assert!(heights.is_empty());
      let map = ScrollMap::new(&heights, 20.0, 400.0);
      assert_eq!(window(&heights, &map, 20.0, 0.0, 5).rows, 0..0);
    }
  }

  mod scrolling {
    use super::*;

    #[test]
    fn a_short_text_scrolls_one_to_one() {
      let heights = Heights::new(&[1; 100], None);
      let map = ScrollMap::new(&heights, 20.0, 400.0);
      assert_eq!(map.canvas_px, 2000.0);
      assert_eq!(map.content_top(300.0), 300.0);
      assert_eq!(map.scroll_top_for(300.0), 300.0);
      let shown = window(&heights, &map, 20.0, 300.0, 2);
      assert_eq!(shown.rows, 13..38);
      assert_eq!(shown.top_px, 260.0);
    }

    #[test]
    fn a_huge_text_maps_onto_the_canvas_and_reaches_its_end() {
      let heights = Heights::Single(2_000_000);
      let map = ScrollMap::new(&heights, 20.0, 500.0);
      assert_eq!(map.canvas_px, MAX_CANVAS_PX);
      let bottom = map.canvas_px - map.viewport_px;
      let shown = window(&heights, &map, 20.0, bottom, 0);
      assert_eq!(shown.rows.end, 2_000_000);
      // The window sits where the viewport is, whatever the mapping.
      assert_eq!(shown.top_px, bottom);
      assert!((map.scroll_top_for(map.content_top(123_456.0)) - 123_456.0).abs() < 1e-6);
    }

    #[test]
    fn centring_a_row() {
      let heights = Heights::new(&[1; 100], None);
      assert_eq!(centered(&heights, 50, 20.0, 400.0), 1000.0 - 190.0);
      assert_eq!(centered(&heights, 0, 20.0, 400.0), 0.0);
    }
  }

  mod pieces {
    use super::*;

    #[test]
    fn decorations_cut_a_row_into_pieces() {
      let row = Span::new(10, 20);
      let decos = [
        Deco {
          span: Span::new(5, 14),
          mark: Mark::Time,
        },
        Deco {
          span: Span::new(12, 16),
          mark: Mark::Match,
        },
        Deco {
          span: Span::new(30, 40),
          mark: Mark::Level,
        },
      ];
      let cut: Vec<(u32, u32, Vec<Mark>)> = pieces(row, &decos)
        .into_iter()
        .map(|piece| (piece.span.start, piece.span.end, piece.marks))
        .collect();
      assert_eq!(
        cut,
        [
          (10, 12, vec![Mark::Time]),
          (12, 14, vec![Mark::Time, Mark::Match]),
          (14, 16, vec![Mark::Match]),
          (16, 20, vec![]),
        ]
      );
    }

    #[test]
    fn a_row_without_decorations_is_one_piece() {
      assert_eq!(
        pieces(Span::new(0, 3), &[]),
        [Piece {
          span: Span::new(0, 3),
          marks: vec![]
        }]
      );
    }

    #[test]
    fn runs_and_matches_over_a_row() {
      let style = AnsiStyle::default();
      let runs = [
        StyledRun {
          span: Span::new(0, 5),
          style,
        },
        StyledRun {
          span: Span::new(8, 12),
          style,
        },
        StyledRun {
          span: Span::new(20, 30),
          style,
        },
      ];
      assert_eq!(runs_over(&runs, Span::new(4, 10)).count(), 2);
      assert_eq!(runs_over(&runs, Span::new(12, 20)).count(), 0);
      let matches = [
        Match {
          line: 0,
          span: Span::new(1, 2),
        },
        Match {
          line: 2,
          span: Span::new(10, 12),
        },
        Match {
          line: 2,
          span: Span::new(14, 16),
        },
        Match {
          line: 3,
          span: Span::new(20, 21),
        },
      ];
      let over: Vec<usize> = matches_over(&matches, 2, Span::new(9, 15))
        .map(|(index, _)| index)
        .collect();
      assert_eq!(over, [1, 2]);
    }
  }

  fn indexed(text: &str) -> (String, Vec<Span>) {
    let mut lines = Vec::new();
    index_lines(text, 0, usize::MAX, &mut lines);
    (text.to_owned(), lines)
  }
}
