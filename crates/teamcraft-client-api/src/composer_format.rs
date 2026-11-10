//! Formatting for the mobile composers (#1051). The text field of the Android and iOS apps holds the
//! Markdown subset of [`crate::markdown`] as typed, so formatting is editing that text: the toolbar
//! wraps the selection in markers or puts a prefix on its lines, and toggles the same again off.
//! [`style_spans`] says which parts of the source to style as they will render (presentation only;
//! the text never changes by it).
//!
//! All offsets are UTF-16 code units, what `NSRange` and Compose's `TextRange` count, so the apps
//! hand over their selection as it is. An offset inside a surrogate pair snaps to its start.
//!
//! The functions are pure and never panic on odd input. Keystrokes the apps intercept ([`enter`],
//! [`backspace`]) answer `None` when the key should do what it always does.

use crate::markdown::{self, Line, ListKind};

/// Longest text that gets live styling. Above it the field stays plain, so a very long message never
/// makes typing lag.
pub const MAX_STYLED_BYTES: usize = 20_000;

const FENCE: &str = "```";

/// What a formatting button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
  Bold,
  Italic,
  Strike,
  Code,
  CodeBlock,
  Quote,
  Bulleted,
  Numbered,
  Link,
}

/// The text after an edit, with the selection (a caret when both are equal) in UTF-16 units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
  pub text: String,
  pub selection_start: u32,
  pub selection_end: u32,
}

/// How a part of the source is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanKind {
  /// Syntax characters: `**`, a backtick, `> `, `- `, a code fence, the brackets of a link.
  Marker,
  Bold,
  Italic,
  Underline,
  Strike,
  /// Inline code (its content, not the backticks).
  Code,
  /// A line in a fenced code block.
  CodeBlock,
  /// The text of a quote line.
  Quote,
  /// The text of a heading line.
  Heading,
  /// A link's label, or a bare link.
  Link,
  Mention,
}

/// A range of the source, in UTF-16 units, and how to show it. Spans may overlap (bold inside a
/// link); later ones are drawn over earlier ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyleSpan {
  pub start: u32,
  pub end: u32,
  pub kind: SpanKind,
}

/// Applies `format` to the selection `start..end`: toggles it on, or off again when the selection
/// already has it. Inside a code block nothing but the block itself changes.
pub fn apply_format(text: &str, start: u32, end: u32, format: Format) -> Edit {
  let (start, end) = selection(text, start, end);
  let doc = Doc::new(text);
  let result = match format {
    Format::Bold => doc.toggle_inline(start, end, Mark::Bold),
    Format::Italic => doc.toggle_inline(start, end, Mark::Italic),
    Format::Strike => doc.toggle_inline(start, end, Mark::Strike),
    Format::Code => doc.toggle_inline(start, end, Mark::Code),
    Format::Quote => doc.toggle_prefix(start, end, Block::Quote),
    Format::Bulleted => doc.toggle_prefix(start, end, Block::Bullet),
    Format::Numbered => doc.toggle_prefix(start, end, Block::Numbered(1)),
    Format::CodeBlock => doc.toggle_code_block(start, end),
    Format::Link => doc.toggle_link(start, end),
  };
  result.map_or_else(|| unchanged(text, start, end), |raw| raw.into_edit())
}

/// The formats the selection has, for the buttons' pressed state. None for a text over
/// [`MAX_STYLED_BYTES`], like the styling.
pub fn active_formats(text: &str, start: u32, end: u32) -> Vec<Format> {
  if text.len() > MAX_STYLED_BYTES {
    return Vec::new();
  }
  let (start, end) = selection(text, start, end);
  let doc = Doc::new(text);
  let mut active = Vec::new();
  for (format, mark) in [
    (Format::Bold, Mark::Bold),
    (Format::Italic, Mark::Italic),
    (Format::Strike, Mark::Strike),
    (Format::Code, Mark::Code),
  ] {
    if doc.is_inside_mark(start, end, mark) {
      active.push(format);
    }
  }
  if doc.is_inside_link(start, end) {
    active.push(Format::Link);
  }
  let touched = doc.touched_lines(start, end);
  if !touched.is_empty() && touched.iter().all(|&at| doc.lines[at].role != Role::Text) {
    active.push(Format::CodeBlock);
  }
  for (format, block) in [
    (Format::Quote, Block::Quote),
    (Format::Bulleted, Block::Bullet),
    (Format::Numbered, Block::Numbered(1)),
  ] {
    if doc.has_block(&touched, block) {
      active.push(format);
    }
  }
  active
}

/// What Enter at the caret does in a quote, a list or a code block: the next line carries on the
/// block, and Enter on an empty last line leaves it. `None`: a plain new line.
pub fn enter(text: &str, start: u32, end: u32) -> Option<Edit> {
  let (start, end) = selection(text, start, end);
  if start != end {
    return None;
  }
  Doc::new(text).enter(start).map(Raw::into_edit)
}

/// What Backspace at the caret does at the start of a block: it takes the block's marker off (or the
/// empty code block away). `None`: Backspace as always.
pub fn backspace(text: &str, start: u32, end: u32) -> Option<Edit> {
  let (start, end) = selection(text, start, end);
  if start != end {
    return None;
  }
  Doc::new(text).backspace(start).map(Raw::into_edit)
}

/// How to style `text` as it will render. Empty for a text over [`MAX_STYLED_BYTES`].
pub fn style_spans(text: &str) -> Vec<StyleSpan> {
  if text.len() > MAX_STYLED_BYTES {
    return Vec::new();
  }
  let doc = Doc::new(text);
  let units = Units::new(text);
  let mut out = Vec::new();
  let mut push = |start: usize, end: usize, kind: SpanKind| {
    if start < end {
      out.push(StyleSpan {
        start: units.at(start),
        end: units.at(end),
        kind,
      });
    }
  };
  for (at, line) in doc.lines.iter().enumerate() {
    match line.role {
      Role::FenceOpen | Role::FenceClose => push(line.start, line.end, SpanKind::Marker),
      Role::Code => push(line.start, line.end, SpanKind::CodeBlock),
      Role::Text => {
        let info = doc.info(at);
        match info.block {
          Block::Quote => push(line.start + info.prefix, line.end, SpanKind::Quote),
          Block::Heading => push(line.start + info.prefix, line.end, SpanKind::Heading),
          _ => {},
        }
        let mut markers = Vec::new();
        if info.prefix > 0 {
          markers.push((line.start, line.start + info.prefix));
        }
        for span in doc
          .spans
          .iter()
          .filter(|span| span.open >= line.start && span.end <= line.end)
        {
          let kind = match span.style {
            Style::Bold => Some(SpanKind::Bold),
            Style::Italic => Some(SpanKind::Italic),
            Style::Underline => Some(SpanKind::Underline),
            Style::Strike => Some(SpanKind::Strike),
            Style::Code => Some(SpanKind::Code),
            Style::Labeled | Style::Bare => Some(SpanKind::Link),
            Style::Mention => Some(SpanKind::Mention),
            Style::Escape => None,
          };
          if let Some(kind) = kind {
            push(span.content_start, span.content_end, kind);
          }
          if span.content_start > span.open {
            markers.push((span.open, span.content_start));
          }
          if span.end > span.content_end {
            markers.push((span.content_end, span.end));
          }
        }
        for (from, to) in markers {
          push(from, to, SpanKind::Marker);
        }
      },
    }
  }
  out
}

// ---- UTF-16 offsets ----

/// Byte offsets to UTF-16 units, for a text styled in one pass.
struct Units {
  /// UTF-16 unit of every byte offset that starts a char (others are never asked for).
  at_byte: Vec<u32>,
}

impl Units {
  fn new(text: &str) -> Self {
    let mut at_byte = vec![0; text.len() + 1];
    let mut unit = 0_u32;
    for (byte, ch) in text.char_indices() {
      at_byte[byte] = unit;
      unit += ch.len_utf16() as u32;
    }
    at_byte[text.len()] = unit;
    Self { at_byte }
  }

  fn at(&self, byte: usize) -> u32 {
    self.at_byte.get(byte).copied().unwrap_or_default()
  }
}

fn utf16_len(text: &str) -> u32 {
  text.encode_utf16().count() as u32
}

/// The byte offset of UTF-16 unit `unit`, snapped back to a char start; the end when past it.
fn byte_at(text: &str, unit: u32) -> usize {
  let mut seen = 0_u32;
  for (byte, ch) in text.char_indices() {
    let next = seen + ch.len_utf16() as u32;
    if unit < next {
      return byte;
    }
    seen = next;
  }
  text.len()
}

/// The selection as ordered byte offsets.
fn selection(text: &str, start: u32, end: u32) -> (usize, usize) {
  let (start, end) = (byte_at(text, start), byte_at(text, end));
  (start.min(end), start.max(end))
}

/// `at` moved back to the start of the char it is in.
fn floor_boundary(text: &str, at: usize) -> usize {
  let mut at = at.min(text.len());
  while !text.is_char_boundary(at) {
    at -= 1;
  }
  at
}

fn unchanged(text: &str, start: usize, end: usize) -> Edit {
  Raw {
    text: text.to_owned(),
    start,
    end,
  }
  .into_edit()
}

/// An edit with byte offsets.
struct Raw {
  text: String,
  start: usize,
  end: usize,
}

impl Raw {
  fn into_edit(self) -> Edit {
    let selection_start = utf16_len(&self.text[..floor_boundary(&self.text, self.start)]);
    let selection_end = utf16_len(&self.text[..floor_boundary(&self.text, self.end)]);
    Edit {
      text: self.text,
      selection_start,
      selection_end,
    }
  }
}

// ---- Text edits ----

/// Replaces `start..end` with `new`.
struct Op {
  start: usize,
  end: usize,
  new: String,
}

impl Op {
  fn insert(at: usize, new: &str) -> Self {
    Self {
      start: at,
      end: at,
      new: new.to_owned(),
    }
  }

  fn remove(start: usize, end: usize) -> Self {
    Self {
      start,
      end,
      new: String::new(),
    }
  }
}

/// The text with the ops (ascending, not overlapping) applied.
fn apply_ops(text: &str, ops: &[Op]) -> String {
  let mut out = String::with_capacity(text.len() + 8);
  let mut at = 0;
  for op in ops {
    out.push_str(&text[at..op.start]);
    out.push_str(&op.new);
    at = op.end;
  }
  out.push_str(&text[at..]);
  out
}

/// Where `pos` of the old text is in the new one. At an insertion's spot the position goes after the
/// inserted text with `is_after`; inside a replaced range it goes to the end of what replaced it, at
/// most.
fn remap(pos: usize, ops: &[Op], is_after: bool) -> usize {
  let mut delta = 0_isize;
  for op in ops {
    let new_len = op.new.len() as isize;
    let old_len = (op.end - op.start) as isize;
    if op.start == op.end {
      if pos > op.start || (pos == op.start && is_after) {
        delta += new_len;
      } else {
        break;
      }
    } else if pos >= op.end {
      delta += new_len - old_len;
    } else if pos > op.start {
      let inside = (pos - op.start).min(op.new.len());
      return (op.start as isize + delta) as usize + inside;
    } else {
      break;
    }
  }
  (pos as isize + delta).max(0) as usize
}

/// The result of ops with the old selection carried over.
fn carried(text: &str, ops: &[Op], start: usize, end: usize) -> Raw {
  let new_text = apply_ops(text, ops);
  let new_start = remap(start, ops, true).min(new_text.len());
  let new_end = remap(end, ops, false).max(new_start).min(new_text.len());
  Raw {
    text: new_text,
    start: new_start,
    end: new_end,
  }
}

// ---- The source, line by line ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
  Text,
  FenceOpen,
  FenceClose,
  Code,
}

#[derive(Clone, Copy, Debug)]
struct SourceLine {
  start: usize,
  /// Before the newline.
  end: usize,
  role: Role,
}

/// The block kind a text line is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Block {
  Plain,
  Quote,
  Bullet,
  /// The number as written; `Numbered(1)` stands for any number when asking for a kind.
  Numbered(u32),
  Heading,
}

impl Block {
  fn is_same_kind(self, other: Self) -> bool {
    matches!(
      (self, other),
      (Self::Quote, Self::Quote) | (Self::Bullet, Self::Bullet) | (Self::Numbered(_), Self::Numbered(_))
    )
  }
}

struct LineInfo {
  block: Block,
  /// Bytes of the block's marker (`> `, `- `, `12. `, `## `).
  prefix: usize,
}

/// The inline markup that parses as one thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Style {
  Bold,
  Italic,
  Underline,
  Strike,
  Code,
  Labeled,
  Bare,
  Mention,
  /// A backslash and what it escapes.
  Escape,
}

#[derive(Clone, Copy, Debug)]
struct InlineSpan {
  style: Style,
  /// The opening marker starts here.
  open: usize,
  content_start: usize,
  content_end: usize,
  /// After the closing marker.
  end: usize,
}

/// What the toolbar's inline buttons mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
  Bold,
  Italic,
  Strike,
  Code,
}

impl Mark {
  fn style(self) -> Style {
    match self {
      Self::Bold => Style::Bold,
      Self::Italic => Style::Italic,
      Self::Strike => Style::Strike,
      Self::Code => Style::Code,
    }
  }

  /// The marker a new span is written with.
  fn marker(self) -> &'static str {
    match self {
      Self::Bold => "**",
      Self::Italic => "_",
      Self::Strike => "~~",
      Self::Code => "`",
    }
  }
}

struct Doc<'a> {
  text: &'a str,
  lines: Vec<SourceLine>,
  spans: Vec<InlineSpan>,
}

impl<'a> Doc<'a> {
  fn new(text: &'a str) -> Self {
    let mut lines = Vec::new();
    let mut is_in_fence = false;
    let mut start = 0;
    for raw in text.split('\n') {
      let role = if is_in_fence {
        if raw.trim_end() == FENCE {
          is_in_fence = false;
          Role::FenceClose
        } else {
          Role::Code
        }
      } else if raw.trim_start().starts_with(FENCE) {
        is_in_fence = true;
        Role::FenceOpen
      } else {
        Role::Text
      };
      lines.push(SourceLine {
        start,
        end: start + raw.len(),
        role,
      });
      start += raw.len() + 1;
    }
    let mut doc = Self {
      text,
      lines,
      spans: Vec::new(),
    };
    let mut spans = Vec::new();
    for at in 0..doc.lines.len() {
      let line = doc.lines[at];
      if line.role == Role::Text {
        let from = line.start + doc.info(at).prefix;
        scan(&text[from..line.end], from, &mut spans);
      }
    }
    doc.spans = spans;
    doc
  }

  fn line_text(&self, at: usize) -> &'a str {
    &self.text[self.lines[at].start..self.lines[at].end]
  }

  fn info(&self, at: usize) -> LineInfo {
    let line = self.line_text(at);
    let (block, rest) = match markdown::classify(line) {
      Line::Heading(_, rest) => (Block::Heading, rest),
      Line::Quote(rest) => (Block::Quote, rest),
      Line::Item(ListKind::Bulleted, rest) => (Block::Bullet, rest),
      Line::Item(ListKind::Numbered { start }, rest) => (Block::Numbered(start), rest),
      Line::Text(rest) => (Block::Plain, rest),
    };
    LineInfo {
      block,
      prefix: line.len() - rest.len(),
    }
  }

  /// The line that holds the offset (a line's end belongs to it).
  fn line_at(&self, pos: usize) -> usize {
    self
      .lines
      .iter()
      .position(|line| pos <= line.end)
      .unwrap_or(self.lines.len() - 1)
  }

  /// The lines the selection touches: the caret's line, or those with selected text (a selection
  /// ending at a line's start doesn't take that line).
  fn touched_lines(&self, start: usize, end: usize) -> Vec<usize> {
    let first = self.line_at(start);
    let mut last = self.line_at(end);
    if end > start && last > first && end == self.lines[last].start {
      last -= 1;
    }
    (first..=last).collect()
  }

  fn is_code_at(&self, start: usize, end: usize) -> bool {
    self
      .touched_lines(start, end)
      .iter()
      .any(|&at| self.lines[at].role != Role::Text)
  }

  fn has_block(&self, touched: &[usize], block: Block) -> bool {
    let mut candidates = touched.iter().filter(|&&at| {
      self.lines[at].role == Role::Text && (touched.len() == 1 || !self.line_text(at).trim().is_empty())
    });
    candidates.next().is_some() && {
      let mut all = touched.iter().filter(|&&at| {
        self.lines[at].role == Role::Text && (touched.len() == 1 || !self.line_text(at).trim().is_empty())
      });
      all.all(|&at| self.info(at).block.is_same_kind(block))
    }
  }

  // ---- Inline ----

  fn is_inside_mark(&self, start: usize, end: usize, mark: Mark) -> bool {
    self
      .spans
      .iter()
      .any(|span| span.style == mark.style() && span.content_start <= start && end <= span.content_end)
  }

  fn is_inside_link(&self, start: usize, end: usize) -> bool {
    self
      .spans
      .iter()
      .any(|span| span.style == Style::Labeled && span.content_start <= start && end <= span.content_end)
  }

  fn toggle_inline(&self, start: usize, end: usize, mark: Mark) -> Option<Raw> {
    if self.is_code_at(start, end) {
      return None;
    }
    let mut segments = self.segments(start, end);
    if segments.is_empty() {
      return self.toggle_at_caret(start, mark);
    }
    for segment in &mut segments {
      *segment = self.whole_spans(*segment);
    }
    let style = mark.style();
    // Off when every piece already has the mark; otherwise on for all.
    let is_marked = |&(from, to): &(usize, usize)| {
      self
        .spans
        .iter()
        .any(|span| span.style == style && span.open == from && span.end == to)
        || self
          .spans
          .iter()
          .any(|span| span.style == style && span.content_start <= from && to <= span.content_end)
    };
    let is_off = segments.iter().all(is_marked);
    let mut ops = Vec::new();
    for &(from, to) in &segments {
      if is_off {
        self.unmark(from, to, style, &mut ops);
      } else if !is_marked(&(from, to)) {
        self.mark(from, to, mark, &mut ops);
      }
    }
    ops.sort_by_key(|op| (op.start, op.end));
    let first = segments.first().map_or(start, |segment| segment.0);
    let last = segments.last().map_or(end, |segment| segment.1);
    Some(carried(self.text, &ops, first, last))
  }

  /// The selection's pieces of text: one per line, without the white space at their ends.
  fn segments(&self, start: usize, end: usize) -> Vec<(usize, usize)> {
    if start == end {
      return Vec::new();
    }
    let mut out = Vec::new();
    for line in &self.lines {
      let from = line.start.max(start);
      let to = line.end.min(end);
      if from >= to {
        continue;
      }
      let piece = &self.text[from..to];
      let trimmed = piece.trim_start();
      let from = from + (piece.len() - trimmed.len());
      let to = from + trimmed.trim_end().len();
      if from < to {
        out.push((from, to));
      }
    }
    out
  }

  /// The range grown to whole spans: markers are never cut in half, and a span is never closed
  /// outside the range that opened it.
  fn whole_spans(&self, (mut from, mut to): (usize, usize)) -> (usize, usize) {
    loop {
      let mut grown = false;
      for span in &self.spans {
        let overlaps = span.open < to && from < span.end;
        let is_inside_content = span.content_start <= from && to <= span.content_end;
        let is_cut = overlaps && !is_inside_content && !(from <= span.open && span.end <= to);
        if is_cut {
          from = from.min(span.open);
          to = to.max(span.end);
          grown = true;
        }
      }
      if !grown {
        return (from, to);
      }
    }
  }

  /// Wraps `from..to` in the mark; the same mark inside it is dropped, so the new one is the only one.
  fn mark(&self, from: usize, to: usize, mark: Mark, ops: &mut Vec<Op>) {
    for span in &self.spans {
      if span.style == mark.style() && span.open >= from && span.end <= to {
        ops.push(Op::remove(span.open, span.content_start));
        ops.push(Op::remove(span.content_end, span.end));
      }
    }
    let marker = match mark {
      Mark::Italic => self.italic_marker(from, to),
      _ => mark.marker(),
    };
    ops.push(Op::insert(from, marker));
    ops.push(Op::insert(to, marker));
  }

  /// `_` where it parses as emphasis (it opens at a word's start, and `snake_case` isn't italic),
  /// else `*`.
  fn italic_marker(&self, from: usize, to: usize) -> &'static str {
    let before = self.text[..from].chars().next_back();
    let is_word_start = before.is_none_or(|ch| !ch.is_alphanumeric() && ch != '_');
    let inner = &self.text[from..to];
    if is_word_start && !inner.contains('_') {
      "_"
    } else {
      "*"
    }
  }

  /// Takes the mark off `from..to`: a span that is exactly the range loses its markers, one around
  /// the range closes before it and opens again after it.
  fn unmark(&self, from: usize, to: usize, style: Style, ops: &mut Vec<Op>) {
    if let Some(span) = self
      .spans
      .iter()
      .find(|span| span.style == style && span.open == from && span.end == to)
    {
      ops.push(Op::remove(span.open, span.content_start));
      ops.push(Op::remove(span.content_end, span.end));
      return;
    }
    // The innermost span around the range.
    let Some(span) = self
      .spans
      .iter()
      .filter(|span| span.style == style && span.content_start <= from && to <= span.content_end)
      .min_by_key(|span| span.content_end - span.content_start)
    else {
      return;
    };
    let cuts_other_span = self.spans.iter().any(|other| {
      other.open != span.open
        && other.content_start <= span.content_end
        && other.open >= span.content_start
        && [from, to].iter().any(|&cut| other.open < cut && cut < other.end)
    });
    let marker = &self.text[span.open..span.content_start];
    let left = &self.text[span.content_start..from];
    let right = &self.text[to..span.content_end];
    if cuts_other_span {
      ops.push(Op::remove(span.open, span.content_start));
      ops.push(Op::remove(span.content_end, span.end));
      return;
    }
    // The span closes before the range and opens again after it; the range itself stays put.
    ops.push(Op {
      start: span.open,
      end: from,
      new: rewrap(left, marker),
    });
    ops.push(Op {
      start: to,
      end: span.end,
      new: rewrap(right, marker),
    });
  }

  fn toggle_at_caret(&self, caret: usize, mark: Mark) -> Option<Raw> {
    let style = mark.style();
    // An empty pair, from an earlier press: gone again.
    for marker in marker_forms(mark) {
      let run_before = run_before(self.text, caret, marker.chars().next()?);
      let run_after = run_after(self.text, caret, marker.chars().next()?);
      if run_before == marker.len() && run_after == marker.len() {
        let ops = [Op::remove(caret - marker.len(), caret + marker.len())];
        return Some(carried(self.text, &ops, caret, caret));
      }
    }
    // Inside a span: it ends.
    if let Some(span) = self
      .spans
      .iter()
      .filter(|span| span.style == style && span.content_start <= caret && caret <= span.content_end)
      .min_by_key(|span| span.content_end - span.content_start)
    {
      let ops = [
        Op::remove(span.open, span.content_start),
        Op::remove(span.content_end, span.end),
      ];
      return Some(carried(self.text, &ops, caret, caret));
    }
    // Inside a word: the word.
    let before = self.text[..caret].chars().next_back();
    let after = self.text[caret..].chars().next();
    if before.is_some_and(char::is_alphanumeric) && after.is_some_and(char::is_alphanumeric) {
      let from = caret
        - self.text[..caret]
          .chars()
          .rev()
          .take_while(|ch| ch.is_alphanumeric())
          .map(char::len_utf8)
          .sum::<usize>();
      let to = caret
        + self.text[caret..]
          .chars()
          .take_while(|ch| ch.is_alphanumeric())
          .map(char::len_utf8)
          .sum::<usize>();
      let (from, to) = self.whole_spans((from, to));
      let mut ops = Vec::new();
      self.mark(from, to, mark, &mut ops);
      ops.sort_by_key(|op| (op.start, op.end));
      return Some(carried(self.text, &ops, caret, caret));
    }
    // Elsewhere: an empty pair to type into.
    let marker = match mark {
      Mark::Italic => self.italic_marker(caret, caret),
      _ => mark.marker(),
    };
    let pair = format!("{marker}{marker}");
    let text = format!("{}{pair}{}", &self.text[..caret], &self.text[caret..]);
    let at = caret + marker.len();
    Some(Raw {
      text,
      start: at,
      end: at,
    })
  }

  // ---- Link ----

  fn toggle_link(&self, start: usize, end: usize) -> Option<Raw> {
    if self.is_code_at(start, end) {
      return None;
    }
    // In a link: just its label.
    if let Some(span) = self
      .spans
      .iter()
      .find(|span| span.style == Style::Labeled && span.open <= start && end <= span.end)
    {
      let label = &self.text[span.content_start..span.content_end];
      let unescaped = unescape_label(label);
      let text = format!("{}{unescaped}{}", &self.text[..span.open], &self.text[span.end..]);
      return Some(Raw {
        text,
        start: span.open,
        end: span.open + unescaped.len(),
      });
    }
    let selected = &self.text[start..end];
    if selected.contains('\n') {
      return None;
    }
    let is_url = selected.starts_with("https://") || selected.starts_with("http://");
    let (label, url) = if is_url {
      ("", selected)
    } else if selected.is_empty() {
      ("", "https://")
    } else {
      (selected, "https://")
    };
    let escaped = escape_label(label);
    let text = format!("{}[{escaped}]({url}){}", &self.text[..start], &self.text[end..]);
    if is_url || selected.is_empty() {
      // The label comes next.
      let at = start + 1;
      Some(Raw {
        text,
        start: at,
        end: at,
      })
    } else {
      // The address to type over.
      let from = start + 1 + escaped.len() + 2;
      Some(Raw {
        text,
        start: from,
        end: from + url.len(),
      })
    }
  }

  // ---- Blocks ----

  fn toggle_prefix(&self, start: usize, end: usize, block: Block) -> Option<Raw> {
    let touched = self.touched_lines(start, end);
    if touched.iter().any(|&at| self.lines[at].role != Role::Text) {
      return None;
    }
    let is_single = touched.len() == 1;
    let targets: Vec<usize> = touched
      .iter()
      .copied()
      .filter(|&at| is_single || !self.line_text(at).trim().is_empty())
      .collect();
    if targets.is_empty() {
      return None;
    }
    let is_off = targets.iter().all(|&at| self.info(at).block.is_same_kind(block));
    let mut number = match block {
      Block::Numbered(_) => self.numbering_start(targets[0]),
      _ => 1,
    };
    let mut ops = Vec::new();
    for &at in &targets {
      let line = self.lines[at];
      let prefix = self.info(at).prefix;
      let new = if is_off {
        String::new()
      } else {
        match block {
          Block::Quote => "> ".to_owned(),
          Block::Bullet => "- ".to_owned(),
          Block::Numbered(_) => {
            let marker = format!("{number}. ");
            number += 1;
            marker
          },
          Block::Plain | Block::Heading => String::new(),
        }
      };
      ops.push(Op {
        start: line.start,
        end: line.start + prefix,
        new,
      });
    }
    Some(carried(self.text, &ops, start, end))
  }

  /// The number a new list takes: one after the item right above, else 1.
  fn numbering_start(&self, at: usize) -> u32 {
    if at == 0 || self.lines[at - 1].role != Role::Text {
      return 1;
    }
    match self.info(at - 1).block {
      Block::Numbered(number) => number.saturating_add(1),
      _ => 1,
    }
  }

  fn toggle_code_block(&self, start: usize, end: usize) -> Option<Raw> {
    let touched = self.touched_lines(start, end);
    let first = *touched.first()?;
    if self.lines[first].role != Role::Text || touched.iter().any(|&at| self.lines[at].role != Role::Text) {
      // In a block: take its fences away.
      if touched.iter().all(|&at| self.lines[at].role != Role::Text) || self.lines[first].role != Role::Text {
        let (open, close) = self.fence_around(first)?;
        let mut ops = vec![self.remove_line(open)];
        if let Some(close) = close {
          ops.push(self.remove_line(close));
        }
        ops.sort_by_key(|op| (op.start, op.end));
        return Some(carried(self.text, &ops, start, end));
      }
      return None;
    }
    let from = self.lines[first].start;
    let last = *touched.last()?;
    let to = self.lines[last].end;
    let ops = [Op::insert(from, "```\n"), Op::insert(to, "\n```")];
    let is_blank = start == end && self.line_text(first).is_empty();
    if is_blank {
      let at = from + 4;
      return Some(Raw {
        text: apply_ops(self.text, &ops),
        start: at,
        end: at,
      });
    }
    Some(carried(self.text, &ops, start, end))
  }

  /// The opening fence of the block that holds line `at`, and its closing fence if it has one.
  fn fence_around(&self, at: usize) -> Option<(usize, Option<usize>)> {
    let open = (0..=at)
      .rev()
      .find(|&index| self.lines[index].role == Role::FenceOpen)?;
    let close = (at..self.lines.len()).find(|&index| self.lines[index].role == Role::FenceClose);
    if self.lines[at].role == Role::FenceOpen && close.is_none() {
      return Some((at, None));
    }
    Some((open, close))
  }

  /// Takes line `at` out with one of its newlines.
  fn remove_line(&self, at: usize) -> Op {
    let line = self.lines[at];
    if line.end < self.text.len() {
      Op::remove(line.start, line.end + 1)
    } else if line.start > 0 {
      Op::remove(line.start - 1, line.end)
    } else {
      Op::remove(line.start, line.end)
    }
  }

  // ---- Keys ----

  fn enter(&self, caret: usize) -> Option<Raw> {
    let at = self.line_at(caret);
    let line = self.lines[at];
    match line.role {
      Role::Code => {
        // The empty last line of a code block: out of it, to what follows.
        let next = self.lines.get(at + 1)?;
        if line.start != line.end || next.role != Role::FenceClose {
          return None;
        }
        let mut text = apply_ops(self.text, &[Op::remove(line.start, line.end + 1)]);
        let after_fence = next.end - (line.end + 1 - line.start);
        let caret = if after_fence >= text.len() {
          text.push('\n');
          text.len()
        } else {
          after_fence + 1
        };
        Some(Raw {
          text,
          start: caret,
          end: caret,
        })
      },
      Role::FenceOpen | Role::FenceClose => None,
      Role::Text => {
        let info = self.info(at);
        if info.prefix == 0 || caret < line.start + info.prefix {
          return None;
        }
        let next_prefix = match info.block {
          Block::Quote => "> ".to_owned(),
          Block::Bullet => self.text[line.start..line.start + info.prefix].to_owned(),
          Block::Numbered(number) => format!("{}. ", number.saturating_add(1)),
          Block::Plain | Block::Heading => return None,
        };
        let is_empty = line.end == line.start + info.prefix || self.line_text(at) == ">";
        if is_empty && caret == line.end {
          let ops = [Op::remove(line.start, line.end)];
          return Some(Raw {
            text: apply_ops(self.text, &ops),
            start: line.start,
            end: line.start,
          });
        }
        let text = format!("{}\n{next_prefix}{}", &self.text[..caret], &self.text[caret..]);
        let at = caret + 1 + next_prefix.len();
        Some(Raw {
          text,
          start: at,
          end: at,
        })
      },
    }
  }

  fn backspace(&self, caret: usize) -> Option<Raw> {
    let at = self.line_at(caret);
    let line = self.lines[at];
    match line.role {
      Role::Text => {
        let info = self.info(at);
        if info.prefix == 0 || caret != line.start + info.prefix {
          return None;
        }
        let ops = [Op::remove(line.start, line.start + info.prefix)];
        Some(Raw {
          text: apply_ops(self.text, &ops),
          start: line.start,
          end: line.start,
        })
      },
      Role::Code => {
        // The start of an empty code block's only line: the block goes.
        let before = at.checked_sub(1).map(|index| self.lines[index])?;
        let after = self.lines.get(at + 1)?;
        if caret != line.start
          || line.start != line.end
          || before.role != Role::FenceOpen
          || after.role != Role::FenceClose
        {
          return None;
        }
        let ops = [Op::remove(before.start, after.end)];
        Some(Raw {
          text: apply_ops(self.text, &ops),
          start: before.start,
          end: before.start,
        })
      },
      Role::FenceOpen | Role::FenceClose => None,
    }
  }
}

/// Wraps `inner` in the marker, with its leading and trailing white space left outside (`** a**`
/// isn't bold). Nothing of it is kept for white space only.
fn rewrap(inner: &str, marker: &str) -> String {
  let trimmed = inner.trim();
  if trimmed.is_empty() {
    return inner.to_owned();
  }
  let lead = &inner[..inner.len() - inner.trim_start().len()];
  let trail = &inner[inner.trim_end().len()..];
  format!("{lead}{marker}{trimmed}{marker}{trail}")
}

/// The ways a mark is written, for an empty pair.
fn marker_forms(mark: Mark) -> &'static [&'static str] {
  match mark {
    Mark::Bold => &["**"],
    Mark::Italic => &["_", "*"],
    Mark::Strike => &["~~"],
    Mark::Code => &["`"],
  }
}

/// How many bytes of `marker_char` end right before `at`.
fn run_before(text: &str, at: usize, marker_char: char) -> usize {
  text[..at]
    .chars()
    .rev()
    .take_while(|&ch| ch == marker_char)
    .map(char::len_utf8)
    .sum()
}

/// How many bytes of `marker_char` start at `at`.
fn run_after(text: &str, at: usize, marker_char: char) -> usize {
  text[at..]
    .chars()
    .take_while(|&ch| ch == marker_char)
    .map(char::len_utf8)
    .sum()
}

/// A link label as `[label](url)` reads it back: `\` and `]` need a backslash.
fn escape_label(label: &str) -> String {
  let mut out = String::with_capacity(label.len());
  for ch in label.chars() {
    if ch == '\\' || ch == ']' {
      out.push('\\');
    }
    out.push(ch);
  }
  out
}

fn unescape_label(label: &str) -> String {
  let mut out = String::with_capacity(label.len());
  let mut chars = label.chars().peekable();
  while let Some(ch) = chars.next() {
    match chars.peek() {
      Some(next) if ch == '\\' && next.is_ascii_punctuation() => {
        out.push(*next);
        chars.next();
      },
      _ => out.push(ch),
    }
  }
  out
}

// ---- Inline scan (the rules of `markdown::parse_inline`, with positions) ----

/// Finds the inline markup of `src` (which starts at `base` in the whole text), nested spans too.
fn scan(src: &str, base: usize, out: &mut Vec<InlineSpan>) {
  let mut at = 0;
  let mut maybe_prev: Option<char> = None;
  while let Some(ch) = src[at..].chars().next() {
    let rest = &src[at..];
    let is_boundary = maybe_prev.is_none_or(|prev| !prev.is_alphanumeric());
    let is_word_start = is_boundary && maybe_prev != Some('_');
    // (style, marker length, bytes used, whether its content holds more markup)
    let found: Option<(Style, usize, usize, bool)> = match ch {
      '`' => markdown::delimited(rest, "`").map(|(_, used)| (Style::Code, 1, used, false)),
      '*' if rest.starts_with("**") => {
        markdown::delimited(rest, "**").map(|(_, used)| (Style::Bold, 2, used, true))
      },
      '~' if rest.starts_with("~~") => {
        markdown::delimited(rest, "~~").map(|(_, used)| (Style::Strike, 2, used, true))
      },
      '_' if is_word_start && rest.starts_with("__") => {
        markdown::delimited(rest, "__").map(|(_, used)| (Style::Underline, 2, used, true))
      },
      '_' if is_word_start => markdown::delimited(rest, "_").map(|(_, used)| (Style::Italic, 1, used, true)),
      '*' => markdown::delimited(rest, "*").map(|(_, used)| (Style::Italic, 1, used, true)),
      _ => None,
    };
    if let Some((style, marker, used, has_markup)) = found {
      let span = InlineSpan {
        style,
        open: base + at,
        content_start: base + at + marker,
        content_end: base + at + used - marker,
        end: base + at + used,
      };
      out.push(span);
      if has_markup {
        scan(&src[at + marker..at + used - marker], base + at + marker, out);
      }
      at += used;
      maybe_prev = None;
      continue;
    }
    let other: Option<(Style, usize, usize, usize)> = match ch {
      '[' => markdown::labeled_link(rest).and_then(|(_, used)| {
        let label_end = 1 + markdown::find_unescaped(&rest[1..], "]")?;
        Some((Style::Labeled, 1, label_end, used))
      }),
      '@' if is_boundary => markdown::mention(rest).map(|(_, used)| (Style::Mention, 0, used, used)),
      'h' | 't' if is_boundary => markdown::link(rest).map(|(_, used)| (Style::Bare, 0, used, used)),
      '\\' => rest[1..]
        .chars()
        .next()
        .filter(char::is_ascii_punctuation)
        .map(|escaped| (Style::Escape, 1, 1 + escaped.len_utf8(), 1 + escaped.len_utf8())),
      _ => None,
    };
    if let Some((style, content_from, content_to, used)) = other {
      out.push(InlineSpan {
        style,
        open: base + at,
        content_start: base + at + content_from,
        content_end: base + at + content_to,
        end: base + at + used,
      });
      maybe_prev = match style {
        Style::Escape => rest[1..].chars().next(),
        _ => None,
      };
      at += used;
      continue;
    }
    maybe_prev = Some(ch);
    at += ch.len_utf8();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Applies `format` to the text where `[` and `]` mark the selection (or `|` the caret), and
  /// writes the result the same way.
  fn run(marked: &str, format: Format) -> String {
    let (text, start, end) = parse_marked(marked);
    let edit = apply_format(&text, start, end, format);
    write_marked(&edit.text, edit.selection_start, edit.selection_end)
  }

  fn parse_marked(marked: &str) -> (String, u32, u32) {
    let mut text = String::new();
    let (mut start, mut end) = (None, None);
    let mut units = 0_u32;
    // `⟨⟩` stand for the selection where the text has brackets of its own.
    let (open, close) = if marked.contains('⟨') {
      ('⟨', '⟩')
    } else {
      ('[', ']')
    };
    for ch in marked.chars() {
      match ch {
        '|' => {
          start = Some(units);
          end = Some(units);
        },
        ch if ch == open => start = Some(units),
        ch if ch == close => end = Some(units),
        _ => {
          text.push(ch);
          units += ch.len_utf16() as u32;
        },
      }
    }
    (text, start.unwrap_or(0), end.unwrap_or(start.unwrap_or(0)))
  }

  fn write_marked(text: &str, start: u32, end: u32) -> String {
    let (open, close) = if text.contains('[') {
      ('⟨', '⟩')
    } else {
      ('[', ']')
    };
    let mut out = String::new();
    let mut units = 0_u32;
    for ch in text.chars() {
      if start == end && units == start {
        out.push('|');
      } else {
        if units == start {
          out.push(open);
        }
        if units == end {
          out.push(close);
        }
      }
      out.push(ch);
      units += ch.len_utf16() as u32;
    }
    if start == end && units == start {
      out.push('|');
    } else {
      if units == start {
        out.push(open);
      }
      if units == end {
        out.push(close);
      }
    }
    out
  }

  fn enter_at(marked: &str) -> Option<String> {
    let (text, start, end) = parse_marked(marked);
    enter(&text, start, end).map(|edit| write_marked(&edit.text, edit.selection_start, edit.selection_end))
  }

  fn backspace_at(marked: &str) -> Option<String> {
    let (text, start, end) = parse_marked(marked);
    backspace(&text, start, end)
      .map(|edit| write_marked(&edit.text, edit.selection_start, edit.selection_end))
  }

  fn active(marked: &str) -> Vec<Format> {
    let (text, start, end) = parse_marked(marked);
    active_formats(&text, start, end)
  }

  #[test]
  fn bold_wraps_the_selection_and_toggles_off() {
    assert_eq!(run("a [word] b", Format::Bold), "a **[word]** b");
    assert_eq!(run("a **[word]** b", Format::Bold), "a [word] b");
  }

  #[test]
  fn toggling_off_works_with_the_markers_selected_too() {
    assert_eq!(run("a [**word**] b", Format::Bold), "a [word] b");
  }

  #[test]
  fn white_space_stays_outside_the_markers() {
    assert_eq!(run("a[ word ]b", Format::Bold), "a **[word]** b");
  }

  #[test]
  fn each_line_of_a_selection_is_marked_alone() {
    assert_eq!(run("[one\ntwo]", Format::Strike), "~~[one~~\n~~two]~~");
    assert_eq!(run("~~[one~~\n~~two]~~", Format::Strike), "[one\ntwo]");
  }

  #[test]
  fn italic_picks_the_marker_that_reads_back() {
    assert_eq!(run("a [word]", Format::Italic), "a _[word]_");
    assert_eq!(run("un[believ]able", Format::Italic), "un*[believ]*able");
    assert_eq!(run("a [snake_case]", Format::Italic), "a *[snake_case]*");
    assert_eq!(run("a *[word]*", Format::Italic), "a [word]");
    assert_eq!(run("a _[word]_", Format::Italic), "a [word]");
  }

  #[test]
  fn code_wraps_and_unwraps() {
    assert_eq!(run("run [ls -l] now", Format::Code), "run `[ls -l]` now");
    assert_eq!(run("run `[ls -l]` now", Format::Code), "run [ls -l] now");
  }

  #[test]
  fn part_of_a_marked_span_is_split_out() {
    assert_eq!(
      run("**one [two] three**", Format::Bold),
      "**one** [two] **three**"
    );
    assert_eq!(run("**[one] two**", Format::Bold), "[one] **two**");
    assert_eq!(run("**one [two]**", Format::Bold), "**one** [two]");
  }

  #[test]
  fn marking_across_a_span_takes_the_span_whole() {
    assert_eq!(run("[a **b] c**", Format::Italic), "_[a **b c**]_");
    assert_eq!(run("a [**b** c]", Format::Bold), "a **[b c]**");
  }

  #[test]
  fn other_marks_nest() {
    assert_eq!(run("**[word]**", Format::Italic), "**_[word]_**");
    assert_eq!(run("**_[word]_**", Format::Bold), "_[word]_");
  }

  #[test]
  fn the_caret_in_a_word_marks_the_word_and_unmarks_it() {
    assert_eq!(run("he|llo world", Format::Bold), "**he|llo** world");
    assert_eq!(run("**he|llo** world", Format::Bold), "he|llo world");
  }

  #[test]
  fn the_caret_elsewhere_opens_an_empty_pair_and_closes_it_again() {
    assert_eq!(run("hello |", Format::Bold), "hello **|**");
    assert_eq!(run("hello **|**", Format::Bold), "hello |");
    assert_eq!(run("|", Format::Code), "`|`");
    assert_eq!(run("hello|", Format::Italic), "hello*|*");
  }

  #[test]
  fn nothing_is_marked_inside_a_code_block() {
    assert_eq!(run("```\n[word]\n```", Format::Bold), "```\n[word]\n```");
  }

  #[test]
  fn quote_prefixes_the_lines_and_toggles_off() {
    assert_eq!(run("[one\ntwo]", Format::Quote), "> [one\n> two]");
    assert_eq!(run("> [one\n> two]", Format::Quote), "[one\ntwo]");
    assert_eq!(run("one|", Format::Quote), "> one|");
    assert_eq!(run("|", Format::Quote), "> |");
    assert_eq!(run("> |", Format::Quote), "|");
  }

  #[test]
  fn lists_replace_another_block_and_number_the_lines() {
    assert_eq!(run("[one\ntwo]", Format::Bulleted), "- [one\n- two]");
    assert_eq!(run("[one\ntwo]", Format::Numbered), "1. [one\n2. two]");
    assert_eq!(run("- [one\n- two]", Format::Numbered), "1. [one\n2. two]");
    assert_eq!(run("1. [one\n2. two]", Format::Numbered), "[one\ntwo]");
    assert_eq!(run("> [one]", Format::Bulleted), "- [one]");
    assert_eq!(run("# [Title]", Format::Quote), "> [Title]");
  }

  #[test]
  fn a_list_goes_on_where_the_one_above_ends() {
    assert_eq!(run("1. a\n2. b\n[c]", Format::Numbered), "1. a\n2. b\n3. [c]");
  }

  #[test]
  fn blank_lines_between_selected_lines_stay_blank() {
    assert_eq!(run("[one\n\ntwo]", Format::Bulleted), "- [one\n\n- two]");
  }

  #[test]
  fn a_selection_ending_at_a_line_start_leaves_that_line_alone() {
    assert_eq!(run("[one\n]two", Format::Quote), "> [one\n]two");
  }

  #[test]
  fn code_block_fences_the_lines_and_toggles_off() {
    assert_eq!(run("[a\nb]", Format::CodeBlock), "```\n[a\nb]\n```");
    assert_eq!(run("```\n[a\nb]\n```", Format::CodeBlock), "[a\nb]");
    assert_eq!(run("x\n```rust\na|\n```\ny", Format::CodeBlock), "x\na|\ny");
    assert_eq!(run("|", Format::CodeBlock), "```\n|\n```");
    assert_eq!(run("```\n|\n```", Format::CodeBlock), "|");
  }

  #[test]
  fn an_unclosed_fence_toggles_off_too() {
    assert_eq!(run("```\na|", Format::CodeBlock), "a|");
  }

  #[test]
  fn a_link_wraps_the_selection_and_leaves_the_address_to_type() {
    assert_eq!(run("see ⟨docs⟩ now", Format::Link), "see [docs](⟨https://⟩) now");
    assert_eq!(run("see | now", Format::Link), "see [|](https://) now");
    assert_eq!(
      run("see ⟨https://example.com⟩ now", Format::Link),
      "see [|](https://example.com) now"
    );
  }

  #[test]
  fn a_link_toggles_back_to_its_label() {
    assert_eq!(
      run("see [docs](https://example.com⟨⟩) now", Format::Link),
      "see [docs] now"
    );
    assert_eq!(
      run("see [do⟨⟩cs](https://example.com) now", Format::Link),
      "see [docs] now"
    );
  }

  #[test]
  fn a_label_with_a_bracket_is_escaped() {
    let (text, start, end) = parse_marked("⟨a]b⟩");
    let edit = apply_format(&text, start, end, Format::Link);
    assert_eq!(edit.text, "[a\\]b](https://)");
    let typed = edit.text.replace("https://", "https://example.com");
    assert!(
      matches!(markdown::parse_inline(&typed).as_slice(), [markdown::Inline::LabeledLink { label, .. }] if label == "a]b")
    );
  }

  #[test]
  fn offsets_count_utf16_units() {
    // The emoji takes two units, `ä` one.
    assert_eq!(run("😀 [ä]", Format::Bold), "😀 **[ä]**");
    let edit = apply_format("😀 ä", 3, 4, Format::Bold);
    assert_eq!(edit.text, "😀 **ä**");
    assert_eq!((edit.selection_start, edit.selection_end), (5, 6));
  }

  #[test]
  fn an_offset_inside_a_surrogate_pair_snaps_to_its_start() {
    let edit = apply_format("😀", 1, 1, Format::Bold);
    assert_eq!(edit.text, "****😀");
  }

  #[test]
  fn offsets_past_the_end_are_clamped() {
    let edit = apply_format("ab", 9, 40, Format::Bold);
    assert_eq!(edit.text, "ab****");
    let edit = apply_format("", 0, 0, Format::Quote);
    assert_eq!(edit.text, "> ");
  }

  #[test]
  fn the_result_reads_back_as_the_format() {
    for (marked, format) in [
      ("[word]", Format::Bold),
      ("[word]", Format::Strike),
      ("[word]", Format::Italic),
      ("[a b]", Format::Code),
    ] {
      let (text, start, end) = parse_marked(marked);
      let edit = apply_format(&text, start, end, format);
      let parsed = markdown::parse_inline(&edit.text);
      assert!(
        !matches!(parsed.as_slice(), [markdown::Inline::Text(_)]),
        "{edit:?} reads back as plain text"
      );
    }
  }

  #[test]
  fn enter_continues_a_list_and_a_quote() {
    assert_eq!(enter_at("- one|").as_deref(), Some("- one\n- |"));
    assert_eq!(enter_at("1. one|").as_deref(), Some("1. one\n2. |"));
    assert_eq!(enter_at("> one|").as_deref(), Some("> one\n> |"));
    assert_eq!(enter_at("- on|e").as_deref(), Some("- on\n- |e"));
  }

  #[test]
  fn enter_on_an_empty_item_leaves_the_block() {
    assert_eq!(enter_at("- one\n- |").as_deref(), Some("- one\n|"));
    assert_eq!(enter_at("> |").as_deref(), Some("|"));
    assert_eq!(enter_at(">|").as_deref(), Some("|"));
    assert_eq!(enter_at("1. |").as_deref(), Some("|"));
  }

  #[test]
  fn enter_elsewhere_is_a_plain_new_line() {
    assert_eq!(enter_at("plain|"), None);
    assert_eq!(enter_at("-| one"), None);
    assert_eq!(enter_at("# head|"), None);
    assert_eq!(enter_at("[a]b"), None);
    assert_eq!(enter_at("```\ncode|\n```"), None);
    assert_eq!(enter_at("|```"), None);
  }

  #[test]
  fn enter_on_the_empty_last_line_of_a_code_block_leaves_it() {
    assert_eq!(
      enter_at("```\ncode\n|\n```").as_deref(),
      Some("```\ncode\n```\n|")
    );
    assert_eq!(
      enter_at("```\ncode\n|\n```\nafter").as_deref(),
      Some("```\ncode\n```\n|after")
    );
    assert_eq!(enter_at("```\n|\n```").as_deref(), Some("```\n```\n|"));
  }

  #[test]
  fn backspace_after_a_marker_takes_it_off() {
    assert_eq!(backspace_at("- |").as_deref(), Some("|"));
    assert_eq!(backspace_at("one\n> |two").as_deref(), Some("one\n|two"));
    assert_eq!(backspace_at("12. |x").as_deref(), Some("|x"));
    assert_eq!(backspace_at(">|").as_deref(), Some("|"));
  }

  #[test]
  fn backspace_elsewhere_is_as_always() {
    assert_eq!(backspace_at("- on|e"), None);
    assert_eq!(backspace_at("|- one"), None);
    assert_eq!(backspace_at("plain|"), None);
    assert_eq!(backspace_at("[- ]one"), None);
  }

  #[test]
  fn backspace_at_the_start_of_an_empty_code_block_removes_it() {
    assert_eq!(backspace_at("a\n```\n|\n```").as_deref(), Some("a\n|"));
    assert_eq!(backspace_at("```\n|\n```").as_deref(), Some("|"));
    assert_eq!(backspace_at("```\ncode\n|\n```"), None);
  }

  #[test]
  fn active_formats_follow_the_selection() {
    assert_eq!(active("**wo|rd**"), [Format::Bold]);
    assert_eq!(active("plain|"), Vec::<Format>::new());
    assert_eq!(active("> **x|**"), [Format::Bold, Format::Quote]);
    assert_eq!(active("- a|"), [Format::Bulleted]);
    assert_eq!(active("1. a|"), [Format::Numbered]);
    assert_eq!(active("```\nco|de\n```"), [Format::CodeBlock]);
    assert_eq!(active("`c|`"), [Format::Code]);
    assert_eq!(active("[x](https://a.b⟨⟩)"), Vec::<Format>::new());
    assert_eq!(active("[x⟨⟩](https://a.b)"), [Format::Link]);
    assert_eq!(active("~~a|~~ _b_"), [Format::Strike]);
    assert_eq!(active("- [a\n- b]"), [Format::Bulleted]);
    assert_eq!(active("- [a\nb]"), Vec::<Format>::new());
  }

  fn kinds(text: &str) -> Vec<(String, SpanKind)> {
    style_spans(text)
      .into_iter()
      .map(|span| {
        let units: Vec<u16> = text.encode_utf16().collect();
        (
          String::from_utf16_lossy(&units[span.start as usize..span.end as usize]),
          span.kind,
        )
      })
      .collect()
  }

  #[test]
  fn spans_style_the_content_and_dim_the_markers() {
    let spans = kinds("a **b** c");
    assert_eq!(
      spans,
      [
        ("b".to_owned(), SpanKind::Bold),
        ("**".to_owned(), SpanKind::Marker),
        ("**".to_owned(), SpanKind::Marker),
      ]
    );
  }

  #[test]
  fn spans_nest_and_cover_blocks() {
    let spans = kinds("> **a _b_**");
    assert!(spans.contains(&("**a _b_**".to_owned(), SpanKind::Quote)));
    assert!(spans.contains(&("a _b_".to_owned(), SpanKind::Bold)));
    assert!(spans.contains(&("b".to_owned(), SpanKind::Italic)));
    assert!(spans.contains(&("> ".to_owned(), SpanKind::Marker)));
  }

  #[test]
  fn spans_for_code_links_mentions_and_fences() {
    let spans = kinds("`x` [t](https://a.b) @bob https://a.b/c\n```rs\nlet a = 1;\n```");
    assert!(spans.contains(&("x".to_owned(), SpanKind::Code)));
    assert!(spans.contains(&("t".to_owned(), SpanKind::Link)));
    assert!(spans.contains(&("](https://a.b)".to_owned(), SpanKind::Marker)));
    assert!(spans.contains(&("@bob".to_owned(), SpanKind::Mention)));
    assert!(spans.contains(&("https://a.b/c".to_owned(), SpanKind::Link)));
    assert!(spans.contains(&("```rs".to_owned(), SpanKind::Marker)));
    assert!(spans.contains(&("let a = 1;".to_owned(), SpanKind::CodeBlock)));
  }

  #[test]
  fn markup_inside_a_code_block_is_not_styled() {
    let spans = kinds("```\n**x**\n```");
    assert!(!spans.iter().any(|(_, kind)| *kind == SpanKind::Bold));
  }

  #[test]
  fn text_that_is_not_markup_has_no_spans() {
    assert!(style_spans("snake_case and 2 * 3 and ** ** and a_b_c").is_empty());
    assert!(style_spans("").is_empty());
  }

  #[test]
  fn span_offsets_are_utf16_units() {
    let spans = style_spans("😀 **ä**");
    let bold = spans.iter().find(|span| span.kind == SpanKind::Bold);
    assert_eq!(bold.map(|span| (span.start, span.end)), Some((5, 6)));
  }

  #[test]
  fn a_very_long_text_is_not_styled() {
    let text = format!("**a** {}", "x".repeat(MAX_STYLED_BYTES));
    assert!(style_spans(&text).is_empty());
  }

  #[test]
  fn the_scan_agrees_with_the_parser_on_what_is_emphasis() {
    for source in [
      "**b** and _i_ and ~~s~~ and `c` and __u__",
      "un*believ*able snake_case _x_",
      "** no ** __ no __ ~~ no ~~",
      "\\*not\\* and **\\*yes\\***",
      "see [l](https://a.b) and https://c.d, @eve.",
    ] {
      let parsed = markdown::parse_inline(source);
      let styled = flat_count(&parsed);
      let scanned = style_spans(source)
        .iter()
        .filter(|span| !matches!(span.kind, SpanKind::Marker))
        .count();
      assert_eq!(scanned, styled, "{source}");
    }
  }

  fn flat_count(inlines: &[markdown::Inline]) -> usize {
    use markdown::Inline;
    inlines
      .iter()
      .map(|inline| match inline {
        Inline::Text(_) => 0,
        Inline::Bold(inner) | Inline::Italic(inner) | Inline::Underline(inner) | Inline::Strike(inner) => {
          1 + flat_count(inner)
        },
        Inline::Code(_) | Inline::Link(_) | Inline::LabeledLink { .. } | Inline::Mention(_) => 1,
      })
      .sum()
  }

  #[test]
  fn arbitrary_selections_never_panic() {
    let samples = [
      "",
      "a",
      "**",
      "```",
      "```\n",
      "> \n- \n1. ",
      "😀**😀**\n> 😀",
      "[a](b",
      "\\",
      "a\n\n\nb",
    ];
    let formats = [
      Format::Bold,
      Format::Italic,
      Format::Strike,
      Format::Code,
      Format::CodeBlock,
      Format::Quote,
      Format::Bulleted,
      Format::Numbered,
      Format::Link,
    ];
    for text in samples {
      let units = utf16_len(text);
      for start in 0..=units + 1 {
        for end in start..=units + 1 {
          for format in formats {
            let edit = apply_format(text, start, end, format);
            assert!(edit.selection_start <= edit.selection_end);
            assert!(edit.selection_end <= utf16_len(&edit.text));
          }
          let _ = active_formats(text, start, end);
          let _ = enter(text, start, end);
          let _ = backspace(text, start, end);
        }
      }
      let _ = style_spans(text);
    }
  }
}
